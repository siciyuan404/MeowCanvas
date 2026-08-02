// RunningHub 后端处理器。
// 对接 RunningHub AI 工作流平台,覆盖:
//   - app / workflow 信息查询
//   - 本地工作流缓存 CRUD
//   - 素材上传 / 任务提交 / 任务查询
// 响应统一为 {success, data?, detail?} 形式,前端通过 success 判断成败。
// 网络错误 / 上游错误也返回 HTTP 200 + success:false,保证前端按统一字段解析。
use crate::http_util::{ok, not_configured, not_found, AppState};
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::Json;
use reqwest::Client;
use serde_json::{json, Value};
use std::collections::HashMap;

/// RunningHub 平台在 providers 列表中的固定 id
const PROVIDER_ID: &str = "runninghub";
/// 默认上游地址,provider 未配置 base_url 时使用
const DEFAULT_BASE_URL: &str = "https://api.runninghub.cn";

/// 从 store 取出 runninghub provider,返回 (api_key, base_url)
/// 若未配置则返回 None,调用方应返回 not_configured("RunningHub")
fn resolve_provider(state: &AppState) -> Option<(String, String)> {
    let s = state.0.lock().unwrap();
    let p = s.get_provider(PROVIDER_ID)?;
    let key = p.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let url = p.get("base_url").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_BASE_URL).to_string();
    Some((key, url))
}

/// 根据 useWallet 选择 api_key:钱包模式下优先用 wallet_api_key,空则回退到 api_key
fn pick_api_key(state: &AppState, use_wallet: bool) -> Option<String> {
    let s = state.0.lock().unwrap();
    let p = s.get_provider(PROVIDER_ID)?;
    let normal = p.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if use_wallet {
        let wallet = p.get("wallet_api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
        Some(if wallet.is_empty() { normal } else { wallet })
    } else {
        Some(normal)
    }
}

/// 从上游响应中提取 nodeInfoList,容错多种字段名
fn extract_node_list(raw: &Value) -> Value {
    // 优先直接取 nodeInfoList,其次尝试 data.nodeInfoList
    if let Some(list) = raw.get("nodeInfoList") {
        return list.clone();
    }
    if let Some(list) = raw.get("data").and_then(|d| d.get("nodeInfoList")) {
        return list.clone();
    }
    json!([])
}

/// 从上游 query 响应中提取 outputs 数组(图片 / 文件列表)
fn extract_outputs(raw: &Value) -> Vec<Value> {
    raw.get("data").and_then(|d| d.get("outputs"))
        .and_then(|o| o.as_array())
        .cloned()
        .or_else(|| raw.get("outputs").and_then(|o| o.as_array()).cloned())
        .unwrap_or_default()
}

/// 从上游错误响应中提取人类可读的 detail 文案
fn upstream_detail(text: &str) -> String {
    let parsed: Value = serde_json::from_str(text).unwrap_or(json!({}));
    parsed.get("msg").or_else(|| parsed.get("message"))
        .or_else(|| parsed.get("detail")).or_else(|| parsed.get("error"))
        .map(|v| {
            if let Some(s) = v.as_str() { s.to_string() } else { v.to_string() }
        })
        .unwrap_or_else(|| text.to_string())
}

// ====================================================================
// 1. GET /api/runninghub/app-info?webappId=xxx
// 查询 app 模式的节点信息
// ====================================================================
pub async fn app_info(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (api_key, base_url) = match resolve_provider(&state) {
        Some(v) => v,
        None => return not_configured("RunningHub"),
    };
    let webapp_id = match q.get("webappId") {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return ok(json!({ "success": false, "detail": "webappId 不能为空" })),
    };

    let url = format!("{}/task/openapi/getAppInfo", base_url.trim_end_matches('/'));
    let body = json!({ "apiKey": api_key, "webappId": webapp_id });

    let client = Client::new();
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => return ok(json!({
            "success": false,
            "detail": format!("调用 RunningHub getAppInfo 失败: {e}")
        })),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return ok(json!({
            "success": false,
            "detail": format!("RunningHub getAppInfo 返回 {}: {}", status.as_u16(), upstream_detail(&text))
        }));
    }
    let raw: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    // 上游可能返回 code/data 或直接平铺字段,统一包装
    let upstream_ok = raw.get("code").and_then(|c| c.as_i64()).map(|c| c == 200)
        .unwrap_or(true);
    if !upstream_ok {
        let detail = upstream_detail(&text);
        return ok(json!({ "success": false, "detail": detail, "raw": raw }));
    }
    let node_info_list = extract_node_list(&raw);
    ok(json!({
        "success": true,
        "data": { "nodeInfoList": node_info_list, "raw": raw }
    }))
}

