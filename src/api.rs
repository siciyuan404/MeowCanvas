// HTTP API 处理器。覆盖应用启动 / 画布核心 / 配置 / 历史记录所需的全部端点。
// AI 生成类端点 (generate / online-image / 角度等) 返回友好的未配置提示,
// 保证前端不崩溃,后续可逐步接入真实 provider。
use crate::store::SharedStore;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex as TokioMutex;

pub type AppState = Arc<SharedStore>;

// ===== 画布图像异步任务内存存储 (task_id -> ePhone 任务信息) =====
#[derive(Clone)]
struct TaskInfo {
    ephone_task_id: String,
    api_key: String,
    base_url: String,
}

static TASK_STORE: OnceLock<TokioMutex<HashMap<String, TaskInfo>>> = OnceLock::new();

fn task_store() -> &'static TokioMutex<HashMap<String, TaskInfo>> {
    TASK_STORE.get_or_init(|| TokioMutex::new(HashMap::new()))
}

fn ok(body: Value) -> Response {
    (StatusCode::OK, Json(body)).into_response()
}

/// 未配置 AI provider 时的统一占位响应
fn not_configured(name: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "detail": format!("该功能 ({}) 尚未配置 API。请在 API 设置中添加供应商。", name) })),
    ).into_response()
}

// ===== 应用信息 =====
pub async fn app_info() -> Response {
    ok(json!({
        "name": "MeowCanvas",
        "version": env!("CARGO_PKG_VERSION"),
        "repo_url": "https://github.com/hero8152/Infinite-Canvas",
        "version_url": "https://raw.githubusercontent.com/hero8152/Infinite-Canvas/main/VERSION",
        "sources": {
            "github": {
                "tree_url": "https://api.github.com/repos/hero8152/Infinite-Canvas/git/trees/main?recursive=1",
                "version_url": "https://raw.githubusercontent.com/hero8152/Infinite-Canvas/main/VERSION"
            }
        }
    }))
}

// ===== 配置 =====
pub async fn get_config() -> Response {
    // 从嵌入的 api_providers.json 加载供应商列表,让前端可以展示 ePhone AI / RunningHub 等平台
    let providers = crate::server::Asset::get("runninghub/api_providers.json")
        .and_then(|asset| serde_json::from_slice::<Value>(&asset.data).ok())
        .unwrap_or_else(|| json!([]));
    ok(json!({ "api_providers": providers }))
}
pub async fn get_config_token() -> Response { ok(json!({ "token": "" })) }

// ===== 项目 =====
pub async fn list_projects(State(state): State<AppState>) -> Response {
    let s = state.0.lock().unwrap();
    ok(json!({ "projects": s.list_projects() }))
}

pub async fn create_project(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let mut s = state.0.lock().unwrap();
    let p = s.create_project(body);
    ok(json!({ "project": p }))
}

pub async fn delete_project(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut s = state.0.lock().unwrap();
    let done = s.delete_project(&id);
    ok(json!({ "success": done }))
}

// ===== 画布 =====
pub async fn list_canvases(State(state): State<AppState>) -> Response {
    let s = state.0.lock().unwrap();
    ok(json!({ "canvases": s.list_canvases() }))
}

pub async fn list_trash(State(state): State<AppState>) -> Response {
    let s = state.0.lock().unwrap();
    ok(json!({ "canvases": s.list_trash() }))
}

pub async fn get_canvas(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let s = state.0.lock().unwrap();
    match s.get_canvas(&id) {
        Some(c) => ok(json!({ "canvas": c })),
        None => (StatusCode::NOT_FOUND, Json(json!({ "detail": "画布不存在" }))).into_response(),
    }
}

pub async fn create_canvas(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let mut s = state.0.lock().unwrap();
    let c = s.create_canvas(body);
    ok(json!({ "canvas": c }))
}

pub async fn update_canvas(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut s = state.0.lock().unwrap();
    match s.update_canvas(&id, body) {
        Some(c) => ok(json!({ "canvas": c })),
        None => (StatusCode::NOT_FOUND, Json(json!({ "detail": "画布不存在" }))).into_response(),
    }
}

