// AI 生成相关后端处理器: LLM 对话 / 流式对话 / 画布 LLM 重写 / 会话管理 /
// 文件上传 / 同步文生图 / 异步图片任务 / 视频生成 / 即梦轮询 / ComfyUI 占位等。
// 所有错误响应统一为 {detail:"..."} 格式,前端优先读取 detail 字段。
use crate::http_util::*;
use axum::extract::{Multipart, Path, State};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex as TokioMutex;

// ===== 画布图像异步任务内存存储 (our_task_id -> ePhone 任务信息) =====
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

/// SSE 事件流的项目类型 (永远发送 Ok, 错误用 error 事件承载)
type SseItem = Result<Event, std::io::Error>;

// ===== 通用辅助 =====

/// 解析 chat 请求的 provider / model / endpoint。
/// 返回 (api_key, chat_url, model)。provider 缺失或无 api_key 时返回 bad。
fn resolve_chat_provider(state: &AppState, body: &Value) -> Result<(String, String, String), Response> {
    let provider_id = body.get("provider").and_then(|v| v.as_str()).unwrap_or("");
    let is_modelscope = provider_id == "modelscope";
    let (api_key, base_url) = {
        let s = state.0.lock().unwrap();
        match s.get_provider(provider_id) {
            Some(p) => provider_endpoint(&p),
            None => return Err(bad("LLM provider 未配置或缺少 API Key")),
        }
    };
    if api_key.is_empty() {
        return Err(bad("LLM provider 未配置或缺少 API Key"));
    }
    let model = if is_modelscope {
        body.get("ms_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| body.get("model").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string()
    } else {
        body.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string()
    };
    let chat_url = if is_modelscope {
        // ModelScope 走官方推理端点,覆盖 provider 配置的 base_url
        "https://api-inference.modelscope.cn/v1/chat/completions".to_string()
    } else {
        format!("{}/chat/completions", base_url.trim_end_matches('/'))
    };
    Ok((api_key, chat_url, model))
}

/// 组装 OpenAI chat messages: [system?] + 已有历史 + [user message]
fn build_chat_messages(system_prompt: &str, history: &[Value], user_message: &str) -> Vec<Value> {
    let mut msgs = Vec::new();
    if !system_prompt.is_empty() {
        msgs.push(json!({ "role": "system", "content": system_prompt }));
    }
    for m in history {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        if role == "system" {
            continue; // 避免与上面注入的 system 重复
        }
        let content = m.get("content").and_then(|v| v.as_str()).unwrap_or("");
        msgs.push(json!({ "role": role, "content": content }));
    }
    msgs.push(json!({ "role": "user", "content": user_message }));
    msgs
}

/// 取得或新建会话。conversation_id 为空或查不到时按 fallback_title 新建。
fn get_or_create_conversation(state: &AppState, conversation_id: &str, fallback_title: &str) -> Value {
    let mut s = state.0.lock().unwrap();
    if !conversation_id.is_empty() {
        if let Some(c) = s.get_conversation(conversation_id) {
            return c;
        }
    }
    let title = if fallback_title.is_empty() { "新对话" } else { fallback_title };
    s.create_conversation(title)
}

/// 把用户消息和助手回复追加到会话并持久化,返回更新后的会话。
fn append_and_save(state: &AppState, conv: &Value, conv_id: &str, user_message: &str, reference_images: Value, assistant_content: &str) -> Value {
    let mut updated = conv.clone();
    if let Some(msgs) = updated.get_mut("messages").and_then(|v| v.as_array_mut()) {
        let mut user_msg = json!({ "role": "user", "content": user_message });
        if let Some(obj) = user_msg.as_object_mut() {
            obj.insert("attachments".into(), reference_images);
        }
        msgs.push(user_msg);
        msgs.push(json!({ "role": "assistant", "content": assistant_content }));
    }
    let mut s = state.0.lock().unwrap();
    match s.update_conversation(conv_id, updated) {
        Some(c) => c,
        None => conv.clone(),
    }
}

/// 查询 ePhone 异步任务,返回原始响应 JSON (或错误字符串)。
async fn fetch_ephone_task(api_key: &str, base_url: &str, ephone_task_id: &str) -> Result<Value, String> {
    let poll_url = format!("{}/task/{}", base_url.trim_end_matches('/'), ephone_task_id);
    let client = reqwest::Client::new();
    let resp = client
        .get(&poll_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
        .map_err(|e| format!("查询任务状态失败: {e}"))?;
    let body_text = resp.text().await.unwrap_or_default();
    let body_json: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| json!({}));
    Ok(body_json)
}

/// 从 ePhone 任务响应中提取 outputs URL 列表。
fn ephone_output_urls(body: &Value) -> Vec<String> {
    body.get("outputs")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

/// 把 tokio 无界 Receiver 转成 Stream (供 Sse 使用)。
fn rx_to_stream<T: Send + 'static>(rx: tokio::sync::mpsc::UnboundedReceiver<T>) -> impl Stream<Item = T> + Send + 'static {
    futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|v| (v, rx))
    })
}

