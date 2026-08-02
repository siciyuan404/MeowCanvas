// 素材库 / 提示词库 / 历史记录 / 画布元数据 / 角度图等辅助功能后端处理器。
// 共享 crate::http_util 中的 AppState 与辅助响应函数。
//
// 端点分组:
//   A. 画布元数据 (canvas meta / touch / restore)
//   B. 历史记录 (history list / delete)
//   C. 素材库 asset-library (libraries / categories / items / workflows 上传 / 数字人占位)
//   D. 提示词库 prompt-libraries (libraries / items / categories)
//   E. 角度图 angle (ModelScope 图像编辑 / 任务轮询)
//   F. Modelscope msgen (文生图 / 图生图)
use crate::http_util::{
    AppState, bad, bad_gateway, not_configured, not_found, ok, provider_endpoint, upstream_error,
};
use axum::extract::{Json, Multipart, Path, Query, State};
use axum::response::Response;
use serde_json::{json, Value};
use std::collections::HashMap;

/// 生成 uuid v4 字符串,作为新对象 id
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ============================================================
// A. 画布元数据
// ============================================================

/// GET /api/canvases/:id/meta - 取画布元数据
pub async fn canvas_meta_get(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let s = state.0.lock().unwrap();
    match s.get_canvas(&id) {
        Some(c) => {
            let meta = json!({
                "id": c.get("id").cloned().unwrap_or(Value::Null),
                "title": c.get("title").cloned().unwrap_or(Value::Null),
                "kind": c.get("kind").cloned().unwrap_or(Value::Null),
                "icon": c.get("icon").cloned().unwrap_or(Value::Null),
                "color": c.get("color").cloned().unwrap_or(Value::Null),
                "owner": c.get("owner").cloned().unwrap_or(Value::Null),
                "pinned": c.get("pinned").cloned().unwrap_or(Value::Null),
                "project": c.get("project").cloned().unwrap_or(Value::Null),
                "updated_at": c.get("updated_at").cloned().unwrap_or(Value::Null),
                "created_at": c.get("created_at").cloned().unwrap_or(Value::Null),
            });
            ok(json!({ "canvas": meta }))
        }
        None => not_found("画布不存在"),
    }
}

/// POST /api/canvases/:id/meta - 更新画布元数据 (patch)
pub async fn canvas_meta_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let mut s = state.0.lock().unwrap();
    match s.update_canvas_meta(&id, body) {
        Some(c) => ok(json!({ "canvas": c })),
        None => not_found("画布不存在"),
    }
}

/// POST /api/canvases/:id/touch - 刷新访问时间
pub async fn canvas_touch(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut s = state.0.lock().unwrap();
    match s.touch_canvas(&id) {
        Some(c) => ok(json!({ "canvas": c })),
        None => not_found("画布不存在"),
    }
}

/// POST /api/canvases/:id/restore - 从回收站恢复画布
pub async fn canvas_restore(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let mut s = state.0.lock().unwrap();
    let done = s.restore_canvas(&id);
    ok(json!({ "success": done }))
}

// ============================================================
// B. 历史记录
// ============================================================

/// GET /api/history?type=xxx - 返回裸数组 (前端期望直接 JSON 数组)
pub async fn history_list(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let t = q.get("type").cloned().unwrap_or_default();
    if t.is_empty() {
        return ok(json!([]));
    }
    let s = state.0.lock().unwrap();
    let list = s.list_history(&t);
    // 直接以 JSON 数组响应,不包裹 {history:[]}
    ok(Value::Array(list))
}

