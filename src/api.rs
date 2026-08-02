// HTTP API 处理器。保留应用核心端点:应用信息 / 配置 / 项目 / 画布 / 队列 / providers。
// AI 生成 / ComfyUI / RunningHub / 素材库 / 提示词库 / 历史记录 / 会话 等端点已迁移到
// ai / comfy / runninghub / library 模块,本文件只保留路由所需的核心处理器。
use crate::http_util::ok;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use serde_json::{json, Value};
use std::collections::HashMap;

// 重新导出 AppState,server.rs 通过 api::AppState 引用
pub use crate::http_util::AppState;

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

// ===== 画布资源 (占位) =====
pub async fn canvas_assets_check(Json(_body): Json<Value>) -> Response {
    ok(json!({ "exists": false }))
}

pub async fn canvas_assets_download() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "detail": "画布资源下载未实现" }))).into_response()
}

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

// 更新检查:返回当前版本,告知已是最新 (避免离线包反复弹窗)
pub async fn check_update() -> Response {
    ok(json!({ "current": env!("CARGO_PKG_VERSION"), "reachable": false, "latest": null }))
}
