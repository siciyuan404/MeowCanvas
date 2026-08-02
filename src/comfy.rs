// ComfyUI 后端处理器。
// 对接本地/远程 ComfyUI 实例,覆盖:
//   - ComfyUI 实例配置 (GET/PUT /api/comfyui/instances)
//   - 本地工作流 CRUD (/api/workflows)
//   - ComfyUI 风格文件上传与图片查看代理 (/api/upload, /api/view)
//   - 工作流同步测试运行 (/api/workflows/:name/run)
//   - 画布异步 ComfyUI 任务 (/api/canvas-comfy-tasks)
// 所有错误统一返回 {detail:"..."},实例 URL 拼接为 http://{instance}/...。
use crate::http_util::{bad, bad_gateway, not_configured, not_found, ok, upstream_error, AppState};
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex as TokioMutex;

// ===== 画布 ComfyUI 异步任务内存存储 (our_task_id -> 任务信息) =====
#[derive(Clone)]
struct ComfyTaskInfo {
    prompt_id: String,
    instance: String,
}

static COMFY_TASK_STORE: OnceLock<TokioMutex<HashMap<String, ComfyTaskInfo>>> = OnceLock::new();

fn comfy_task_store() -> &'static TokioMutex<HashMap<String, ComfyTaskInfo>> {
    COMFY_TASK_STORE.get_or_init(|| TokioMutex::new(HashMap::new()))
}

/// 从 store 取出第一个 ComfyUI 实例 (形如 "127.0.0.1:8188"),没有则返回 None
fn first_instance(state: &AppState) -> Option<String> {
    let s = state.0.lock().unwrap();
    s.list_comfy_instances().into_iter()
        .next()
        .and_then(|v| {
            if let Some(s) = v.as_str() { Some(s.to_string()) }
            else { Some(v.to_string().trim_matches('"').to_string()) }
        })
        .filter(|s| !s.is_empty())
}

// ====================================================================
// 1. GET /api/comfyui/instances — 列出 ComfyUI 实例
// 响应: {instances:["127.0.0.1:8188", ...]}
// ====================================================================
pub async fn comfy_instances_get(State(state): State<AppState>) -> Response {
    let s = state.0.lock().unwrap();
    // 每个元素统一转成字符串
    let instances: Vec<Value> = s.list_comfy_instances().into_iter()
        .map(|v| {
            match v {
                Value::String(s) => Value::String(s),
                other => Value::String(other.to_string().trim_matches('"').to_string()),
            }
        })
        .collect();
    ok(json!({ "instances": instances }))
}

// ====================================================================
// 2. PUT /api/comfyui/instances — 保存 ComfyUI 实例
// 请求体: {instances:["host:port", ...]}
// 响应: {instances:[...]}
// ====================================================================
pub async fn comfy_instances_put(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let instances = body.get("instances").and_then(|v| v.as_array())
        .cloned().unwrap_or_default();
    let mut s = state.0.lock().unwrap();
    s.save_comfy_instances(instances.clone());
    ok(json!({ "instances": instances }))
}

// ====================================================================
// 3. GET /api/workflows — 工作流列表
// 响应: {workflows:[{name,title,field_count,builtin,updated_at}]}
// ====================================================================
pub async fn workflows_list(State(state): State<AppState>) -> Response {
    let s = state.0.lock().unwrap();
    let workflows: Vec<Value> = s.list_workflows().into_iter().map(|w| {
        let name = w.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let config = w.get("config").cloned().unwrap_or(json!({}));
        let title = config.get("title").and_then(|v| v.as_str()).unwrap_or(&name).to_string();
        let field_count = config.get("fields").and_then(|v| v.as_array())
            .map(|a| a.len()).unwrap_or(0);
        let builtin = w.get("builtin").and_then(|v| v.as_bool()).unwrap_or(false);
        let updated_at = w.get("updated_at").cloned().unwrap_or(json!(null));
        json!({
            "name": name,
            "title": title,
            "field_count": field_count,
            "builtin": builtin,
            "updated_at": updated_at,
        })
    }).collect();
    ok(json!({ "workflows": workflows }))
}