/// POST /api/history/delete?type=xxx - 删除指定时间戳的历史记录
/// type 从 query 取,timestamp 从 body 取
pub async fn history_delete(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Response {
    let t = q.get("type").cloned().unwrap_or_default();
    let timestamp = body
        .get("timestamp")
        .map(|v| match v {
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            _ => String::new(),
        })
        .unwrap_or_default();
    if timestamp.is_empty() {
        return bad("缺少 timestamp");
    }
    let mut s = state.0.lock().unwrap();
    let done = s.delete_history(&t, &timestamp);
    ok(json!({ "success": done }))
}

// ============================================================
// C. 素材库 asset-library
// ============================================================

/// GET /api/asset-library - 返回整个素材库
pub async fn asset_library_get(State(state): State<AppState>) -> Response {
    let s = state.0.lock().unwrap();
    ok(json!({ "library": s.get_asset_library() }))
}

/// PATCH /api/asset-library - 整体重命名占位 (实际重命名走 /libraries/:id)
pub async fn asset_library_rename(
    State(state): State<AppState>,
    Json(_body): Json<Value>,
) -> Response {
    let s = state.0.lock().unwrap();
    ok(json!({ "library": s.get_asset_library() }))
}

/// POST /api/asset-library/libraries - 创建资产库
pub async fn asset_library_create_library(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let id = new_id();
    let new_lib = json!({
        "id": id,
        "name": name,
        "readonly": false,
        "categories": [],
    });
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        arr.push(new_lib);
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib, "asset_library": { "id": id } }))
}

/// PATCH /api/asset-library/libraries/:id - 重命名资产库
pub async fn asset_library_rename_library(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if l.get("id").and_then(|v| v.as_str()) == Some(&id) {
                l["name"] = json!(name);
                found = true;
                break;
            }
        }
    }
    if !found {
        return not_found("资产库不存在");
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// DELETE /api/asset-library/libraries/:id - 删除资产库
/// 若 active_library_id==该 id 则置 null
pub async fn asset_library_delete_library(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        arr.retain(|l| l.get("id").and_then(|v| v.as_str()) != Some(&id));
    }
    // 若 active_library_id == 该 id,置 null
    let active = lib.get("active_library_id").and_then(|v| v.as_str());
    if active == Some(id.as_str()) {
        lib["active_library_id"] = Value::Null;
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// POST /api/asset-library/categories - 创建分类
pub async fn asset_library_create_category(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let library_id = body.get("library_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let cat_type = body.get("type").and_then(|v| v.as_str()).unwrap_or("image").to_string();
    if library_id.is_empty() {
        return bad("缺少 library_id");
    }
    let id = new_id();
    let new_cat = json!({
        "id": id,
        "name": name,
        "type": cat_type,
        "items": [],
    });
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if l.get("id").and_then(|v| v.as_str()) == Some(&library_id) {
                if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                    cats.push(new_cat);
                }
                found = true;
                break;
            }
        }
    }
    if !found {
        return not_found("资产库不存在");
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib, "category": { "id": id } }))
}

/// PATCH /api/asset-library/categories/:id - 重命名分类
/// 请求体 {name, library_id?}: library_id 为空时遍历所有 library
pub async fn asset_library_rename_category(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let library_id = body.get("library_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            let lib_match = l.get("id").and_then(|v| v.as_str()) == Some(&library_id);
            if library_id.is_empty() || lib_match {
                if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                    for c in cats.iter_mut() {
                        if c.get("id").and_then(|v| v.as_str()) == Some(&id) {
                            c["name"] = json!(name);
                            found = true;
                            break;
                        }
                    }
                }
                if found {
                    break;
                }
            }
        }
    }
    if !found {
        return not_found("分类不存在");
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// DELETE /api/asset-library/categories/:id?library_id=xxx - 删除分类
pub async fn asset_library_delete_category(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let library_id = q.get("library_id").cloned().unwrap_or_default();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            let lib_match = l.get("id").and_then(|v| v.as_str()) == Some(&library_id);
            if library_id.is_empty() || lib_match {
                if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                    let before = cats.len();
                    cats.retain(|c| c.get("id").and_then(|v| v.as_str()) != Some(&id));
                    if cats.len() != before {
                        found = true;
                    }
                }
                if found {
                    break;
                }
            }
        }
    }
    if !found {
        return not_found("分类不存在");
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// POST /api/asset-library/items - 创建单个素材
pub async fn asset_library_create_item(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let library_id = body.get("library_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let category_id = body.get("category_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if library_id.is_empty() || category_id.is_empty() {
        return bad("缺少 library_id 或 category_id");
    }
    let id = new_id();
    let new_item = json!({
        "id": id,
        "name": name,
        "url": url,
        "category_id": category_id,
        "registrations": {},
    });
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if l.get("id").and_then(|v| v.as_str()) == Some(&library_id) {
                if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                    for c in cats.iter_mut() {
                        if c.get("id").and_then(|v| v.as_str()) == Some(&category_id) {
                            if let Some(items) = c.get_mut("items").and_then(|v| v.as_array_mut()) {
                                items.push(new_item);
                            }
                            found = true;
                            break;
                        }
                    }
                }
                break;
            }
        }
    }
    if !found {
        return not_found("分类或资产库不存在");
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib, "item": { "id": id } }))
}