/// 解析一段上游 SSE 块,把 delta 转发给客户端。[DONE] 表示上游结束。
fn process_sse_block(block: &str, tx: &tokio::sync::mpsc::UnboundedSender<SseItem>, full_text: &mut String) {
    for line in block.lines() {
        let line = line.trim();
        let data = match line.strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue,
        };
        if data == "[DONE]" {
            return;
        }
        let parsed: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let delta = parsed
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|v| v.as_str());
        if let Some(d) = delta {
            if !d.is_empty() {
                full_text.push_str(d);
                let _ = tx.send(Ok(Event::default().data(json!({ "type": "delta", "delta": d }).to_string())));
            }
        }
    }
}

/// 从文件名提取小写扩展名 (含点),无扩展名或过长则返回空。
fn ext_of(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() && ext.len() <= 8 => format!(".{}", ext.to_lowercase()),
        _ => String::new(),
    }
}

/// 根据 content_type / 文件名判断资源类型。
fn kind_of(content_type: &str, filename: &str) -> &'static str {
    let ct = content_type.to_lowercase();
    if ct.starts_with("image/") {
        return "image";
    }
    if ct.starts_with("video/") {
        return "video";
    }
    if ct.starts_with("audio/") {
        return "audio";
    }
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "avif" => "image",
        "mp4" | "webm" | "mov" | "mkv" => "video",
        "mp3" | "wav" | "m4a" | "aac" | "ogg" | "flac" => "audio",
        _ => "image",
    }
}

// ===== 1 & 2. LLM 对话 /api/chat (POST) — 非流式 (agent 模式复用同一处理器) =====

pub async fn chat(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let conversation_id = body.get("conversation_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let system_prompt = body.get("system_prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let reference_images = body.get("reference_images").cloned().unwrap_or(json!([]));

    let (api_key, chat_url, model) = match resolve_chat_provider(&state, &body) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let title_30: String = message.chars().take(30).collect();
    let conv = get_or_create_conversation(&state, &conversation_id, &title_30);
    let conv_id = conv.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let history: Vec<Value> = conv.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let messages = build_chat_messages(&system_prompt, &history, &message);

    let req_body = json!({ "model": model, "messages": messages, "stream": false });
    let client = reqwest::Client::new();
    let resp = match client
        .post(&chat_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&req_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return bad_gateway(format!("调用 LLM 失败: {e}")),
    };
    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return upstream_error(st, txt).await;
    }
    let resp_json: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("解析 LLM 响应失败: {e}")),
    };
    let content = resp_json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let saved = append_and_save(&state, &conv, &conv_id, &message, reference_images, &content);
    let title = saved.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let messages_out = saved.get("messages").cloned().unwrap_or(json!([]));
    ok(json!({ "conversation": { "id": conv_id, "title": title, "messages": messages_out } }))
}

// ===== 3. /api/chat/stream (POST) — SSE 流式 =====