// ====================================================================
// 4. GET /api/workflows/:name — 单个工作流
// 响应: {workflow:<ComfyUI JSON>, config:{...}, builtin:false}
// ====================================================================
pub async fn workflow_get(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let w = {
        let s = state.0.lock().unwrap();
        s.get_workflow(&name)
    };
    match w {
        Some(w) => {
            let workflow = w.get("workflow").cloned().unwrap_or(json!({}));
            let config = w.get("config").cloned().unwrap_or(json!({}));
            let builtin = w.get("builtin").and_then(|v| v.as_bool()).unwrap_or(false);
            ok(json!({ "workflow": workflow, "config": config, "builtin": builtin }))
        }
        None => not_found("工作流不存在"),
    }
}

// ====================================================================
// 5. POST /api/workflows — 上传新工作流
// 请求体: {name:"xxx", workflow:{...}}
// ====================================================================
pub async fn workflow_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if name.is_empty() {
        return bad("name 不能为空");
    }
    let workflow = body.get("workflow").cloned().unwrap_or(json!({}));
    // 新建工作流默认空配置
    let config = json!({ "title": name, "fields": [], "mini_cards": {} });
    let doc = json!({ "workflow": workflow, "config": config });
    let saved = {
        let mut s = state.0.lock().unwrap();
        s.upsert_workflow(&name, doc)
    };
    ok(json!({
        "name": name,
        "workflow": saved.get("workflow").cloned().unwrap_or(json!({})),
        "config": saved.get("config").cloned().unwrap_or(json!({})),
        "builtin": saved.get("builtin").and_then(|v| v.as_bool()).unwrap_or(false),
    }))
}

// ====================================================================
// 6. PUT /api/workflows/:name/config — 更新工作流配置
// 请求体: 完整 config 对象 {title, fields, mini_cards}
// 响应: 更新后的 workflow 对象
// ====================================================================
pub async fn workflow_config_update(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    // 先读现有 workflow JSON,保留原样,仅替换 config
    let existing = {
        let s = state.0.lock().unwrap();
        s.get_workflow(&name)
    };
    let existing = match existing {
        Some(w) => w,
        None => return not_found("工作流不存在"),
    };
    let workflow = existing.get("workflow").cloned().unwrap_or(json!({}));
    let doc = json!({ "workflow": workflow, "config": body });
    let saved = {
        let mut s = state.0.lock().unwrap();
        s.upsert_workflow(&name, doc)
    };
    ok(saved)
}

// ====================================================================
// 7. DELETE /api/workflows/:name — 删除工作流
// 响应: {success:true}
// ====================================================================
pub async fn workflow_delete(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Response {
    let done = {
        let mut s = state.0.lock().unwrap();
        s.delete_workflow(&name)
    };
    ok(json!({ "success": done }))
}

// ====================================================================
// 8. POST /api/upload — ComfyUI 风格文件上传 (Multipart, 字段名 "files")
// 优先转发到第一个 ComfyUI 实例的 /upload/image,失败则存本地 uploads 目录
// 响应: ComfyUI 格式 {files:[{comfy_name, filename, subfolder, type}]}
// ====================================================================
pub async fn upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Response {
    // 读取所有文件字段 (兼容 "files" / "image" 等任意带文件名的字段)
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            _ => break,
        };
        let filename = field.file_name().unwrap_or("upload.bin").to_string();
        let data = match field.bytes().await {
            Ok(b) => b.to_vec(),
            Err(_) => continue,
        };
        files.push((filename, data));
    }

    if files.is_empty() {
        return bad("未提供文件");
    }

    // 取第一个 ComfyUI 实例,尝试转发到其 /upload/image 端点
    let instance = first_instance(&state);
    if let Some(inst) = instance {
        let mut form = reqwest::multipart::Form::new();
        for (name, data) in &files {
            // ComfyUI /upload/image 期望字段名为 "image"
            let part = reqwest::multipart::Part::bytes(data.clone())
                .file_name(name.clone());
            form = form.part("image", part);
        }
        let url = format!("http://{}/upload/image", inst);
        let client = reqwest::Client::new();
        match client.post(&url).multipart(form).send().await {
            Ok(resp) if resp.status().is_success() => {
                // 原样返回上游 JSON 响应
                let body_json: Value = resp.json().await.unwrap_or(json!({}));
                return ok(body_json);
            }
            _ => {
                // 转发失败 (无实例或上游错误),回退到本地存储
            }
        }
    }

    // 本地回退:保存到 store.uploads_dir()
    let uploads_dir = {
        let s = state.0.lock().unwrap();
        s.uploads_dir()
    };
    let _ = std::fs::create_dir_all(&uploads_dir);
    let mut out_files: Vec<Value> = Vec::new();
    for (name, data) in &files {
        let path = uploads_dir.join(name);
        if std::fs::write(&path, data).is_ok() {
            out_files.push(json!({
                "comfy_name": name,
                "filename": name,
                "subfolder": "",
                "type": "input",
            }));
        }
    }
    ok(json!({ "files": out_files }))
}