/// POST /api/asset-library/items/batch - 批量创建素材
/// 全部追加到 body 指定的 library_id/category_id 下
pub async fn asset_library_create_items_batch(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let library_id = body.get("library_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let category_id = body.get("category_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let items_in = body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if library_id.is_empty() || category_id.is_empty() {
        return bad("缺少 library_id 或 category_id");
    }
    // 预先生成所有新 item (统一绑定到 body 级 category_id)
    let mut ids: Vec<Value> = Vec::new();
    let mut new_items: Vec<Value> = Vec::new();
    for it in items_in.iter() {
        let url = it.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let name = it.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let id = new_id();
        ids.push(json!({ "id": id }));
        new_items.push(json!({
            "id": id,
            "name": name,
            "url": url,
            "category_id": category_id,
            "registrations": {},
        }));
    }
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if l.get("id").and_then(|v| v.as_str()) == Some(&library_id) {
                if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                    for c in cats.iter_mut() {
                        if c.get("id").and_then(|v| v.as_str()) == Some(&category_id) {
                            if let Some(items) = c.get_mut("items").and_then(|v| v.as_array_mut()) {
                                for ni in &new_items {
                                    items.push(ni.clone());
                                }
                            }
                            found = true;
                            break;
                        }
                    }
                }
                break;
            }
        }
    }
    if !found {
        return not_found("分类或资产库不存在");
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib, "items": ids }))
}

/// PATCH /api/asset-library/items/:id - 重命名单个素材
pub async fn asset_library_rename_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                for c in cats.iter_mut() {
                    if let Some(items) = c.get_mut("items").and_then(|v| v.as_array_mut()) {
                        for it in items.iter_mut() {
                            if it.get("id").and_then(|v| v.as_str()) == Some(&id) {
                                it["name"] = json!(name);
                                found = true;
                                break;
                            }
                        }
                    }
                    if found {
                        break;
                    }
                }
            }
            if found {
                break;
            }
        }
    }
    if !found {
        return not_found("素材不存在");
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// DELETE /api/asset-library/items/:id - 删除单个素材
pub async fn asset_library_delete_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                for c in cats.iter_mut() {
                    if let Some(items) = c.get_mut("items").and_then(|v| v.as_array_mut()) {
                        let before = items.len();
                        items.retain(|it| it.get("id").and_then(|v| v.as_str()) != Some(&id));
                        if items.len() != before {
                            found = true;
                        }
                    }
                    if found {
                        break;
                    }
                }
            }
            if found {
                break;
            }
        }
    }
    if !found {
        return not_found("素材不存在");
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// POST /api/asset-library/items/delete - 批量删除素材
pub async fn asset_library_delete_items(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let ids: Vec<String> = body
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    if ids.is_empty() {
        return ok(json!({ "library": lib, "removed": 0 }));
    }
    let mut removed = 0usize;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                for c in cats.iter_mut() {
                    if let Some(items) = c.get_mut("items").and_then(|v| v.as_array_mut()) {
                        let before = items.len();
                        items.retain(|it| {
                            let iid = it.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            !ids.iter().any(|x| x == iid)
                        });
                        removed += before - items.len();
                    }
                }
            }
        }
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib, "removed": removed }))
}