pub async fn chat_stream(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let conversation_id = body.get("conversation_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let system_prompt = body.get("system_prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let reference_images = body.get("reference_images").cloned().unwrap_or(json!([]));

    let (api_key, chat_url, model) = match resolve_chat_provider(&state, &body) {
        Ok(v) => v,
        Err(r) => return r, // JSON 错误,非 SSE
    };

    let title_30: String = message.chars().take(30).collect();
    let conv = get_or_create_conversation(&state, &conversation_id, &title_30);
    let conv_id = conv.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let history: Vec<Value> = conv.get("messages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let messages = build_chat_messages(&system_prompt, &history, &message);

    let req_body = json!({ "model": model, "messages": messages, "stream": true });
    let client = reqwest::Client::new();
    let resp = match client
        .post(&chat_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&req_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return bad_gateway(format!("调用 LLM 失败: {e}")),
    };
    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return upstream_error(st, txt).await;
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<SseItem>();
    let state_clone = state.clone();

    tokio::spawn(async move {
        // 1. meta 事件: 回传当前会话 (含已有历史, 不含本次新消息)
        let meta = json!({
            "type": "meta",
            "conversation": {
                "id": conv.get("id").cloned().unwrap_or(json!(null)),
                "title": conv.get("title").cloned().unwrap_or(json!("")),
                "messages": conv.get("messages").cloned().unwrap_or(json!([])),
            }
        });
        let _ = tx.send(Ok(Event::default().data(meta.to_string())));

        // 2. 逐块读取上游 SSE 响应并解析 (chunk() 无需 reqwest stream feature)
        let mut resp = resp;
        let mut buffer = String::new();
        let mut full_text = String::new();
        let mut errored = false;
        loop {
            let chunk = match resp.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    let _ = tx.send(Ok(Event::default().data(
                        json!({ "type": "error", "detail": format!("上游流读取失败: {e}") }).to_string(),
                    )));
                    errored = true;
                    break;
                }
            };
            buffer.push_str(std::str::from_utf8(&chunk).unwrap_or(""));
            // SSE 事件以空行分隔
            loop {
                if let Some(idx) = buffer.find("\n\n") {
                    let event_block = buffer[..idx].to_string();
                    buffer = buffer[idx + 2..].to_string();
                    process_sse_block(&event_block, &tx, &mut full_text);
                } else {
                    break;
                }
            }
        }
        if !buffer.trim().is_empty() {
            process_sse_block(&buffer, &tx, &mut full_text);
        }
        if errored {
            return;
        }

        // 3. done 事件: 追加 user + assistant 消息并持久化,回传完整会话
        let saved = append_and_save(&state_clone, &conv, &conv_id, &message, reference_images, &full_text);
        let done = json!({
            "type": "done",
            "conversation": {
                "id": saved.get("id").cloned().unwrap_or(json!(null)),
                "title": saved.get("title").cloned().unwrap_or(json!("")),
                "messages": saved.get("messages").cloned().unwrap_or(json!([])),
            }
        });
        let _ = tx.send(Ok(Event::default().data(done.to_string())));
    });

    Sse::new(rx_to_stream(rx)).into_response()
}

// ===== 4. /api/canvas-llm (POST) — 画布 LLM 节点重写 (单轮无历史) =====

pub async fn canvas_llm(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let message = body.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let system_prompt = body.get("system_prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let (api_key, chat_url, model) = match resolve_chat_provider(&state, &body) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let mut messages = Vec::new();
    if !system_prompt.is_empty() {
        messages.push(json!({ "role": "system", "content": system_prompt }));
    }
    messages.push(json!({ "role": "user", "content": message }));

    let req_body = json!({ "model": model, "messages": messages, "stream": false });
    let client = reqwest::Client::new();
    let resp = match client
        .post(&chat_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&req_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return bad_gateway(format!("调用 LLM 失败: {e}")),
    };
    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return upstream_error(st, txt).await;
    }
    let resp_json: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("解析 LLM 响应失败: {e}")),
    };
    let content = resp_json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    ok(json!({ "text": content }))
}

// ===== 5. /api/conversations (GET) — 会话列表 =====

pub async fn conversations_list(State(state): State<AppState>) -> Response {
    let convs = state.0.lock().unwrap().list_conversations();
    let out: Vec<Value> = convs
        .iter()
        .map(|c| {
            let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let title = c.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let last_message = c
                .get("messages")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.last())
                .and_then(|m| m.get("content"))
                .and_then(|v| v.as_str())
                .map(|s| s.chars().take(100).collect::<String>())
                .unwrap_or_default();
            json!({ "id": id, "title": title, "last_message": last_message })
        })
        .collect();
    ok(json!({ "conversations": out }))
}

// ===== 6. /api/conversations (POST) — 创建会话 =====

