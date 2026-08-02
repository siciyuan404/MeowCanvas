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
use std::sync::Arc;

pub type AppState = Arc<SharedStore>;

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
pub async fn providers_get() -> Response { ok(json!({ "providers": [] })) }
pub async fn providers_test() -> Response { ok(json!({ "ok": false, "detail": "未配置" })) }
pub async fn providers_fetch_models() -> Response { ok(json!({ "models": [] })) }
pub async fn conversations() -> Response { ok(json!({ "conversations": [] })) }
pub async fn chat() -> Response { not_configured("GPT 对话") }
pub async fn chat_stream() -> Response { not_configured("GPT 流式对话") }
pub async fn ai_upload() -> Response { not_configured("AI 文件上传") }

// 更新检查:返回当前版本,告知已是最新 (避免离线包反复弹窗)
pub async fn check_update() -> Response {
    ok(json!({ "current": env!("CARGO_PKG_VERSION"), "reachable": false, "latest": null }))
}