// ====================================================================
// 9. GET /api/view — ComfyUI 图片查看代理
// 查询参数: filename, type=input, subfolder?(可选)
// 有实例时代理到 http://{instance}/view;无实例时从本地 uploads 读取
// 响应: 直接返回图片字节 (非 JSON)
// ====================================================================
pub async fn view_proxy(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let filename = q.get("filename").cloned().unwrap_or_default();
    let ftype = q.get("type").cloned().unwrap_or_else(|| "input".to_string());
    let subfolder = q.get("subfolder").cloned().unwrap_or_default();

    if let Some(inst) = first_instance(&state) {
        let client = reqwest::Client::new();
        let req = client.get(format!("http://{}/view", inst))
            .query(&[
                ("filename", filename.as_str()),
                ("type", ftype.as_str()),
                ("subfolder", subfolder.as_str()),
            ]);
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                let content_type = resp.headers().get(header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("application/octet-stream").to_string();
                let bytes = resp.bytes().await.unwrap_or_default().to_vec();
                return (
                    StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK),
                    [(header::CONTENT_TYPE, content_type)],
                    bytes,
                ).into_response();
            }
            Err(_) => {
                // 连接失败,回退到本地
            }
        }
    }

    // 无实例或代理失败:从本地 uploads 目录读取
    let uploads_dir = {
        let s = state.0.lock().unwrap();
        s.uploads_dir()
    };
    let path = uploads_dir.join(&filename);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string())],
                bytes,
            ).into_response()
        }
        Err(_) => not_found("文件不存在"),
    }
}