pub async fn delete_canvas(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut s = state.0.lock().unwrap();
    let done = s.trash_canvas(&id);
    ok(json!({ "success": done }))
}

pub async fn purge_canvas(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut s = state.0.lock().unwrap();
    let done = s.purge_canvas(&id);
    ok(json!({ "success": done }))
}

// ===== 工作流 (ComfyUI) =====
pub async fn list_workflows() -> Response {
    // 暂无工作流;画布节点系统仍可正常使用,只是没有预置 ComfyUI 工作流
    ok(json!({ "workflows": [] }))
}

// ===== 队列状态 =====
pub async fn queue_status(Query(_q): Query<HashMap<String, String>>) -> Response {
    ok(json!({ "total": 0, "position": 0, "queue": [] }))
}

// ===== WebSocket 统计 =====
pub async fn ws_stats(ws: WebSocketUpgrade, Query(_q): Query<HashMap<String, String>>) -> Response {
    ws.on_upgrade(handle_ws)
}

async fn handle_ws(mut socket: WebSocket) {
    // 周期性推送空统计,前端 nano-monitor 保持空闲态
    let hello = json!({ "type": "stats", "online_count": 1 }).to_string();
    let _ = socket.send(Message::Text(hello.into())).await;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
    loop {
        interval.tick().await;
        let msg = Message::Text(json!({ "type": "stats", "online_count": 1 }).to_string().into());
        if socket.send(msg).await.is_err() {
            break;
        }
    }
}

// ===== 历史记录 =====
pub async fn list_history(Query(q): Query<HashMap<String, String>>) -> Response {
    // 前端期望直接是 JSON 数组: allHistory = await res.json(); allHistory.slice(...)
    let _t = q.get("type").cloned().unwrap_or_default();
    ok(json!([]))
}

pub async fn delete_history(Json(_body): Json<Value>) -> Response {
    ok(json!({ "success": true }))
}

// ===== 素材库 =====
pub async fn asset_library_get() -> Response {
    ok(json!({ "library": { "items": [], "workflows": [] } }))
}

pub async fn asset_library_rename(Json(body): Json<Value>) -> Response {
    let _ = body;
    ok(json!({ "library": { "items": [], "workflows": [] } }))
}

pub async fn asset_library_delete_item(Path(_id): Path<String>) -> Response {
    ok(json!({ "library": { "items": [], "workflows": [] } }))
}

pub async fn canvas_assets_check(Json(_body): Json<Value>) -> Response {
    ok(json!({ "exists": false }))
}

// 文件上传:返回一个占位 URL,前端流程不中断
pub async fn upload() -> Response {
    not_configured("文件上传")
}

// 画布资源下载占位
pub async fn canvas_assets_download() -> Response {
    not_configured("画布资源")
}

// ===== AI 生成占位端点 (前端会优雅显示错误) =====
pub async fn generate() -> Response { not_configured("文生图 generate") }
pub async fn online_image() -> Response { not_configured("在线生图 online-image") }
pub async fn angle_generate() -> Response { not_configured("角度控制 angle/generate") }
pub async fn angle_poll() -> Response { not_configured("角度控制 poll_status") }
pub async fn ms_generate() -> Response { not_configured("ModelScope ms/generate") }

// ===== Providers (用户配置的 API 供应商 + API Key 持久化) =====

/// GET /api/providers - 返回 providers 列表,api_key 已掩码为 has_key / key_preview
pub async fn providers_get(State(state): State<AppState>) -> Response {
    let s = state.0.lock().unwrap();
    let masked: Vec<Value> = s.list_providers().iter().map(mask_provider).collect();
    ok(json!({ "providers": masked }))
}