pub async fn conversations_create(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("新对话");
    let conv = state.0.lock().unwrap().create_conversation(title);
    ok(json!({ "conversation": conv }))
}

// ===== 7. /api/conversations/:id (GET) — 获取单会话 =====

pub async fn conversation_get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.0.lock().unwrap().get_conversation(&id) {
        Some(c) => ok(json!({ "conversation": c })),
        None => not_found("会话不存在"),
    }
}

// ===== 8. /api/conversations/:id (DELETE) — 删除会话 =====

pub async fn conversation_delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let done = state.0.lock().unwrap().delete_conversation(&id);
    ok(json!({ "success": done }))
}

// ===== 9. /api/ai/upload (POST multipart) — 文件上传 =====

pub async fn ai_upload(State(state): State<AppState>, mut multipart: Multipart) -> Response {
    let dir = state.0.lock().unwrap().uploads_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut files = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name().unwrap_or("") != "files" {
            continue;
        }
        let original = field.file_name().unwrap_or("file").to_string();
        let content_type = field.content_type().unwrap_or("application/octet-stream").to_string();
        let bytes = match field.bytes().await {
            Ok(b) => b,
            Err(_) => continue,
        };
        let ext = ext_of(&original);
        let filename = format!("{}{}", uuid::Uuid::new_v4(), ext);
        let path = dir.join(&filename);
        if std::fs::write(&path, &bytes).is_err() {
            continue;
        }
        let kind = kind_of(&content_type, &original);
        files.push(json!({ "url": format!("/uploads/{}", filename), "name": original, "kind": kind }));
    }
    ok(json!({ "files": files }))
}

// ===== 10. /api/online-image (POST) — 同步文生图 (OpenAI images.generations) =====

pub async fn online_image(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let provider_id = body.get("provider_id").and_then(|v| v.as_str()).unwrap_or("");
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let model = body.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("gpt-image-1");
    let size = body.get("size").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("1024x1024");
    let quality = body.get("quality").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && *s != "auto");

    let (api_key, base_url) = {
        let s = state.0.lock().unwrap();
        match s.get_provider(provider_id) {
            Some(p) => provider_endpoint(&p),
            None => return bad("未找到图像 provider"),
        }
    };
    if api_key.is_empty() {
        return bad("图像 provider 未配置 API Key");
    }

    let mut req = json!({ "model": model, "prompt": prompt, "n": 1, "size": size });
    if let Some(q) = quality {
        req["quality"] = json!(q);
    }
    let url = format!("{}/images/generations", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return bad_gateway(format!("调用图像 API 失败: {e}")),
    };
    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return upstream_error(st, txt).await;
    }
    let resp_json: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("解析图像响应失败: {e}")),
    };
    let images: Vec<String> = resp_json
        .get("data")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    if let Some(u) = item.get("url").and_then(|v| v.as_str()) {
                        Some(u.to_string())
                    } else if let Some(b64) = item.get("b64_json").and_then(|v| v.as_str()) {
                        Some(format!("data:image/png;base64,{}", b64))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    ok(json!({ "images": images, "task_id": null, "provider_id": provider_id, "backend": "openai" }))
}

// ===== 11. /api/canvas-image-tasks (POST) — 异步图片任务 (ePhone) =====

