// 共享 HTTP 辅助函数与 AppState 类型别名。
// 各业务模块 (ai/comfy/runninghub/library) 共用,避免重复定义。
use crate::store::SharedStore;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::{json, Value};
use std::sync::Arc;

pub type AppState = Arc<SharedStore>;

/// 200 OK + JSON
pub fn ok(body: Value) -> Response {
    (StatusCode::OK, Json(body)).into_response()
}

/// 503 未配置 (统一占位响应)
pub fn not_configured(name: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "detail": format!("该功能 ({}) 尚未配置 API。请在 API 设置中添加供应商。", name) })),
    ).into_response()
}

/// 400 Bad Request + {detail}
pub fn bad(detail: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "detail": detail.into() }))).into_response()
}

/// 404 Not Found + {detail}
pub fn not_found(detail: impl Into<String>) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "detail": detail.into() }))).into_response()
}

/// 502 Bad Gateway + {detail}
pub fn bad_gateway(detail: impl Into<String>) -> Response {
    (StatusCode::BAD_GATEWAY, Json(json!({ "detail": detail.into() }))).into_response()
}

/// 从 provider Value 中提取 (api_key, base_url)。base_url 默认 OpenAI 官方。
pub fn provider_endpoint(p: &Value) -> (String, String) {
    let key = p.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let url = p.get("base_url").and_then(|v| v.as_str())
        .unwrap_or("https://api.openai.com/v1").to_string();
    (key, url)
}

/// 把上游 HTTP 响应转成我们的 {detail} 错误响应。
/// 优先读 detail/error/message 字段,回退到原文。
pub async fn upstream_error(status: reqwest::StatusCode, body_text: String) -> Response {
    let parsed: Value = serde_json::from_str(&body_text).unwrap_or(json!({}));
    let detail = parsed.get("detail").or_else(|| parsed.get("error"))
        .or_else(|| parsed.get("message"))
        .map(|v| {
            if let Some(s) = v.as_str() { s.to_string() }
            else { v.to_string() }
        })
        .unwrap_or_else(|| body_text.clone());
    (
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
        Json(json!({ "detail": format!("上游错误: {}", detail) })),
    ).into_response()
}
