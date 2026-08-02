// Axum 路由 + 嵌入式静态资源服务。前端静态文件在编译期通过 rust-embed 打包进二进制,
// 运行时无需额外文件,实现真正的单文件桌面应用。
use crate::api;
use crate::store::{SharedStore, Store};
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

#[derive(RustEmbed)]
#[folder = "frontend/"]
pub struct Asset;

pub struct Server {
    pub port: u16,
}

/// 启动嵌入式 HTTP 服务,返回监听端口
pub fn spawn_server(data_dir: &std::path::Path) -> anyhow::Result<Server> {
    let store = Store::open(data_dir)?;
    let uploads_dir = store.uploads_dir();
    let state: api::AppState = Arc::new(SharedStore(std::sync::Mutex::new(store)));

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // 文件上传/大请求体上限 200MB
    let limit = RequestBodyLimitLayer::new(200 * 1024 * 1024);

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
        .route("/api/canvases/:id/meta", get(crate::library::canvas_meta_get).post(crate::library::canvas_meta_update))
        .route("/api/canvases/:id/touch", post(crate::library::canvas_touch))
        .route("/api/canvases/:id/restore", post(crate::library::canvas_restore))
        // ===== 工作流 / ComfyUI =====
        .route("/api/workflows", get(crate::comfy::workflows_list).post(crate::comfy::workflow_create))
        .route("/api/workflows/:name", get(crate::comfy::workflow_get).delete(crate::comfy::workflow_delete))
        .route("/api/workflows/:name/config", axum::routing::put(crate::comfy::workflow_config_update))
        .route("/api/workflows/:name/run", post(crate::comfy::workflow_run))
        .route("/api/canvas-comfy-tasks", post(crate::comfy::canvas_comfy_tasks_create))
        .route("/api/canvas-comfy-tasks/:task_id", get(crate::comfy::canvas_comfy_task_poll))
        .route("/api/comfyui/instances", get(crate::comfy::comfy_instances_get).put(crate::comfy::comfy_instances_put))
        .route("/api/upload", post(crate::comfy::upload))
        .route("/api/view", get(crate::comfy::view_proxy))
        // ===== 队列 / WebSocket =====
        .route("/api/queue_status", get(api::queue_status))
        .route("/ws/stats", get(api::ws_stats))
        // ===== 历史记录 =====
        .route("/api/history", get(crate::library::history_list))
        .route("/api/history/delete", post(crate::library::history_delete))
        // ===== 素材库 asset-library =====
        .route("/api/asset-library", get(crate::library::asset_library_get).patch(crate::library::asset_library_rename))
        .route("/api/asset-library/libraries", post(crate::library::asset_library_create_library))
        .route("/api/asset-library/libraries/:id", axum::routing::patch(crate::library::asset_library_rename_library).delete(crate::library::asset_library_delete_library))
        .route("/api/asset-library/categories", post(crate::library::asset_library_create_category))
        .route("/api/asset-library/categories/:id", axum::routing::patch(crate::library::asset_library_rename_category).delete(crate::library::asset_library_delete_category))
        .route("/api/asset-library/items", post(crate::library::asset_library_create_item))
        .route("/api/asset-library/items/batch", post(crate::library::asset_library_create_items_batch))
        .route("/api/asset-library/items/delete", post(crate::library::asset_library_delete_items))
        .route("/api/asset-library/items/move", post(crate::library::asset_library_move_items))
        .route("/api/asset-library/items/classify", post(crate::library::asset_library_classify_items))
        .route("/api/asset-library/items/workflows/upload", post(crate::library::asset_library_workflow_upload))
        .route("/api/asset-library/items/:id", axum::routing::patch(crate::library::asset_library_rename_item).delete(crate::library::asset_library_delete_item))
        .route("/api/asset-library/items/:id/register-avatar", post(crate::library::asset_library_register_avatar))
        .route("/api/asset-library/items/:id/avatar-status", post(crate::library::asset_library_avatar_status))
        .route("/api/canvas-assets/check", post(api::canvas_assets_check))
        .route("/api/canvas-assets/download", get(api::canvas_assets_download).post(api::canvas_assets_download))
        // ===== 提示词库 prompt-libraries =====
        .route("/api/prompt-libraries", get(crate::library::prompt_library_get).post(crate::library::prompt_library_create))
        .route("/api/prompt-libraries/items", post(crate::library::prompt_library_create_item))
        .route("/api/prompt-libraries/items/delete", post(crate::library::prompt_library_delete_items))
        .route("/api/prompt-libraries/items/:id", axum::routing::patch(crate::library::prompt_library_update_item).delete(crate::library::prompt_library_delete_item))
        .route("/api/prompt-libraries/categories", post(crate::library::prompt_library_create_category))
        .route("/api/prompt-libraries/categories/:id", axum::routing::patch(crate::library::prompt_library_rename_category).delete(crate::library::prompt_library_delete_category))
        .route("/api/prompt-libraries/:id", axum::routing::patch(crate::library::prompt_library_rename).delete(crate::library::prompt_library_delete))
        // ===== AI 生成 =====
        .route("/api/generate", post(crate::ai::generate))
        .route("/api/online-image", post(crate::ai::online_image))
        .route("/api/canvas-image-tasks", post(crate::ai::canvas_image_tasks_create))
        .route("/api/canvas-image-tasks/:id", get(crate::ai::canvas_image_tasks_get))
        .route("/api/canvas-video", post(crate::ai::canvas_video))
        .route("/api/jimeng/query-media", post(crate::ai::jimeng_query_media))
        .route("/api/image-task-query", post(crate::ai::image_task_query))
        .route("/api/angle/generate", post(crate::library::angle_generate))
        .route("/api/angle/poll_status", post(crate::library::angle_poll_status))
        .route("/api/ms/generate", post(crate::library::ms_generate))
        // ===== Providers =====
        .route("/api/providers", get(api::providers_get).put(api::providers_update))
        .route("/api/providers/test-connection", post(crate::ai::providers_test_connection))
        .route("/api/providers/fetch-models", post(crate::ai::providers_fetch_models))
        .route("/api/providers/probe-async", post(crate::ai::providers_test_connection))
        // ===== LLM 对话 =====
        .route("/api/conversations", get(crate::ai::conversations_list).post(crate::ai::conversations_create))
        .route("/api/conversations/:id", get(crate::ai::conversation_get).delete(crate::ai::conversation_delete))
        .route("/api/chat", post(crate::ai::chat))
        .route("/api/chat/agent", post(crate::ai::chat))
        .route("/api/chat/stream", post(crate::ai::chat_stream))
        .route("/api/canvas-llm", post(crate::ai::canvas_llm))
        .route("/api/ai/upload", post(crate::ai::ai_upload))
        // ===== RunningHub =====
        .route("/api/runninghub/app-info", get(crate::runninghub::app_info))
        .route("/api/runninghub/workflow-info", get(crate::runninghub::workflow_info))
        .route("/api/runninghub/workflows/fetch", post(crate::runninghub::fetch_workflow))
        .route("/api/runninghub/workflows/:workflowId", get(crate::runninghub::get_workflow).put(crate::runninghub::update_workflow).delete(crate::runninghub::delete_workflow))
        .route("/api/runninghub/upload-asset", post(crate::runninghub::upload_asset))
        .route("/api/runninghub/submit", post(crate::runninghub::submit))
        .route("/api/runninghub/workflow-submit", post(crate::runninghub::workflow_submit))
        .route("/api/runninghub/query", get(crate::runninghub::query))
        // ===== 更新检查 =====
        .route("/api/check-update", get(api::check_update))
        .route("/api/update-connectivity", get(api::check_update))
        // ===== 上传文件静态服务 (/uploads/* 指向 data/uploads 目录) =====
        .route("/uploads/*path", get(uploads_handler))
        .fallback(get(api_fallback))
        .layer(limit)
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

    // 保留 uploads_dir 引用避免 unused 警告
    let _ = uploads_dir;

    Ok(Server { port })
}

/// fallback: /api/* 开头的路径返回 404 JSON (让前端正确识别错误),
/// 其他路径返回 index.html (SPA 路由)
async fn api_fallback(req: Request) -> Response {
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

/// 上传文件静态服务:从 data/uploads 目录读取
async fn uploads_handler(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    let cleaned = path.trim_start_matches('/');
    // 防止路径穿越
    if cleaned.contains("..") {
        return (StatusCode::BAD_REQUEST, "invalid path").into_response();
    }
    // 从 app_state 拿 data_dir
    // 这里用全局静态不方便,改用环境变量回退:实际上 uploads 在 exe 同级 data/uploads
    let uploads_dir = resolve_uploads_dir();
    let file_path = uploads_dir.join(cleaned);
    match tokio::fs::read(&file_path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&file_path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.as_ref().to_string()),
                 (header::CACHE_CONTROL, "max-age=3600".to_string())],
                Body::from(bytes),
            ).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "detail": "file not found" }))).into_response(),
    }
}

/// 解析 uploads 目录:与 main.rs 的 data_dir 逻辑一致
fn resolve_uploads_dir() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.join("data").join("uploads");
        }
    }
    std::path::PathBuf::from("data").join("uploads")
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