// ====================================================================
// 10. POST /api/workflows/:name/run — 工作流同步测试运行
// 请求体: {fields:{fieldId:value}, config:{...}, client_id:"workflow-test"}
// 响应: {images:["url1","url2",...]} 或 {detail}
// ====================================================================
pub async fn workflow_run(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    // 取工作流定义
    let stored = {
        let s = state.0.lock().unwrap();
        s.get_workflow(&name)
    };
    let stored = match stored {
        Some(w) => w,
        None => return not_found("工作流不存在"),
    };
    let mut workflow = stored.get("workflow").cloned().unwrap_or(json!({}));
    let stored_config = stored.get("config").cloned().unwrap_or(json!({}));
    // 请求体里的 config 优先,否则用存储的 config
    let config = body.get("config").cloned().unwrap_or(stored_config);
    let fields = body.get("fields").cloned().unwrap_or(json!({}));
    let client_id = body.get("client_id").and_then(|v| v.as_str())
        .unwrap_or("workflow-test").to_string();

    // 把 fields 按 config.fields 映射写回 workflow 节点 inputs
    apply_fields(&mut workflow, &fields, &config);

    // 取第一个 ComfyUI 实例
    let instance = match first_instance(&state) {
        Some(i) => i,
        None => return not_configured("ComfyUI 运行"),
    };

    let client = reqwest::Client::new();
    let prompt_url = format!("http://{}/prompt", instance);
    let prompt_body = json!({ "prompt": workflow, "client_id": client_id });

    // 提交 prompt
    let resp = match client.post(&prompt_url).json(&prompt_body).send().await {
        Ok(r) => r,
        Err(e) => return bad_gateway(format!("连接 ComfyUI 失败: {e}")),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return upstream_error(status, text).await;
    }
    let resp_json: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    let prompt_id = match resp_json.get("prompt_id").and_then(|v| v.as_str()).map(|s| s.to_string()) {
        Some(id) => id,
        None => return bad_gateway(format!("ComfyUI 未返回 prompt_id: {}", text)),
    };

    // 轮询 history 直到出结果 (最多 120 秒,每 1.5 秒一次)
    let history_url = format!("http://{}/history/{}", instance, prompt_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        if std::time::Instant::now() > deadline {
            return bad_gateway("ComfyUI 任务执行超时 (120s)");
        }
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let hresp = match client.get(&history_url).send().await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let hjson: Value = match hresp.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        // history 格式: {prompt_id: {outputs:{...}, status:{status_str, messages}}}
        if let Some(task) = hjson.get(prompt_id.as_str()) {
            let status_str = task.get("status").and_then(|s| s.get("status_str"))
                .and_then(|v| v.as_str()).unwrap_or("");
            if status_str == "error" {
                let msgs = task.get("status").and_then(|s| s.get("messages"))
                    .map(|m| m.to_string()).unwrap_or_else(|| "ComfyUI 执行失败".to_string());
                return bad_gateway(format!("ComfyUI 执行失败: {}", msgs));
            }
            // 有 outputs 即视为成功
            if let Some(outputs) = task.get("outputs") {
                let images = collect_output_images(outputs, &instance);
                return ok(json!({ "images": images }));
            }
        }
    }
}

// ====================================================================
// 11. POST /api/canvas-comfy-tasks — 异步 ComfyUI 任务 (画布节点用)
// 请求体: {prompt?, width?, height?, workflow_json, params:{nodeId:{field:value}},
//          type:"zimage"|"klein"|..., client_id}
// 响应: {task_id:"our_task_id"}
// ====================================================================
pub async fn canvas_comfy_tasks_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    // 解析工作流: workflow_json 可以是工作流名称 (查 store),也可以是直接传入的 JSON
    let mut workflow = resolve_workflow(&state, &body);

    // 把 params 合并进 workflow 节点 inputs
    if let Some(params) = body.get("params") {
        apply_params(&mut workflow, params);
    }

    let client_id = body.get("client_id").and_then(|v| v.as_str())
        .unwrap_or("canvas-task").to_string();

    // 取第一个 ComfyUI 实例
    let instance = match first_instance(&state) {
        Some(i) => i,
        None => return not_configured("ComfyUI 画布任务"),
    };

    let client = reqwest::Client::new();
    let prompt_url = format!("http://{}/prompt", instance);
    let prompt_body = json!({ "prompt": workflow, "client_id": client_id });

    // 提交 prompt
    let resp = match client.post(&prompt_url).json(&prompt_body).send().await {
        Ok(r) => r,
        Err(e) => return bad_gateway(format!("连接 ComfyUI 失败: {e}")),
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return upstream_error(status, text).await;
    }
    let resp_json: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    let prompt_id = match resp_json.get("prompt_id").and_then(|v| v.as_str()).map(|s| s.to_string()) {
        Some(id) => id,
        None => return bad_gateway(format!("ComfyUI 未返回 prompt_id: {}", text)),
    };

    // 生成我们的 task_id,映射到 ComfyUI prompt_id
    let our_task_id = uuid::Uuid::new_v4().to_string();
    comfy_task_store().lock().await.insert(our_task_id.clone(), ComfyTaskInfo {
        prompt_id,
        instance,
    });

    ok(json!({ "task_id": our_task_id }))
}