/// POST /api/asset-library/items/move - 移动素材到目标 library/category
pub async fn asset_library_move_items(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let target_library_id = body
        .get("target_library_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let target_category_id = body
        .get("target_category_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ids: Vec<String> = body
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if target_library_id.is_empty() || target_category_id.is_empty() {
        return bad("缺少 target_library_id 或 target_category_id");
    }
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    // 1. 从所有 library 的所有 category 中收集并移除匹配的 item
    let mut moved_items: Vec<Value> = Vec::new();
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                for c in cats.iter_mut() {
                    if let Some(items) = c.get_mut("items").and_then(|v| v.as_array_mut()) {
                        let mut taken: Vec<Value> = Vec::new();
                        items.retain(|it| {
                            let iid = it.get("id").and_then(|v| v.as_str()).unwrap_or("");
                            if ids.iter().any(|x| x == iid) {
                                taken.push(it.clone());
                                false
                            } else {
                                true
                            }
                        });
                        moved_items.extend(taken);
                    }
                }
            }
        }
    }
    // 2. 更新这些 item 的 category_id,插入到目标 category
    let moved_count = moved_items.len();
    for it in moved_items.iter_mut() {
        it["category_id"] = json!(target_category_id);
    }
    if !moved_items.is_empty() {
        if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
            for l in arr.iter_mut() {
                if l.get("id").and_then(|v| v.as_str()) == Some(&target_library_id) {
                    if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                        for c in cats.iter_mut() {
                            if c.get("id").and_then(|v| v.as_str()) == Some(&target_category_id) {
                                if let Some(items) = c.get_mut("items").and_then(|v| v.as_array_mut()) {
                                    for it in &moved_items {
                                        items.push(it.clone());
                                    }
                                }
                                break;
                            }
                        }
                    }
                    break;
                }
            }
        }
    }
    s.save_asset_library(lib.clone());
    ok(json!({ "library": lib, "moved": moved_count }))
}

/// POST /api/asset-library/items/classify - AI 分类 (简化占位,不实际调 AI)
pub async fn asset_library_classify_items(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let s = state.0.lock().unwrap();
    let lib = s.get_asset_library();
    let ids = body.get("ids").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    // 每个 id 对应一个失败占位项
    let items: Vec<Value> = ids
        .iter()
        .map(|_| json!({ "ok": false, "error": "AI 分类暂未实现" }))
        .collect();
    ok(json!({
        "library": lib,
        "items": items,
        "count": 0,
    }))
}

/// POST /api/asset-library/workflows/upload - 上传工作流文件 (Multipart)
/// 字段: files[] + library_id + category_id
/// 保存文件到 data_dir/workflows/,生成 item 记录
pub async fn asset_library_workflow_upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Response {
    let mut library_id = String::new();
    let mut category_id = String::new();
    // 收集文件 (原始文件名, 字节内容)
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "library_id" => {
                library_id = field.text().await.unwrap_or_default();
            }
            "category_id" => {
                category_id = field.text().await.unwrap_or_default();
            }
            "files[]" | "files" => {
                let orig = field.file_name().unwrap_or("workflow.json").to_string();
                let bytes = match field.bytes().await {
                    Ok(b) => b.to_vec(),
                    Err(_) => continue,
                };
                files.push((orig, bytes));
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }
    if library_id.is_empty() || category_id.is_empty() {
        return bad("缺少 library_id 或 category_id");
    }
    if files.is_empty() {
        return bad("未接收到文件");
    }
    // 准备 workflows 目录
    let workflows_dir = {
        let s = state.0.lock().unwrap();
        let dir = s.data_dir().join("workflows");
        let _ = std::fs::create_dir_all(&dir);
        dir
    };
    // 保存文件并构造 item
    let mut saved_items: Vec<Value> = Vec::new();
    for (orig, bytes) in files.into_iter() {
        let id = new_id();
        let safe_name = format!("{}_{}", id, orig);
        let path = workflows_dir.join(&safe_name);
        let _ = std::fs::write(&path, &bytes);
        let url = format!("/data/workflows/{}", safe_name);
        let item = json!({
            "id": id,
            "name": orig,
            "url": url,
            "category_id": category_id,
            "registrations": {},
        });
        saved_items.push(item);
    }
    // 把 saved_items push 到对应 category
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_asset_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if l.get("id").and_then(|v| v.as_str()) == Some(&library_id) {
                if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                    for c in cats.iter_mut() {
                        if c.get("id").and_then(|v| v.as_str()) == Some(&category_id) {
                            if let Some(items) = c.get_mut("items").and_then(|v| v.as_array_mut()) {
                                for it in &saved_items {
                                    items.push(it.clone());
                                }
                            }
                            found = true;
                            break;
                        }
                    }
                }
                break;
            }
        }
    }
    if !found {
        return not_found("分类或资产库不存在");
    }
    s.save_asset_library(lib.clone());
    let ids: Vec<Value> = saved_items
        .iter()
        .map(|it| json!({ "id": it["id"].clone() }))
        .collect();
    ok(json!({ "library": lib, "items": ids }))
}