/// PUT /api/providers - 保存 providers,处理 api_key 增删,返回掩码后的列表
pub async fn providers_update(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let providers_in = match body.as_array() {
        Some(arr) => arr.clone(),
        None => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "detail": "请求体必须是数组" }))).into_response();
        }
    };
    let existing: HashMap<String, Value> = {
        let s = state.0.lock().unwrap();
        s.list_providers().into_iter()
            .filter_map(|p| {
                let id = p.get("id").and_then(|i| i.as_str())?.to_string();
                Some((id, p))
            })
            .collect()
    };
    let mut merged: Vec<Value> = Vec::new();
    for mut p in providers_in {
        if let Some(id) = p.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()) {
            let clear_key = p.get("clear_key").and_then(|v| v.as_bool()).unwrap_or(false);
            let new_key = p.get("api_key").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
            if let Some(old) = existing.get(&id) {
                if clear_key {
                    if let Some(obj) = p.as_object_mut() { obj.remove("api_key"); }
                } else if new_key.is_none() {
                    // 保留旧 key (前端未输入新 key 也未清除)
                    if let Some(old_key) = old.get("api_key").and_then(|v| v.as_str()) {
                        p["api_key"] = json!(old_key);
                    }
                }
            }
            // 移除前端临时字段,不持久化
            if let Some(obj) = p.as_object_mut() {
                obj.remove("clear_key");
                obj.remove("clear_wallet_key");
                obj.remove("clear_volcengine_access_key_id");
                obj.remove("clear_volcengine_secret_access_key");
                obj.remove("_clearKey");
                obj.remove("_clearWalletKey");
            }
            merged.push(p);
        }
    }
    let mut s = state.0.lock().unwrap();
    s.update_providers(merged);
    let masked: Vec<Value> = s.list_providers().iter().map(mask_provider).collect();
    ok(json!({ "providers": masked }))
}

/// 掩码 provider: 移除敏感字段,替换为 has_key / key_preview / key_env
fn mask_provider(p: &Value) -> Value {
    let mut m = p.clone();
    let has_key = m.get("api_key").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty());
    let has_wallet_key = m.get("wallet_api_key").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty());
    let key_preview = mask_key_preview(m.get("api_key").and_then(|v| v.as_str()).unwrap_or(""));
    if let Some(obj) = m.as_object_mut() {
        obj.remove("api_key");
        obj.remove("wallet_api_key");
        obj.remove("volcengine_access_key_id");
        obj.remove("volcengine_secret_access_key");
    }
    m["has_key"] = json!(has_key);
    m["has_wallet_key"] = json!(has_wallet_key);
    m["key_preview"] = json!(key_preview);
    m["key_env"] = json!("providers.json");
    m
}

fn mask_key_preview(key: &str) -> String {
    if key.is_empty() { return String::new(); }
    let len = key.len();
    if len <= 8 { return "****".to_string(); }
    let prefix = &key[..4];
    let suffix = &key[len - 4..];
    format!("{}****{}", prefix, suffix)
}

pub async fn providers_test() -> Response { ok(json!({ "ok": false, "detail": "未配置" })) }
pub async fn providers_fetch_models() -> Response { ok(json!({ "models": [] })) }

// ===== 画布图像异步任务 (ePhone gpt-image-2-dev 集成) =====