// ====================================================================
// 12. GET /api/canvas-comfy-tasks/:task_id — 轮询 ComfyUI 任务
// 响应: {status:"running"} | {status:"succeeded", result:{images,videos,audios,outputs}}
//       | {status:"failed", error:"..."}
// 任务完成后从内存表删除
// ====================================================================
pub async fn canvas_comfy_task_poll(Path(task_id): Path<String>) -> Response {
    let info = {
        let store = comfy_task_store().lock().await;
        store.get(&task_id).cloned()
    };
    let info = match info {
        Some(i) => i,
        None => return not_found("任务不存在或已过期"),
    };

    let client = reqwest::Client::new();
    let history_url = format!("http://{}/history/{}", info.instance, info.prompt_id);
    let resp = match client.get(&history_url).send().await {
        Ok(r) => r,
        Err(e) => return bad_gateway(format!("查询 ComfyUI 历史失败: {e}")),
    };
    let hjson: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return ok(json!({ "status": "running" })),
    };

    // history 中没有该 prompt_id -> 仍在队列/执行中
    let task = match hjson.get(info.prompt_id.as_str()) {
        Some(t) => t,
        None => return ok(json!({ "status": "running" })),
    };

    let status_str = task.get("status").and_then(|s| s.get("status_str"))
        .and_then(|v| v.as_str()).unwrap_or("");
    if status_str == "error" {
        comfy_task_store().lock().await.remove(&task_id);
        let error = task.get("status").and_then(|s| s.get("messages"))
            .map(|m| m.to_string())
            .unwrap_or_else(|| "ComfyUI 执行失败".to_string());
        return ok(json!({ "status": "failed", "error": error }));
    }

    // 有 outputs 即视为成功,收集各类输出 URL
    if let Some(outputs) = task.get("outputs") {
        let (images, videos, audios, output_files) = collect_outputs(outputs, &info.instance);
        comfy_task_store().lock().await.remove(&task_id);
        return ok(json!({
            "status": "succeeded",
            "result": {
                "images": images,
                "videos": videos,
                "audios": audios,
                "outputs": output_files,
            }
        }));
    }

    ok(json!({ "status": "running" }))
}

// ======================== 辅助函数 ========================

/// 从 Value 中按多个候选 key 取字符串值
fn get_str<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return s;
        }
    }
    ""
}

/// 把 fields (fieldId -> value) 按 config.fields 映射写回 workflow 节点的 inputs
/// config.fields 中每个条目含 id / nodeId / fieldName
fn apply_fields(workflow: &mut Value, fields: &Value, config: &Value) {
    let fields_map = match fields.as_object() {
        Some(o) => o,
        None => return,
    };
    let config_fields = match config.get("fields").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return,
    };
    for f in config_fields {
        let id = get_str(f, &["id"]);
        if id.is_empty() { continue; }
        let val = match fields_map.get(id) {
            Some(v) => v,
            None => continue,
        };
        let node_id = get_str(f, &["nodeId", "node_id"]);
        let field_name = get_str(f, &["fieldName", "field_name", "inputName", "input_name"]);
        if node_id.is_empty() || field_name.is_empty() { continue; }
        if let Some(node) = workflow.get_mut(node_id) {
            if let Some(inputs) = node.get_mut("inputs").and_then(|v| v.as_object_mut()) {
                inputs.insert(field_name.to_string(), val.clone());
            }
        }
    }
}

