// Axum 路由 + 嵌入式静态资源服务。前端静态文件在编译期通过 rust-embed 打包进二进制,
// 运行时无需额外文件,实现真正的单文件桌面应用。
use crate::api;
use crate::store::{SharedStore, Store};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, delete};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

#[derive(RustEmbed)]
#[folder = "frontend/"]
pub struct Asset;

pub struct Server {
    pub port: u16,
}

/// 启动嵌入式 HTTP 服务,返回监听端口
pub fn spawn_server(data_dir: &std::path::Path) -> anyhow::Result<Server> {
    let store = Store::open(data_dir)?;
    let state: api::AppState = Arc::new(SharedStore(std::sync::Mutex::new(store)));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        // ===== 静态入口 =====
        .route("/", get(index_html))
        .route("/static/*path", get(static_handler))
        .route("/favicon.ico", get(favicon))
        // ===== 应用信息 / 配置 =====
        .route("/api/app-info", get(api::app_info))
        .route("/api/config", get(api::get_config))
        .route("/api/config/token", get(api::get_config_token))
        // ===== 项目 =====
        .route("/api/projects", get(api::list_projects).post(api::create_project))
        .route("/api/projects/:id", delete(api::delete_project))
        // ===== 画布 =====
        .route("/api/canvases", get(api::list_canvases).post(api::create_canvas))
        .route("/api/canvases/trash", get(api::list_trash))
        .route("/api/canvases/:id", get(api::get_canvas).put(api::update_canvas).delete(api::delete_canvas))
        .route("/api/canvases/:id/purge", delete(api::purge_canvas))
        // ===== 工作流 / 队列 / WebSocket =====
        .route("/api/workflows", get(api::list_workflows))
        .route("/api/queue_status", get(api::queue_status))
        .route("/ws/stats", get(api::ws_stats))
        // ===== 历史记录 / 素材库 =====
        .route("/api/history", get(api::list_history))
        .route("/api/history/delete", post(api::delete_history))
        .route("/api/asset-library", get(api::asset_library_get).patch(api::asset_library_rename))
        .route("/api/asset-library/items/:id", delete(api::asset_library_delete_item))
        .route("/api/canvas-assets/check", post(api::canvas_assets_check))
        .route("/api/canvas-assets/download", get(api::canvas_assets_download).post(api::canvas_assets_download))
        // ===== AI 生成占位 =====
        .route("/api/upload", post(api::upload))
        .route("/api/generate", post(api::generate))
        .route("/api/online-image", post(api::online_image))
        .route("/api/angle/generate", post(api::angle_generate))
        .route("/api/angle/poll_status", get(api::angle_poll))
        .route("/api/ms/generate", post(api::ms_generate))
        // ===== 画布图像异步任务 =====
        .route("/api/canvas-image-tasks", post(api::canvas_image_tasks_create))
        .route("/api/canvas-image-tasks/:id", get(api::canvas_image_tasks_get))
        .route("/api/providers", get(api::providers_get))
        .route("/api/providers/test-connection", post(api::providers_test))
        .route("/api/providers/fetch-models", post(api::providers_fetch_models))
        .route("/api/conversations", get(api::conversations).post(api::conversations))
        .route("/api/chat", post(api::chat))
        .route("/api/chat/agent", post(api::chat))
        .route("/api/chat/stream", post(api::chat_stream))
        .route("/api/ai/upload", post(api::ai_upload))
        .route("/api/check-update", get(api::check_update))
        .route("/api/update-connectivity", get(api::check_update))
        .fallback(get(api_fallback))
        .layer(cors)
        .with_state(state);

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    log::info!("MeowCanvas 服务监听 http://127.0.0.1:{port}");

    // axum serve 需要独占 listener
    let _ = listener.set_nonblocking(true);
    let listener = tokio::net::TcpListener::from_std(listener)?;

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap_or_else(|e| {
            log::error!("HTTP 服务退出: {e}");
        });
    });

    Ok(Server { port })
}

/// fallback: /api/* 开头的路径返回 404 JSON (让前端正确识别错误),
/// 其他路径返回 index.html (SPA 路由)
async fn api_fallback(req: axum::extract::Request) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "detail": format!("接口未实现: {}", path) })),
        ).into_response();
    }
    index_html().await
}

async fn index_html() -> Response {
    serve_asset("index.html").unwrap_or_else(|| {
        (StatusCode::NOT_FOUND, "index.html missing").into_response()
    })
}

async fn favicon() -> Response {
    serve_asset("images/logo.png").or_else(|| serve_asset("favicon.ico"))
        .unwrap_or_else(|| StatusCode::NO_CONTENT.into_response())
}

async fn static_handler(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let cleaned = path.trim_start_matches('/');
    serve_asset(cleaned)
        .or_else(|| serve_asset(&format!("{cleaned}/index.html")))
        .unwrap_or_else(|| {
            // 未找到资源返回 JSON 错误而非 index.html,避免 JS/CSS 被当作 HTML
            if cleaned.ends_with(".js") || cleaned.ends_with(".css") {
                (StatusCode::NOT_FOUND, Json(json!({ "detail": "resource not found", "path": cleaned })))
                    .into_response()
            } else {
                serve_asset("index.html").unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
            }
        })
}

fn serve_asset(path: &str) -> Option<Response> {
    let asset = Asset::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let body = asset.data;
    let resp = (
        [
            (header::CONTENT_TYPE, mime.as_ref().to_string()),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        body,
    );
    Some(resp.into_response())
}