/// POST /api/canvas-image-tasks - 提交图像生成任务到 ePhone 异步任务 API
pub async fn canvas_image_tasks_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let provider_id = body.get("provider_id").and_then(|v| v.as_str()).unwrap_or("");
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let model = body.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("gpt-image-2-dev");
    let size = body.get("size").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && *s != "auto");
    let quality = body.get("quality").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && *s != "auto");
    let n = body.get("n").and_then(|v| v.as_i64()).filter(|&x| x > 0);

    if prompt.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "detail": "prompt 不能为空" }))).into_response();
    }

    // 查找 provider,获取 api_key 和 base_url
    let (api_key, base_url) = {
        let s = state.0.lock().unwrap();
        match s.get_provider(provider_id) {
            Some(p) => {
                let key = p.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let url = p.get("base_url").and_then(|v| v.as_str())
                    .unwrap_or("https://api.ephone.ai/v1").to_string();
                (key, url)
            }
            None => {
                return (StatusCode::BAD_REQUEST, Json(json!({
                    "detail": format!("未找到 provider: {}", provider_id)
                }))).into_response();
            }
        }
    };

    if api_key.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({
            "detail": "该 provider 未配置 API Key,请在 API 设置中添加。"
        }))).into_response();
    }

    // 构建 ePhone 异步任务请求体
    let mut input = json!({ "prompt": prompt });
    if let Some(sz) = size { input["size"] = json!(sz); }
    if let Some(q) = quality { input["quality"] = json!(q); }
    if let Some(nn) = n { input["n"] = json!(nn); }

    let submit_body = json!({ "model": model, "input": input });
    let submit_url = format!("{}/task/submit", base_url.trim_end_matches('/'));

    let client = reqwest::Client::new();
    let resp = match client
        .post(&submit_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&submit_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, Json(json!({
                "detail": format!("调用 ePhone API 失败: {e}")
            }))).into_response();
        }
    };

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let body_json: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| json!({}));

    if !status.is_success() {
        let detail = body_json.get("error").or_else(|| body_json.get("detail"))
            .and_then(|v| v.as_str()).unwrap_or(&body_text);
        return (StatusCode::BAD_GATEWAY, Json(json!({
            "detail": format!("ePhone 提交失败: {}", detail)
        }))).into_response();
    }

    let ephone_task_id = match body_json.get("id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            return (StatusCode::BAD_GATEWAY, Json(json!({
                "detail": format!("ePhone 返回缺少 task id: {}", body_text)
            }))).into_response();
        }
    };

    // 生成我们的 task_id,映射到 ePhone task_id
    let our_task_id = uuid::Uuid::new_v4().to_string();
    task_store().lock().await.insert(our_task_id.clone(), TaskInfo {
        ephone_task_id,
        api_key,
        base_url,
    });

    ok(json!({ "task_id": our_task_id }))
}

/// GET /api/canvas-image-tasks/:id - 轮询 ePhone 任务状态
pub async fn canvas_image_tasks_get(
    Path(task_id): Path<String>,
) -> Response {
    let info = {
        let store = task_store().lock().await;
        store.get(&task_id).cloned()
    };
    let info = match info {
        Some(i) => i,
        None => {
            return (StatusCode::NOT_FOUND, Json(json!({
                "detail": "任务不存在或已过期"
            }))).into_response();
        }
    };

    let poll_url = format!("{}/task/{}", info.base_url.trim_end_matches('/'), info.ephone_task_id);
    let client = reqwest::Client::new();
    let resp = match client
        .get(&poll_url)
        .header("Authorization", format!("Bearer {}", info.api_key))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, Json(json!({
                "detail": format!("查询 ePhone 任务状态失败: {e}")
            }))).into_response();
        }
    };

    let body_text = resp.text().await.unwrap_or_default();
    let body_json: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| json!({}));

    let status = body_json.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
    match status {
        "completed" => {
            let outputs = body_json.get("outputs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let images: Vec<Value> = outputs.iter().map(|url| json!({ "url": url })).collect();
            ok(json!({
                "status": "succeeded",
                "result": { "images": images }
            }))
        }
        "failed" => {
            let error = body_json.get("error").and_then(|v| v.as_str()).unwrap_or("生成失败");
            ok(json!({ "status": "failed", "error": error }))
        }
        _ => {
            // queued / in_progress -> 前端继续轮询
            ok(json!({ "status": status }))
        }
    }
}
pub async fn conversations() -> Response { ok(json!({ "conversations": [] })) }
pub async fn chat() -> Response { not_configured("GPT 对话") }
pub async fn chat_stream() -> Response { not_configured("GPT 流式对话") }
pub async fn ai_upload() -> Response { not_configured("AI 文件上传") }

// 更新检查:返回当前版本,告知已是最新 (避免离线包反复弹窗)
pub async fn check_update() -> Response {
    ok(json!({ "current": env!("CARGO_PKG_VERSION"), "reachable": false, "latest": null }))
}