// ====================================================================
// 2. GET /api/runninghub/workflow-info?workflowId=xxx
// 查询 workflow 模式的节点信息
// ====================================================================
pub async fn workflow_info(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (api_key, base_url) = match resolve_provider(&state) {
        Some(v) => v,
        None => return not_configured("RunningHub"),
    };
    let workflow_id = match q.get("workflowId") {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return ok(json!({ "success": false, "detail": "workflowId 不能为空" })),
    };

    let url = format!(
        "{}/task/openapi/getWorkflowInfo?workflowId={}&apiKey={}",
        base_url.trim_end_matches('/'),
        urlencoding(&workflow_id),
        urlencoding(&api_key)
    );

    let client = Client::new();
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => return ok(json!({
            "success": false,
            "detail": format!("调用 RunningHub getWorkflowInfo 失败: {e}")
        })),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return ok(json!({
            "success": false,
            "detail": format!("RunningHub getWorkflowInfo 返回 {}: {}", status.as_u16(), upstream_detail(&text))
        }));
    }
    let raw: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    let upstream_ok = raw.get("code").and_then(|c| c.as_i64()).map(|c| c == 200)
        .unwrap_or(true);
    if !upstream_ok {
        let detail = upstream_detail(&text);
        return ok(json!({ "success": false, "detail": detail, "raw": raw }));
    }
    let node_info_list = extract_node_list(&raw);
    ok(json!({
        "success": true,
        "data": { "nodeInfoList": node_info_list, "raw": raw }
    }))
}

// ====================================================================
// 3. GET /api/runninghub/workflows/:workflowId
// 取本地缓存的工作流配置。找不到返回 404,前端按 res.ok 处理
// ====================================================================
pub async fn get_workflow(
    State(state): State<AppState>,
    Path(workflow_id): Path<String>,
) -> Response {
    let wf = {
        let s = state.0.lock().unwrap();
        s.get_rh_workflow(&workflow_id)
    };
    match wf {
        Some(workflow) => ok(json!({ "workflow": workflow })),
        None => not_found("not found"),
    }
}

// ====================================================================
// 4. POST /api/runninghub/workflows/fetch
// 拉取上游 workflow 信息并入库
// 请求体: {workflowId, title, description}
// ====================================================================
pub async fn fetch_workflow(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let (api_key, base_url) = match resolve_provider(&state) {
        Some(v) => v,
        None => return not_configured("RunningHub"),
    };
    let workflow_id = body.get("workflowId").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let description = body.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if workflow_id.is_empty() {
        return ok(json!({ "success": false, "detail": "workflowId 不能为空" }));
    }

    // 拉取上游节点信息
    let url = format!(
        "{}/task/openapi/getWorkflowInfo?workflowId={}&apiKey={}",
        base_url.trim_end_matches('/'),
        urlencoding(&workflow_id),
        urlencoding(&api_key)
    );
    let client = Client::new();
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => return ok(json!({
            "success": false,
            "detail": format!("调用 RunningHub getWorkflowInfo 失败: {e}")
        })),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return ok(json!({
            "success": false,
            "detail": format!("RunningHub getWorkflowInfo 返回 {}: {}", status.as_u16(), upstream_detail(&text))
        }));
    }
    let raw: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    let upstream_ok = raw.get("code").and_then(|c| c.as_i64()).map(|c| c == 200)
        .unwrap_or(true);
    if !upstream_ok {
        let detail = upstream_detail(&text);
        return ok(json!({ "success": false, "detail": detail, "raw": raw }));
    }
    let node_info_list = extract_node_list(&raw);
    let fields = node_info_list.clone();

    // 组装工作流对象入库
    let workflow = json!({
        "workflowId": workflow_id,
        "title": title,
        "description": description,
        "fields": fields,
        "workflowJson": raw.get("workflowJson").cloned().unwrap_or(json!({})),
        "optionalImageMode": raw.get("optionalImageMode").cloned().unwrap_or(json!(null)),
        "raw": raw,
        "updatedAt": chrono::Utc::now().timestamp_millis(),
    });
    let stored = {
        let mut s = state.0.lock().unwrap();
        s.upsert_rh_workflow(workflow.clone())
    };

    ok(json!({
        "success": true,
        "data": {
            "workflowId": stored.get("workflowId").cloned().unwrap_or(json!(workflow_id)),
            "title": stored.get("title").cloned().unwrap_or(json!(title)),
            "description": stored.get("description").cloned().unwrap_or(json!(description)),
            "fields": stored.get("fields").cloned().unwrap_or(json!([])),
            "workflowJson": stored.get("workflowJson").cloned().unwrap_or(json!({})),
            "raw": stored.get("raw").cloned().unwrap_or(json!({})),
        }
    }))
}