/// 把 params (nodeId -> {field: value}) 合并进 workflow 对应节点的 inputs
fn apply_params(workflow: &mut Value, params: &Value) {
    let pmap = match params.as_object() {
        Some(o) => o,
        None => return,
    };
    for (node_id, fields) in pmap {
        let node = match workflow.get_mut(node_id.as_str()) {
            Some(n) => n,
            None => continue,
        };
        if let Some(inputs) = node.get_mut("inputs").and_then(|v| v.as_object_mut()) {
            if let Some(fmap) = fields.as_object() {
                for (k, v) in fmap {
                    inputs.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

/// 解析请求体中的工作流定义
/// workflow_json 可以是: 工作流名称 (查 store) / JSON 字符串 / 直接传入的工作流对象
fn resolve_workflow(state: &AppState, body: &Value) -> Value {
    let wj = body.get("workflow_json").cloned().unwrap_or(json!(null));
    match &wj {
        // 字符串: 先当工作流名查 store,查不到再尝试当 JSON 字符串解析
        Value::String(name) => {
            let stored = {
                let s = state.0.lock().unwrap();
                s.get_workflow(name)
            };
            match stored {
                Some(w) => w.get("workflow").cloned().unwrap_or(json!({})),
                None => serde_json::from_str(name).unwrap_or(json!({})),
            }
        }
        // 对象: 若含 "workflow" 字段 (存储格式 {workflow,config,name}) 取其 workflow,否则直接用
        Value::Object(_) => {
            if wj.get("workflow").is_some() {
                wj.get("workflow").cloned().unwrap_or(json!({}))
            } else {
                wj
            }
        }
        _ => json!({}),
    }
}

/// 从 history outputs 中收集 SaveImage/PreviewImage 节点的首张图片 URL (同步运行用)
fn collect_output_images(outputs: &Value, instance: &str) -> Vec<Value> {
    let mut urls: Vec<String> = Vec::new();
    if let Some(obj) = outputs.as_object() {
        for (_node_id, node_out) in obj {
            // 每个节点取 images[0]
            if let Some(first) = node_out.get("images").and_then(|v| v.as_array()).and_then(|a| a.first()) {
                if let Some(url) = view_url(first, instance) {
                    urls.push(url);
                }
            }
        }
    }
    urls.into_iter().map(Value::String).collect()
}

/// 从 history outputs 中收集所有 images / videos / audios / 全部输出文件 URL (异步轮询用)
/// 返回 (images, videos, audios, outputs)
fn collect_outputs(outputs: &Value, instance: &str) -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
    let mut images = Vec::new();
    let mut videos = Vec::new();
    let mut audios = Vec::new();
    let mut output_files = Vec::new();
    if let Some(obj) = outputs.as_object() {
        for (_node_id, node_out) in obj {
            // 图片
            if let Some(arr) = node_out.get("images").and_then(|v| v.as_array()) {
                for item in arr {
                    if let Some(url) = view_url(item, instance) {
                        images.push(json!(url));
                        output_files.push(json!(url));
                    }
                }
            }
            // 视频
            if let Some(arr) = node_out.get("videos").and_then(|v| v.as_array()) {
                for item in arr {
                    if let Some(url) = view_url(item, instance) {
                        videos.push(json!(url));
                        output_files.push(json!(url));
                    }
                }
            }
            // 老版本 ComfyUI 用 gifs 表示动图,归入视频
            if let Some(arr) = node_out.get("gifs").and_then(|v| v.as_array()) {
                for item in arr {
                    if let Some(url) = view_url(item, instance) {
                        videos.push(json!(url));
                        output_files.push(json!(url));
                    }
                }
            }
            // 音频
            if let Some(arr) = node_out.get("audio").and_then(|v| v.as_array()) {
                for item in arr {
                    if let Some(url) = view_url(item, instance) {
                        audios.push(json!(url));
                        output_files.push(json!(url));
                    }
                }
            }
        }
    }
    (images, videos, audios, output_files)
}

/// 根据单个输出条目 (含 filename/subfolder/type) 组装 ComfyUI /view URL
fn view_url(item: &Value, instance: &str) -> Option<String> {
    let filename = item.get("filename").and_then(|v| v.as_str())?;
    if filename.is_empty() { return None; }
    let subfolder = item.get("subfolder").and_then(|v| v.as_str()).unwrap_or("");
    let ftype = item.get("type").and_then(|v| v.as_str()).unwrap_or("output");
    Some(format!(
        "http://{}/view?filename={}&type={}&subfolder={}",
        instance, filename, ftype, subfolder
    ))
}