/// POST /api/asset-library/items/:id/register-avatar - 注册数字人 (简化占位)
pub async fn asset_library_register_avatar(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(_body): Json<Value>,
) -> Response {
    let s = state.0.lock().unwrap();
    ok(json!({ "library": s.get_asset_library() }))
}

/// POST /api/asset-library/items/:id/avatar-status - 数字人状态 (简化占位)
pub async fn asset_library_avatar_status(
    State(state): State<AppState>,
    Path(_id): Path<String>,
    Json(_body): Json<Value>,
) -> Response {
    let s = state.0.lock().unwrap();
    ok(json!({
        "library": s.get_asset_library(),
        "item": { "registrations": {} }
    }))
}

// ============================================================
// D. 提示词库 prompt-libraries
// ============================================================

/// GET /api/prompt-libraries - 返回整个提示词库
pub async fn prompt_library_get(State(state): State<AppState>) -> Response {
    let s = state.0.lock().unwrap();
    ok(json!({ "library": s.get_prompt_library() }))
}

/// POST /api/prompt-libraries - 创建提示词库
pub async fn prompt_library_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let id = new_id();
    let new_lib = json!({
        "id": id,
        "name": name,
        "readonly": false,
        "items": [],
        "categories": [],
    });
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        arr.push(new_lib);
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib, "prompt_library": { "id": id } }))
}

/// PATCH /api/prompt-libraries/:id - 重命名提示词库
pub async fn prompt_library_rename(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if l.get("id").and_then(|v| v.as_str()) == Some(&id) {
                l["name"] = json!(name);
                found = true;
                break;
            }
        }
    }
    if !found {
        return not_found("提示词库不存在");
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// DELETE /api/prompt-libraries/:id - 删除提示词库
pub async fn prompt_library_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        arr.retain(|l| l.get("id").and_then(|v| v.as_str()) != Some(&id));
    }
    let active = lib.get("active_library_id").and_then(|v| v.as_str());
    if active == Some(id.as_str()) {
        lib["active_library_id"] = Value::Null;
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// POST /api/prompt-libraries/items - 创建提示词
pub async fn prompt_library_create_item(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let library_id = body.get("library_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let positive = body.get("positive").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let negative = body.get("negative").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let category = body.get("category").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let scene = body.get("scene").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if library_id.is_empty() {
        return bad("缺少 library_id");
    }
    let id = new_id();
    let new_item = json!({
        "id": id,
        "name": name,
        "positive": positive,
        "negative": negative,
        "category": category,
        "scene": scene,
    });
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if l.get("id").and_then(|v| v.as_str()) == Some(&library_id) {
                if let Some(items) = l.get_mut("items").and_then(|v| v.as_array_mut()) {
                    items.push(new_item);
                }
                found = true;
                break;
            }
        }
    }
    if !found {
        return not_found("提示词库不存在");
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib, "item": { "id": id } }))
}