pub async fn canvas_image_tasks_create(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let provider_id = body.get("provider_id").and_then(|v| v.as_str()).unwrap_or("");
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let model = body.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("gpt-image-2-dev");
    let size = body.get("size").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && *s != "auto");
    let quality = body.get("quality").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && *s != "auto");
    let n = body.get("n").and_then(|v| v.as_i64()).filter(|&x| x > 0);
    let operation = body.get("operation").and_then(|v| v.as_str()).unwrap_or("");
    let resolution_type = body.get("resolution_type").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("2x");

    if prompt.is_empty() && operation.is_empty() {
        return bad("prompt 不能为空");
    }

    let (api_key, base_url) = {
        let s = state.0.lock().unwrap();
        match s.get_provider(provider_id) {
            Some(p) => {
                let key = p.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let url = p.get("base_url").and_then(|v| v.as_str()).unwrap_or("https://api.ephone.ai/v1").to_string();
                (key, url)
            }
            None => return bad(format!("未找到 provider: {}", provider_id)),
        }
    };
    if api_key.is_empty() {
        return bad("该 provider 未配置 API Key");
    }

    let mut input = json!({});
    if !prompt.is_empty() {
        input["prompt"] = json!(prompt);
    }
    if let Some(sz) = size {
        input["size"] = json!(sz);
    }
    if let Some(q) = quality {
        input["quality"] = json!(q);
    }
    if let Some(nn) = n {
        input["n"] = json!(nn);
    }
    // 放大任务补 operation / resolution_type 字段
    if operation == "upscale" {
        input["operation"] = json!("upscale");
        input["resolution_type"] = json!(resolution_type);
    }

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
        Err(e) => return bad_gateway(format!("调用 ePhone API 失败: {e}")),
    };
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let body_json: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        let detail = body_json
            .get("error")
            .or_else(|| body_json.get("detail"))
            .and_then(|v| v.as_str())
            .unwrap_or(&body_text);
        return bad_gateway(format!("ePhone 提交失败: {}", detail));
    }
    let ephone_task_id = match body_json.get("id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return bad_gateway(format!("ePhone 返回缺少 task id: {}", body_text)),
    };

    let our_task_id = uuid::Uuid::new_v4().to_string();
    task_store().lock().await.insert(
        our_task_id.clone(),
        TaskInfo { ephone_task_id, api_key, base_url },
    );
    ok(json!({ "task_id": our_task_id }))
}

// ===== 12. /api/canvas-image-tasks/:id (GET) — 轮询异步任务 =====

pub async fn canvas_image_tasks_get(Path(task_id): Path<String>) -> Response {
    let info = {
        let store = task_store().lock().await;
        store.get(&task_id).cloned()
    };
    let info = match info {
        Some(i) => i,
        None => return not_found("任务不存在或已过期"),
    };
    let body_json = match fetch_ephone_task(&info.api_key, &info.base_url, &info.ephone_task_id).await {
        Ok(v) => v,
        Err(e) => return bad_gateway(e),
    };
    let status = body_json.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
    match status {
        "completed" => {
            let urls = ephone_output_urls(&body_json);
            let images: Vec<Value> = urls.iter().map(|u| json!({ "url": u })).collect();
            ok(json!({ "status": "succeeded", "result": { "images": images } }))
        }
        "failed" => {
            let error = body_json.get("error").and_then(|v| v.as_str()).unwrap_or("生成失败");
            // 失败时回传 upstream_task_id,便于前端恢复查询
            ok(json!({ "status": "failed", "error": error, "upstream_task_id": info.ephone_task_id }))
        }
        _ => ok(json!({ "status": status })),
    }
}

// ===== 13. /api/canvas-video (POST) — 视频生成 =====

pub async fn canvas_video(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let provider_id = body.get("provider_id").and_then(|v| v.as_str()).unwrap_or("");
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let model = body.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("text-to-video");
    let duration = body.get("duration").and_then(|v| v.as_i64()).unwrap_or(5);
    let aspect_ratio = body.get("aspect_ratio").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("16:9");

    let (api_key, base_url) = {
        let s = state.0.lock().unwrap();
        match s.get_provider(provider_id) {
            Some(p) => provider_endpoint(&p),
            None => return bad("未找到视频 provider"),
        }
    };
    if api_key.is_empty() {
        return bad("视频 provider 未配置 API Key");
    }

    let req = json!({ "model": model, "prompt": prompt, "n": 1, "duration": duration, "size": aspect_ratio });
    let url = format!("{}/videos/generations", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return bad_gateway(format!("调用视频 API 失败: {e}")),
    };
    if !resp.status().is_success() {
        let st = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return upstream_error(st, txt).await;
    }
    let resp_json: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return bad_gateway(format!("解析视频响应失败: {e}")),
    };

    // 优先解析同步返回的 url 列表
    let mut urls: Vec<String> = Vec::new();
    if let Some(data) = resp_json.get("data").and_then(|v| v.as_array()) {
        for item in data {
            if let Some(u) = item.get("url").and_then(|v| v.as_str()) {
                urls.push(u.to_string());
            }
        }
    }
    if let Some(u) = resp_json.get("url").and_then(|v| v.as_str()) {
        urls.push(u.to_string());
    }
    if let Some(arr) = resp_json.get("urls").and_then(|v| v.as_array()) {
        for item in arr {
            if let Some(s) = item.as_str() {
                urls.push(s.to_string());
            }
        }
    }
    if !urls.is_empty() {
        return ok(json!({ "urls": urls, "videos": urls }));
    }

    // 异步任务结构: 转成即梦排队信号让前端走轮询
    if let Some(task_id) = resp_json.get("task_id").and_then(|v| v.as_str()) {
        return ok(json!({
            "jimeng_pending": true,
            "submit_id": task_id,
            "kind": "video",
            "queue_info": {},
            "message": "视频生成排队中"
        }));
    }

    ok(json!({ "urls": [], "videos": [], "message": resp_json.to_string() }))
}