// ====================================================================
// 5. PUT /api/runninghub/workflows/:workflowId
// 更新本地工作流配置
// 请求体: {workflowId, title, description, fields, workflowJson, optionalImageMode, raw}
// ====================================================================
pub async fn update_workflow(
    State(state): State<AppState>,
    Path(workflow_id): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    // 路径参数优先,确保 id 一致
    if let Some(obj) = body.as_object_mut() {
        obj.insert("workflowId".into(), json!(workflow_id));
        obj.insert("updatedAt".into(), json!(chrono::Utc::now().timestamp_millis()));
    }
    let stored = {
        let mut s = state.0.lock().unwrap();
        s.upsert_rh_workflow(body)
    };
    ok(json!({ "success": true, "workflow": stored }))
}

// ====================================================================
// 6. DELETE /api/runninghub/workflows/:workflowId
// 删除本地缓存的工作流
// ====================================================================
pub async fn delete_workflow(
    State(state): State<AppState>,
    Path(workflow_id): Path<String>,
) -> Response {
    let done = {
        let mut s = state.0.lock().unwrap();
        s.delete_rh_workflow(&workflow_id)
    };
    ok(json!({ "success": done }))
}

// ====================================================================
// 7. POST /api/runninghub/upload-asset
// 上传素材(图片 URL)到 RunningHub
// 请求体: {url, useWallet:false}
// ====================================================================
pub async fn upload_asset(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let use_wallet = body.get("useWallet").and_then(|v| v.as_bool()).unwrap_or(false);
    let api_key = match pick_api_key(&state, use_wallet) {
        Some(k) if !k.is_empty() => k,
        _ => return not_configured("RunningHub"),
    };
    let (_, base_url) = match resolve_provider(&state) {
        Some(v) => v,
        None => return not_configured("RunningHub"),
    };
    let image_url = body.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if image_url.is_empty() {
        return ok(json!({ "success": false, "detail": "url 不能为空" }));
    }

    let url = format!("{}/task/openapi/uploadImage", base_url.trim_end_matches('/'));
    let req_body = json!({ "apiKey": api_key, "imageUrl": image_url });

    let client = Client::new();
    let resp = match client.post(&url).json(&req_body).send().await {
        Ok(r) => r,
        Err(e) => return ok(json!({
            "success": false,
            "detail": format!("调用 RunningHub uploadImage 失败: {e}")
        })),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return ok(json!({
            "success": false,
            "detail": format!("RunningHub uploadImage 返回 {}: {}", status.as_u16(), upstream_detail(&text))
        }));
    }
    let raw: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    let upstream_ok = raw.get("code").and_then(|c| c.as_i64()).map(|c| c == 200)
        .unwrap_or(true);
    if !upstream_ok {
        let detail = upstream_detail(&text);
        return ok(json!({ "success": false, "detail": detail, "raw": raw }));
    }
    // 上游返回 data.fileName
    let file_name = raw.get("data").and_then(|d| d.get("fileName"))
        .and_then(|v| v.as_str()).map(|s| s.to_string())
        .or_else(|| raw.get("fileName").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    ok(json!({
        "success": true,
        "data": { "fileName": file_name, "raw": raw }
    }))
}

// ====================================================================
// 8. POST /api/runninghub/submit
// app 模式提交任务
// 请求体: {webappId, nodeInfoList, instanceType, useWallet:false}
// ====================================================================
pub async fn submit(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let use_wallet = body.get("useWallet").and_then(|v| v.as_bool()).unwrap_or(false);
    let api_key = match pick_api_key(&state, use_wallet) {
        Some(k) if !k.is_empty() => k,
        _ => return not_configured("RunningHub"),
    };
    let (_, base_url) = match resolve_provider(&state) {
        Some(v) => v,
        None => return not_configured("RunningHub"),
    };
    let webapp_id = body.get("webappId").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if webapp_id.is_empty() {
        return ok(json!({ "success": false, "detail": "webappId 不能为空" }));
    }
    let node_info_list = body.get("nodeInfoList").cloned().unwrap_or(json!([]));
    let instance_type = body.get("instanceType").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let url = format!("{}/task/openapi/create", base_url.trim_end_matches('/'));
    let req_body = json!({
        "apiKey": api_key,
        "webappId": webapp_id,
        "nodeInfoList": node_info_list,
        "instanceType": instance_type,
    });

    let client = Client::new();
    let resp = match client.post(&url).json(&req_body).send().await {
        Ok(r) => r,
        Err(e) => return ok(json!({
            "success": false,
            "detail": format!("调用 RunningHub create 失败: {e}")
        })),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return ok(json!({
            "success": false,
            "detail": format!("RunningHub create 返回 {}: {}", status.as_u16(), upstream_detail(&text))
        }));
    }
    let raw: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    let upstream_ok = raw.get("code").and_then(|c| c.as_i64()).map(|c| c == 200)
        .unwrap_or(true);
    if !upstream_ok {
        let detail = upstream_detail(&text);
        return ok(json!({ "success": false, "detail": detail, "raw": raw }));
    }
    let task_id = raw.get("data").and_then(|d| d.get("taskId"))
        .and_then(|v| v.as_str()).map(|s| s.to_string())
        .or_else(|| raw.get("taskId").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    ok(json!({
        "success": true,
        "data": { "taskId": task_id, "raw": raw }
    }))
}

// ====================================================================
// 9. POST /api/runninghub/workflow-submit
// workflow 模式提交任务
// 请求体: {workflowId, nodeInfoList, useWallet:false, workflow:{...可选}}
// ====================================================================
pub async fn workflow_submit(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let use_wallet = body.get("useWallet").and_then(|v| v.as_bool()).unwrap_or(false);
    let api_key = match pick_api_key(&state, use_wallet) {
        Some(k) if !k.is_empty() => k,
        _ => return not_configured("RunningHub"),
    };
    let (_, base_url) = match resolve_provider(&state) {
        Some(v) => v,
        None => return not_configured("RunningHub"),
    };
    let workflow_id = body.get("workflowId").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if workflow_id.is_empty() {
        return ok(json!({ "success": false, "detail": "workflowId 不能为空" }));
    }
    let node_info_list = body.get("nodeInfoList").cloned().unwrap_or(json!([]));

    // 组装请求体:固定字段 + workflow 子对象中的字段平铺合并
    let mut req_body = json!({
        "apiKey": api_key,
        "workflowId": workflow_id,
        "nodeInfoList": node_info_list,
    });
    if let Some(wf) = body.get("workflow") {
        if let Some(obj) = wf.as_object() {
            if let Some(target) = req_body.as_object_mut() {
                for (k, v) in obj {
                    // 不覆盖已设置的固定字段
                    target.entry(k.clone()).or_insert(v.clone());
                }
            }
        }
    }

    let url = format!("{}/task/openapi/createWorkflow", base_url.trim_end_matches('/'));
    let client = Client::new();
    let resp = match client.post(&url).json(&req_body).send().await {
        Ok(r) => r,
        Err(e) => return ok(json!({
            "success": false,
            "detail": format!("调用 RunningHub createWorkflow 失败: {e}")
        })),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return ok(json!({
            "success": false,
            "detail": format!("RunningHub createWorkflow 返回 {}: {}", status.as_u16(), upstream_detail(&text))
        }));
    }
    let raw: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    let upstream_ok = raw.get("code").and_then(|c| c.as_i64()).map(|c| c == 200)
        .unwrap_or(true);
    if !upstream_ok {
        let detail = upstream_detail(&text);
        return ok(json!({ "success": false, "detail": detail, "raw": raw }));
    }
    let task_id = raw.get("data").and_then(|d| d.get("taskId"))
        .and_then(|v| v.as_str()).map(|s| s.to_string())
        .or_else(|| raw.get("taskId").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .unwrap_or_default();
    ok(json!({
        "success": true,
        "data": { "taskId": task_id, "raw": raw }
    }))
}

// ====================================================================
// 10. GET /api/runninghub/query?taskId=xxx&useWallet=0|1
// 查询任务状态。前端用 GET,内部转发到 RunningHub POST /task/openapi/query
// ====================================================================
pub async fn query(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let use_wallet = q.get("useWallet")
        .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let api_key = match pick_api_key(&state, use_wallet) {
        Some(k) if !k.is_empty() => k,
        _ => return not_configured("RunningHub"),
    };
    let (_, base_url) = match resolve_provider(&state) {
        Some(v) => v,
        None => return not_configured("RunningHub"),
    };
    let task_id = match q.get("taskId") {
        Some(s) if !s.is_empty() => s.clone(),
        _ => return ok(json!({ "success": false, "detail": "taskId 不能为空" })),
    };

    let url = format!("{}/task/openapi/query", base_url.trim_end_matches('/'));
    let req_body = json!({ "apiKey": api_key, "taskId": task_id });

    let client = Client::new();
    let resp = match client.post(&url).json(&req_body).send().await {
        Ok(r) => r,
        Err(e) => return ok(json!({
            "success": false,
            "detail": format!("调用 RunningHub query 失败: {e}")
        })),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return ok(json!({
            "success": false,
            "detail": format!("RunningHub query 返回 {}: {}", status.as_u16(), upstream_detail(&text))
        }));
    }
    let raw: Value = serde_json::from_str(&text).unwrap_or_else(|_| json!({}));
    let upstream_ok = raw.get("code").and_then(|c| c.as_i64()).map(|c| c == 200)
        .unwrap_or(true);
    if !upstream_ok {
        let detail = upstream_detail(&text);
        return ok(json!({ "success": false, "detail": detail, "raw": raw }));
    }

    // 提取状态与输出
    let data = raw.get("data").cloned().unwrap_or(json!({}));
    let status_str = data.get("status").and_then(|v| v.as_str()).unwrap_or("UNKNOWN").to_string();
    let fail_reason = data.get("failReason").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let outputs = extract_outputs(&raw);
    // outputs 中可能直接是 url 字符串,也可能是 {url} 对象
    let urls: Vec<String> = outputs.iter().filter_map(|o| {
        if let Some(s) = o.as_str() { Some(s.to_string()) }
        else { o.get("url").and_then(|v| v.as_str()).map(|s| s.to_string()) }
    }).collect();

    // RUNNINGHUB 状态:SUCCESS / FAILED / QUEUED / RUNNING 等,统一映射
    let mapped_status = match status_str.to_uppercase().as_str() {
        "SUCCESS" => "SUCCESS",
        "FAILED" | "FAIL" => "FAILED",
        "QUEUED" => "QUEUED",
        "RUNNING" => "RUNNING",
        _ => "UNKNOWN",
    };
    ok(json!({
        "success": true,
        "data": {
            "status": mapped_status,
            "urls": urls,
            "image_items": outputs,
            "failReason": fail_reason,
            "raw": raw,
        }
    }))
}

/// 简易 URL 查询参数编码(仅对常见特殊字符做替换)。
/// 避免 100% 引入 urlencoding crate。workflowId / apiKey 一般只含字母数字与下划线。
fn urlencoding(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}