/// PATCH /api/prompt-libraries/items/:id - 更新提示词
pub async fn prompt_library_update_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let library_id = body.get("library_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    let mut found_item: Option<Value> = None;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            let lib_match = l.get("id").and_then(|v| v.as_str()) == Some(&library_id);
            if library_id.is_empty() || lib_match {
                if let Some(items) = l.get_mut("items").and_then(|v| v.as_array_mut()) {
                    for it in items.iter_mut() {
                        if it.get("id").and_then(|v| v.as_str()) == Some(&id) {
                            // 仅更新 body 中存在的字段
                            for key in ["name", "positive", "negative", "category", "scene"] {
                                if let Some(v) = body.get(key) {
                                    it[key] = v.clone();
                                }
                            }
                            found_item = Some(it.clone());
                            break;
                        }
                    }
                }
                if found_item.is_some() {
                    break;
                }
            }
        }
    }
    match found_item {
        Some(item) => {
            s.save_prompt_library(lib.clone());
            ok(json!({ "library": lib, "item": item }))
        }
        None => not_found("提示词不存在"),
    }
}

/// DELETE /api/prompt-libraries/items/:id - 删除提示词
pub async fn prompt_library_delete_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if let Some(items) = l.get_mut("items").and_then(|v| v.as_array_mut()) {
                let before = items.len();
                items.retain(|it| it.get("id").and_then(|v| v.as_str()) != Some(&id));
                if items.len() != before {
                    found = true;
                }
            }
            if found {
                break;
            }
        }
    }
    if !found {
        return not_found("提示词不存在");
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// POST /api/prompt-libraries/items/delete - 批量删除提示词
pub async fn prompt_library_delete_items(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let ids: Vec<String> = body
        .get("ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    if ids.is_empty() {
        return ok(json!({ "library": lib }));
    }
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if let Some(items) = l.get_mut("items").and_then(|v| v.as_array_mut()) {
                items.retain(|it| {
                    let iid = it.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    !ids.iter().any(|x| x == iid)
                });
            }
        }
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// POST /api/prompt-libraries/categories - 创建分类
pub async fn prompt_library_create_category(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let library_id = body.get("library_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if library_id.is_empty() {
        return bad("缺少 library_id");
    }
    let id = new_id();
    let new_cat = json!({ "id": id, "name": name });
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if l.get("id").and_then(|v| v.as_str()) == Some(&library_id) {
                if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                    cats.push(new_cat);
                }
                found = true;
                break;
            }
        }
    }
    if !found {
        return not_found("提示词库不存在");
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib, "category": { "id": id } }))
}

/// PATCH /api/prompt-libraries/categories/:id - 重命名分类
pub async fn prompt_library_rename_category(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let library_id = body.get("library_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            let lib_match = l.get("id").and_then(|v| v.as_str()) == Some(&library_id);
            if library_id.is_empty() || lib_match {
                if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                    for c in cats.iter_mut() {
                        if c.get("id").and_then(|v| v.as_str()) == Some(&id) {
                            c["name"] = json!(name);
                            found = true;
                            break;
                        }
                    }
                }
                if found {
                    break;
                }
            }
        }
    }
    if !found {
        return not_found("分类不存在");
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib }))
}

/// DELETE /api/prompt-libraries/categories/:id - 删除分类
pub async fn prompt_library_delete_category(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let mut s = state.0.lock().unwrap();
    let mut lib = s.get_prompt_library();
    let mut found = false;
    if let Some(arr) = lib.get_mut("libraries").and_then(|v| v.as_array_mut()) {
        for l in arr.iter_mut() {
            if let Some(cats) = l.get_mut("categories").and_then(|v| v.as_array_mut()) {
                let before = cats.len();
                cats.retain(|c| c.get("id").and_then(|v| v.as_str()) != Some(&id));
                if cats.len() != before {
                    found = true;
                }
            }
            if found {
                break;
            }
        }
    }
    if !found {
        return not_found("分类不存在");
    }
    s.save_prompt_library(lib.clone());
    ok(json!({ "library": lib }))
}