// ===== 14. /api/jimeng/query-media (POST) — 即梦异步轮询 =====

pub async fn jimeng_query_media(Json(body): Json<Value>) -> Response {
    let submit_id = body.get("submit_id").and_then(|v| v.as_str()).unwrap_or("");
    let _kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("image");
    if submit_id.is_empty() {
        return ok(json!({ "status": "failed", "urls": [], "error": "submit_id 不能为空" }));
    }
    let info = {
        let store = task_store().lock().await;
        store.get(submit_id).cloned()
    };
    let info = match info {
        Some(i) => i,
        None => return ok(json!({ "status": "failed", "urls": [], "error": "任务不存在或后端已重启" })),
    };
    let body_json = match fetch_ephone_task(&info.api_key, &info.base_url, &info.ephone_task_id).await {
        Ok(v) => v,
        Err(e) => return ok(json!({ "status": "failed", "urls": [], "error": e })),
    };
    let status = body_json.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
    match status {
        "completed" => {
            let urls = ephone_output_urls(&body_json);
            ok(json!({ "status": "succeeded", "urls": urls, "error": "" }))
        }
        "failed" => {
            let error = body_json.get("error").and_then(|v| v.as_str()).unwrap_or("生成失败");
            ok(json!({ "status": "failed", "urls": [], "error": error }))
        }
        _ => ok(json!({ "status": "running", "urls": [], "error": "" })),
    }
}

// ===== 15. /api/image-task-query (POST) — 失败任务恢复查询 =====

pub async fn image_task_query(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let provider_id = body.get("provider_id").and_then(|v| v.as_str()).unwrap_or("");
    let task_id = body.get("task_id").and_then(|v| v.as_str()).unwrap_or("");
    if task_id.is_empty() {
        return bad("task_id 不能为空");
    }
    let (api_key, base_url) = {
        let s = state.0.lock().unwrap();
        match s.get_provider(provider_id) {
            Some(p) => provider_endpoint(&p),
            None => return bad(format!("未找到 provider: {}", provider_id)),
        }
    };
    if api_key.is_empty() {
        return bad("provider 未配置 API Key");
    }
    // task_id 即 ePhone 上游任务 id
    let body_json = match fetch_ephone_task(&api_key, &base_url, &task_id).await {
        Ok(v) => v,
        Err(e) => return bad_gateway(e),
    };
    let status = body_json.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");
    match status {
        "completed" => {
            let urls = ephone_output_urls(&body_json);
            let image_items: Vec<Value> = urls.iter().map(|u| json!({ "url": u })).collect();
            ok(json!({
                "status": "succeeded",
                "urls": urls,
                "image_items": image_items,
                "error": "",
                "message": "生成完成"
            }))
        }
        "failed" => {
            let error = body_json.get("error").and_then(|v| v.as_str()).unwrap_or("生成失败");
            ok(json!({
                "status": "failed",
                "urls": [],
                "image_items": [],
                "error": error,
                "message": error,
                "upstream_task_id": task_id
            }))
        }
        _ => ok(json!({
            "status": "running",
            "urls": [],
            "image_items": [],
            "error": "",
            "message": "任务进行中"
        })),
    }
}

// ===== 16. /api/generate (POST) — ComfyUI 工作流直出 (占位) =====
// TODO: ComfyUI 代理逻辑 (查实例 -> 转发 /prompt -> 轮询 /history -> 取图)
// 较复杂,留给独立的 comfy 模块实现。这里返回未配置占位,确保端点存在不 404。
pub async fn generate(State(_state): State<AppState>, Json(_body): Json<Value>) -> Response {
    not_configured("ComfyUI 文生图 generate")
}

// ===== Providers 测试连接 / 拉取模型 =====

/// POST /api/providers/test-connection
/// 请求体: {base_url, api_key, provider_id, protocol, image_request_mode}
/// 响应: {ok, protocol, image_request_mode, status, message, model_count/total,
///        all, image_models, chat_models, video_models, model_names, raw}
pub async fn providers_test_connection(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let base_url = body.get("base_url").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let api_key = body.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let protocol = body.get("protocol").and_then(|v| v.as_str()).unwrap_or("openai").to_string();

    if base_url.is_empty() {
        return bad("base_url 不能为空");
    }
    if api_key.is_empty() {
        return ok(json!({ "ok": false, "message": "缺少 API Key", "protocol": protocol, "status": 401 }));
    }

    // OpenAI 兼容协议:GET {base_url}/models
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = match client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send().await
    {
        Ok(r) => r,
        Err(e) => {
            return ok(json!({
                "ok": false, "protocol": protocol, "status": 0,
                "message": format!("连接失败: {e}"), "model_count": 0, "total": 0,
                "all": [], "image_models": [], "chat_models": [], "video_models": [], "model_names": {}
            }));
        }
    };

    let status = resp.status().as_u16();
    let body_text = resp.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&body_text).unwrap_or(json!({}));

    if status >= 200 && status < 300 {
        // 解析 models 列表 (OpenAI 格式 {data:[{id}]} 或数组)
        let models: Vec<String> = parsed.get("data").and_then(|d| d.as_array())
            .map(|arr| arr.iter().filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from)).collect())
            .unwrap_or_else(|| {
                parsed.as_array().map(|arr| arr.iter().filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from)).collect())
                    .unwrap_or_default()
            });
        let total = models.len();
        // 简单分类: 含 image/dall/e/flux/sd/imagen 的算图像模型; 含 gpt/claude/llama/qwen/chat 的算对话模型; 含 video/veo/sora 的算视频
        let image_models: Vec<&String> = models.iter().filter(|m| {
            let l = m.to_lowercase();
            l.contains("image") || l.contains("dall") || l.contains("flux") || l.contains("-e-") || l.contains("sd") || l.contains("imagen") || l.contains("kolors") || l.contains("jimeng")
        }).collect();
        let chat_models: Vec<&String> = models.iter().filter(|m| {
            let l = m.to_lowercase();
            l.contains("gpt") || l.contains("claude") || l.contains("llama") || l.contains("qwen") || l.contains("chat") || l.contains("deepseek") || l.contains("gemini")
        }).collect();
        let video_models: Vec<&String> = models.iter().filter(|m| {
            let l = m.to_lowercase();
            l.contains("video") || l.contains("veo") || l.contains("sora") || l.contains("kling")
        }).collect();
        let model_names: Value = models.iter().map(|m| (m.clone(), json!(m))).collect();
        ok(json!({
            "ok": true, "protocol": protocol, "image_request_mode": body.get("image_request_mode").and_then(|v| v.as_str()).unwrap_or("openai"),
            "status": status, "status_code": status,
            "message": format!("✓ 找到 {} 个模型", total),
            "model_count": total, "total": total,
            "all": models, "image_models": image_models, "chat_models": chat_models, "video_models": video_models,
            "model_names": model_names, "raw": parsed
        }))
    } else {
        let detail = parsed.get("detail").or_else(|| parsed.get("error"))
            .or_else(|| parsed.get("message"))
            .map(|v| if let Some(s) = v.as_str() { s.to_string() } else { v.to_string() })
            .unwrap_or(body_text.clone());
        ok(json!({
            "ok": false, "protocol": protocol, "status": status, "status_code": status,
            "message": format!("⚠ 地址验证未通过 (HTTP {})", status),
            "model_count": 0, "total": 0,
            "all": [], "image_models": [], "chat_models": [], "video_models": [], "model_names": {},
            "raw": parsed, "detail": detail
        }))
    }
}

/// POST /api/providers/fetch-models
/// 请求体同 test-connection,响应结构也一致
pub async fn providers_fetch_models(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    // 直接复用 test-connection 逻辑
    providers_test_connection(State(state), Json(body)).await
}