// ============================================================
// E. 角度图 angle (ModelScope 图像编辑)
// ============================================================

/// ModelScope 图像编辑 API 端点
const MODELSCOPE_IMAGE_EDIT_URL: &str =
    "https://api-inference.modelscope.cn/v1/services/aigc/multi-modal/image-edit";
/// ModelScope 任务轮询 API 基址
const MODELSCOPE_TASK_URL: &str = "https://api-inference.modelscope.cn/v1/tasks";

/// POST /api/angle/generate - 角度图生成 (ModelScope 图像编辑)
/// 请求体: {prompt, api_key, type, model, image_urls, client_id}
/// - 同步返回 {url}
/// - 异步返回 {status:"timeout", task_id}
/// - 失败返回 {detail}
pub async fn angle_generate(Json(body): Json<Value>) -> Response {
    let api_key = body.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Qwen/Qwen-Image-Edit-2511")
        .to_string();
    let image_urls: Vec<String> = body
        .get("image_urls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if api_key.is_empty() {
        return not_configured("角度图 ModelScope");
    }
    if prompt.is_empty() {
        return bad("prompt 不能为空");
    }
    if image_urls.is_empty() {
        return bad("image_urls 不能为空");
    }
    // 构造 ModelScope 请求体
    let req_body = json!({
        "model": model,
        "input": {
            "prompt": prompt,
            "image_urls": image_urls,
        }
    });
    let client = reqwest::Client::new();
    let resp = match client
        .post(MODELSCOPE_IMAGE_EDIT_URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&req_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return bad_gateway(format!("调用 ModelScope 失败: {e}"));
        }
    };
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let body_json: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        return upstream_error(status, body_text).await;
    }
    // 1. 直接返回结果: output.image_url 或 data.image_url
    if let Some(url) = body_json
        .get("output")
        .and_then(|o| o.get("image_url"))
        .and_then(|v| v.as_str())
    {
        return ok(json!({ "url": url }));
    }
    if let Some(url) = body_json
        .get("data")
        .and_then(|o| o.get("image_url"))
        .and_then(|v| v.as_str())
    {
        return ok(json!({ "url": url }));
    }
    // 兼容 OpenAI 风格 data[0].url
    if let Some(url) = body_json
        .get("data")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|d| d.get("url"))
        .and_then(|v| v.as_str())
    {
        return ok(json!({ "url": url }));
    }
    // 2. 异步任务: request_id / task_id
    let task_id = body_json
        .get("task_id")
        .and_then(|v| v.as_str())
        .or_else(|| body_json.get("request_id").and_then(|v| v.as_str()))
        .map(|s| s.to_string());
    if let Some(tid) = task_id {
        return ok(json!({ "status": "timeout", "task_id": tid }));
    }
    // 3. 其他无法识别的响应
    bad_gateway(format!("ModelScope 返回无法识别: {}", body_text))
}

/// POST /api/angle/poll_status - 轮询 ModelScope 任务状态 (注意是 POST)
/// 请求体: {task_id, api_key, client_id}
/// - 成功返回 {url}
/// - 未完成返回 {status:"timeout", task_id}
/// - 失败返回 {detail}
pub async fn angle_poll_status(Json(body): Json<Value>) -> Response {
    let task_id = body.get("task_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let api_key = body.get("api_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if task_id.is_empty() {
        return bad("缺少 task_id");
    }
    if api_key.is_empty() {
        return not_configured("角度图 ModelScope");
    }
    let url = format!("{}/{}", MODELSCOPE_TASK_URL, task_id);
    let client = reqwest::Client::new();
    let resp = match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return bad_gateway(format!("查询 ModelScope 任务失败: {e}"));
        }
    };
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let body_json: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        return upstream_error(status, body_text).await;
    }
    // 解析任务状态
    let task_status = body_json
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    match task_status {
        "SUCCEEDED" | "succeeded" | "completed" | "success" => {
            // 取 output.image_url 或 data.image_url
            if let Some(url) = body_json
                .get("output")
                .and_then(|o| o.get("image_url"))
                .and_then(|v| v.as_str())
            {
                return ok(json!({ "url": url }));
            }
            if let Some(url) = body_json
                .get("data")
                .and_then(|o| o.get("image_url"))
                .and_then(|v| v.as_str())
            {
                return ok(json!({ "url": url }));
            }
            if let Some(url) = body_json
                .get("output")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|d| d.get("url"))
                .and_then(|v| v.as_str())
            {
                return ok(json!({ "url": url }));
            }
            bad_gateway(format!("任务完成但未找到图片: {}", body_text))
        }
        "FAILED" | "failed" | "error" => {
            let detail = body_json
                .get("errors")
                .or_else(|| body_json.get("error"))
                .or_else(|| body_json.get("message"))
                .map(|v| {
                    if let Some(s) = v.as_str() {
                        s.to_string()
                    } else {
                        v.to_string()
                    }
                })
                .unwrap_or_else(|| body_text.clone());
            bad_gateway(format!("ModelScope 任务失败: {}", detail))
        }
        // PENDING / RUNNING 等中间态
        _ => ok(json!({ "status": "timeout", "task_id": task_id })),
    }
}

// ============================================================
// F. Modelscope msgen (文生图 / 图生图)
// ============================================================

/// ModelScope 文生图 API 端点
const MODELSCOPE_IMAGES_URL: &str = "https://api-inference.modelscope.cn/v1/images/generations";

/// POST /api/ms/generate - ModelScope 文生图 / 图生图
/// 请求体: {prompt, model, image_urls?, width?, height?, size?, client_id, loras?}
/// api_key 不在 body 中,需从 provider 查 "modelscope" 获取
/// 响应: {url} (取 data[0].url)
pub async fn ms_generate(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let model = body
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("black-forest-labs/FLUX.2-klein-9B")
        .to_string();
    let image_urls: Vec<String> = body
        .get("image_urls")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let size = body.get("size").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if prompt.is_empty() {
        return bad("prompt 不能为空");
    }
    // 从 provider 取 api_key (查找 id 含 "modelscope" 的 provider)
    let (api_key, _base_url) = {
        let s = state.0.lock().unwrap();
        // 优先精确 id,其次按 id/name 模糊匹配
        let providers = s.list_providers();
        let matched = providers
            .iter()
            .find(|p| {
                let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                id == "modelscope" || id.contains("modelscope")
            })
            .or_else(|| {
                providers.iter().find(|p| {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                    name.contains("modelscope")
                })
            });
        match matched {
            Some(p) => provider_endpoint(p),
            None => return not_configured("ModelScope"),
        }
    };
    if api_key.is_empty() {
        return not_configured("ModelScope");
    }
    // 构造 OpenAI 兼容请求体
    let mut req = json!({ "model": model, "prompt": prompt });
    if !size.is_empty() {
        req["size"] = json!(size);
    } else {
        // 用 width/height 拼 size
        let width = body.get("width").and_then(|v| v.as_i64());
        let height = body.get("height").and_then(|v| v.as_i64());
        if let (Some(w), Some(h)) = (width, height) {
            req["size"] = json!(format!("{}x{}", w, h));
        }
    }
    // 图生图: image_urls[0] 作为 image 字段
    if let Some(first) = image_urls.first() {
        req["image"] = json!(first);
    }
    // loras 透传
    if let Some(loras) = body.get("loras") {
        req["loras"] = loras.clone();
    }
    let client = reqwest::Client::new();
    let resp = match client
        .post(MODELSCOPE_IMAGES_URL)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&req)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return bad_gateway(format!("调用 ModelScope 失败: {e}"));
        }
    };
    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    let body_json: Value = serde_json::from_str(&body_text).unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        return upstream_error(status, body_text).await;
    }
    // 取 data[0].url
    let url = body_json
        .get("data")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|d| d.get("url"))
        .and_then(|v| v.as_str());
    match url {
        Some(u) => ok(json!({ "url": u })),
        None => bad_gateway(format!("ModelScope 未返回图片: {}", body_text)),
    }
}
