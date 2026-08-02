// 画布/项目/配置/会话/素材库/提示词库/历史记录等数据持久化层。
// 使用 JSON 文件存储,内容以 serde_json::Value 原样保留,
// 确保前端发送的任何字段都能 1:1 还原。
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

pub struct Store {
    canvases: Vec<Value>,
    projects: Vec<Value>,
    providers: Vec<Value>,
    conversations: Vec<Value>,
    asset_library: Value,
    prompt_library: Value,
    history: Value,
    comfy_instances: Vec<Value>,
    rh_workflows: Vec<Value>,
    workflows: Vec<Value>,
    data_dir: PathBuf,
}

impl Store {
    /// 打开或初始化数据目录。data_dir 下的 *.json 保存各类数据。
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir).context("创建数据目录失败")?;
        std::fs::create_dir_all(data_dir.join("uploads")).context("创建上传目录失败")?;
        let mut s = Store {
            canvases: Vec::new(),
            projects: Vec::new(),
            providers: Vec::new(),
            conversations: Vec::new(),
            asset_library: json!({ "libraries": [], "workflows": [], "active_library_id": null }),
            prompt_library: json!({ "libraries": [], "active_library_id": null }),
            history: json!({}),
            comfy_instances: Vec::new(),
            rh_workflows: Vec::new(),
            workflows: Vec::new(),
            data_dir: data_dir.to_path_buf(),
        };
        s.canvases = s.load("canvases.json");
        s.projects = s.load("projects.json");
        s.providers = s.load("providers.json");
        s.conversations = s.load("conversations.json");
        s.asset_library = s.load_value("asset_library.json");
        s.prompt_library = s.load_value("prompt_library.json");
        s.history = s.load_value("history.json");
        s.comfy_instances = s.load("comfy_instances.json");
        s.rh_workflows = s.load("rh_workflows.json");
        s.workflows = s.load("workflows.json");
        // 首次启动时创建一个默认项目,避免画布列表空空荡荡
        if s.projects.is_empty() {
            let now = now_ms();
            let proj = json!({
                "id": Uuid::new_v4().to_string(),
                "name": "默认项目",
                "color": "#6366f1",
                "created_at": now,
                "updated_at": now,
            });
            s.projects.push(proj.clone());
            s.persist_projects();
        }
        Ok(s)
    }

    fn load(&self, name: &str) -> Vec<Value> {
        let p = self.data_dir.join(name);
        match std::fs::read_to_string(&p) {
            Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn load_value(&self, name: &str) -> Value {
        let p = self.data_dir.join(name);
        match std::fs::read_to_string(&p) {
            Ok(s) if !s.trim().is_empty() => serde_json::from_str(&s).unwrap_or_else(|_| json!({})),
            _ => json!({}),
        }
    }

    fn save(&self, name: &str, data: &[Value]) {
        let p = self.data_dir.join(name);
        if let Ok(json) = serde_json::to_string_pretty(data) {
            let _ = std::fs::write(p, json);
        }
    }

    fn save_value(&self, name: &str, data: &Value) {
        let p = self.data_dir.join(name);
        if let Ok(json) = serde_json::to_string_pretty(data) {
            let _ = std::fs::write(p, json);
        }
    }

    fn persist_canvases(&self) { self.save("canvases.json", &self.canvases); }
    fn persist_projects(&self) { self.save("projects.json", &self.projects); }
    fn persist_providers(&self) { self.save("providers.json", &self.providers); }
    fn persist_conversations(&self) { self.save("conversations.json", &self.conversations); }
    fn persist_asset_library(&self) { self.save_value("asset_library.json", &self.asset_library); }
    fn persist_prompt_library(&self) { self.save_value("prompt_library.json", &self.prompt_library); }
    fn persist_history(&self) { self.save_value("history.json", &self.history); }
    fn persist_comfy_instances(&self) { self.save("comfy_instances.json", &self.comfy_instances); }
    fn persist_rh_workflows(&self) { self.save("rh_workflows.json", &self.rh_workflows); }
    fn persist_workflows(&self) { self.save("workflows.json", &self.workflows); }

    pub fn data_dir(&self) -> &Path { &self.data_dir }
    pub fn uploads_dir(&self) -> PathBuf { self.data_dir.join("uploads") }

    // ===== 画布 CRUD =====

    /// 列出未删除画布 (按更新时间倒序)
    pub fn list_canvases(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self.canvases.iter()
            .filter(|c| !c.get("deleted").and_then(|d| d.as_bool()).unwrap_or(false))
            .cloned().collect();
        v.sort_by(|a, b| {
            let ta = a.get("updated_at").and_then(|x| x.as_i64()).unwrap_or(0);
            let tb = b.get("updated_at").and_then(|x| x.as_i64()).unwrap_or(0);
            tb.cmp(&ta)
        });
        v
    }

    /// 列出回收站画布
    pub fn list_trash(&self) -> Vec<Value> {
        self.canvases.iter()
            .filter(|c| c.get("deleted").and_then(|d| d.as_bool()).unwrap_or(false))
            .cloned()
            .collect()
    }

    pub fn get_canvas(&self, id: &str) -> Option<Value> {
        self.canvases.iter().find(|c| c.get("id").and_then(|i| i.as_str()) == Some(id)).cloned()
    }

    /// 创建画布。body 是前端发来的 {title, icon, kind, project, board_x, board_y}
    pub fn create_canvas(&mut self, mut body: Value) -> Value {
        let now = now_ms();
        let id = Uuid::new_v4().to_string();
        if body.get("title").is_none() { body["title"] = json!("画布"); }
        if body.get("icon").is_none() { body["icon"] = json!("🧩"); }
        if body.get("kind").is_none() { body["kind"] = json!("classic"); }
        if body.get("nodes").is_none() { body["nodes"] = json!([]); }
        if body.get("connections").is_none() { body["connections"] = json!([]); }
        if body.get("viewport").is_none() { body["viewport"] = json!({ "x": 0, "y": 0, "scale": 1 }); }
        if body.get("logs").is_none() { body["logs"] = json!([]); }
        body["id"] = json!(id);
        body["deleted"] = json!(false);
        body["created_at"] = json!(now);
        body["updated_at"] = json!(now);
        self.canvases.push(body.clone());
        self.persist_canvases();
        body
    }

    /// 更新画布。合并字段并刷新 updated_at
    pub fn update_canvas(&mut self, id: &str, mut patch: Value) -> Option<Value> {
        let idx = self.canvases.iter().position(|c| c.get("id").and_then(|i| i.as_str()) == Some(id))?;
        let target = &mut self.canvases[idx];
        if let Some(obj) = patch.as_object_mut() {
            // 前端不期望这些字段被覆盖
            obj.remove("id");
            obj.remove("created_at");
            obj.remove("deleted");
        }
        if let Some(t) = target.as_object_mut() {
            if let Some(po) = patch.as_object() {
                for (k, v) in po { t.insert(k.clone(), v.clone()); }
            }
            t.insert("updated_at".into(), json!(now_ms()));
        }
        let updated = target.clone();
        self.persist_canvases();
        Some(updated)
    }

    /// 更新画布元数据 (title/pinned/color/owner/project/icon 等 patch)
    pub fn update_canvas_meta(&mut self, id: &str, patch: Value) -> Option<Value> {
        let idx = self.canvases.iter().position(|c| c.get("id").and_then(|i| i.as_str()) == Some(id))?;
        let target = &mut self.canvases[idx];
        if let Some(t) = target.as_object_mut() {
            if let Some(po) = patch.as_object() {
                for (k, v) in po {
                    if k == "id" || k == "created_at" || k == "deleted" { continue; }
                    t.insert(k.clone(), v.clone());
                }
            }
            t.insert("updated_at".into(), json!(now_ms()));
        }
        let updated = target.clone();
        self.persist_canvases();
        Some(updated)
    }

    /// 仅刷新访问时间
    pub fn touch_canvas(&mut self, id: &str) -> Option<Value> {
        let idx = self.canvases.iter().position(|c| c.get("id").and_then(|i| i.as_str()) == Some(id))?;
        if let Some(t) = self.canvases[idx].as_object_mut() {
            t.insert("updated_at".into(), json!(now_ms()));
        }
        let updated = self.canvases[idx].clone();
        self.persist_canvases();
        Some(updated)
    }

    /// 移入回收站 (软删除)
    pub fn trash_canvas(&mut self, id: &str) -> bool {
        if let Some(c) = self.canvases.iter_mut().find(|c| c.get("id").and_then(|i| i.as_str()) == Some(id)) {
            c["deleted"] = json!(true);
            c["deleted_at"] = json!(now_ms());
            c["updated_at"] = json!(now_ms());
            self.persist_canvases();
            true
        } else { false }
    }

    /// 从回收站恢复
    pub fn restore_canvas(&mut self, id: &str) -> bool {
        if let Some(c) = self.canvases.iter_mut().find(|c| c.get("id").and_then(|i| i.as_str()) == Some(id)) {
            c["deleted"] = json!(false);
            if let Some(t) = c.as_object_mut() { t.remove("deleted_at"); }
            c["updated_at"] = json!(now_ms());
            self.persist_canvases();
            true
        } else { false }
    }

    /// 彻底删除
    pub fn purge_canvas(&mut self, id: &str) -> bool {
        let before = self.canvases.len();
        self.canvases.retain(|c| c.get("id").and_then(|i| i.as_str()) != Some(id));
        let changed = self.canvases.len() != before;
        if changed { self.persist_canvases(); }
        changed
    }

    // ===== 项目 CRUD =====
    pub fn list_projects(&self) -> Vec<Value> { self.projects.clone() }

    pub fn create_project(&mut self, mut body: Value) -> Value {
        let now = now_ms();
        let id = Uuid::new_v4().to_string();
        if body.get("name").is_none() { body["name"] = json!("新项目"); }
        if body.get("color").is_none() { body["color"] = json!("#6366f1"); }
        body["id"] = json!(id);
        body["created_at"] = json!(now);
        body["updated_at"] = json!(now);
        self.projects.push(body.clone());
        self.persist_projects();
        body
    }

    pub fn delete_project(&mut self, id: &str) -> bool {
        let before = self.projects.len();
        self.projects.retain(|p| p.get("id").and_then(|i| i.as_str()) != Some(id));
        // 该项目下的画布移入回收站
        for c in self.canvases.iter_mut() {
            if c.get("project").and_then(|p| p.as_str()) == Some(id) {
                c["deleted"] = json!(true);
            }
        }
        let changed = self.projects.len() != before;
        if changed { self.persist_projects(); self.persist_canvases(); }
        changed
    }

    // ===== Providers CRUD (保存用户配置的 API 供应商及 API Key) =====

    /// 列出所有 providers (原样返回,包含 api_key)
    pub fn list_providers(&self) -> Vec<Value> { self.providers.clone() }

    /// 整体覆盖 providers 列表 (前端 PUT /api/providers 时调用)
    pub fn update_providers(&mut self, new_providers: Vec<Value>) {
        self.providers = new_providers;
        self.persist_providers();
    }

    /// 按 id 查找 provider
    pub fn get_provider(&self, id: &str) -> Option<Value> {
        self.providers.iter()
            .find(|p| p.get("id").and_then(|i| i.as_str()) == Some(id))
            .cloned()
    }

    // ===== 会话 CRUD (LLM 对话历史) =====

    pub fn list_conversations(&self) -> Vec<Value> { self.conversations.clone() }

    pub fn create_conversation(&mut self, title: &str) -> Value {
        let id = Uuid::new_v4().to_string();
        let conv = json!({
            "id": id,
            "title": if title.is_empty() { "新对话" } else { title },
            "messages": [],
            "created_at": now_ms(),
            "updated_at": now_ms(),
        });
        self.conversations.push(conv.clone());
        self.persist_conversations();
        conv
    }

    pub fn get_conversation(&self, id: &str) -> Option<Value> {
        self.conversations.iter().find(|c| c.get("id").and_then(|i| i.as_str()) == Some(id)).cloned()
    }

    pub fn update_conversation(&mut self, id: &str, conv: Value) -> Option<Value> {
        let idx = self.conversations.iter().position(|c| c.get("id").and_then(|i| i.as_str()) == Some(id))?;
        let mut merged = conv.clone();
        if let Some(t) = merged.as_object_mut() {
            t.insert("id".into(), json!(id));
            t.insert("updated_at".into(), json!(now_ms()));
        }
        self.conversations[idx] = merged.clone();
        self.persist_conversations();
        Some(merged)
    }

    pub fn delete_conversation(&mut self, id: &str) -> bool {
        let before = self.conversations.len();
        self.conversations.retain(|c| c.get("id").and_then(|i| i.as_str()) != Some(id));
        let changed = self.conversations.len() != before;
        if changed { self.persist_conversations(); }
        changed
    }

    // ===== 素材库 (asset-library) =====

    pub fn get_asset_library(&self) -> Value { self.asset_library.clone() }

    pub fn save_asset_library(&mut self, lib: Value) {
        self.asset_library = lib;
        self.persist_asset_library();
    }

    // ===== 提示词库 (prompt-library) =====

    pub fn get_prompt_library(&self) -> Value { self.prompt_library.clone() }

    pub fn save_prompt_library(&mut self, lib: Value) {
        self.prompt_library = lib;
        self.persist_prompt_library();
    }

    // ===== 历史记录 (按 type 分桶) =====

    pub fn list_history(&self, type_name: &str) -> Vec<Value> {
        self.history.get(type_name).and_then(|v| v.as_array()).cloned().unwrap_or_default()
    }

    pub fn add_history(&mut self, type_name: &str, mut record: Value) -> Value {
        if record.get("timestamp").is_none() {
            record["timestamp"] = json!(now_ms());
        }
        let arr = self.history.as_object_mut().unwrap()
            .entry(type_name.to_string()).or_insert_with(|| json!([]));
        if let Some(a) = arr.as_array_mut() {
            a.insert(0, record.clone());
            // 限制每类最多 500 条
            if a.len() > 500 { a.truncate(500); }
        }
        self.persist_history();
        record
    }

    pub fn delete_history(&mut self, type_name: &str, timestamp: &str) -> bool {
        let mut removed = false;
        if let Some(arr) = self.history.get_mut(type_name).and_then(|v| v.as_array_mut()) {
            let before = arr.len();
            arr.retain(|r| {
                let ts = r.get("timestamp").map(|v| match v {
                    Value::Number(n) => n.to_string(),
                    Value::String(s) => s.clone(),
                    _ => String::new(),
                }).unwrap_or_default();
                if ts == timestamp { removed = true; false } else { true }
            });
            let _ = before;
        }
        if removed { self.persist_history(); }
        removed
    }

    // ===== ComfyUI 实例配置 =====

    pub fn list_comfy_instances(&self) -> Vec<Value> { self.comfy_instances.clone() }

    pub fn save_comfy_instances(&mut self, instances: Vec<Value>) {
        self.comfy_instances = instances;
        self.persist_comfy_instances();
    }

    // ===== RunningHub 工作流库 =====

    pub fn list_rh_workflows(&self) -> Vec<Value> { self.rh_workflows.clone() }

    pub fn get_rh_workflow(&self, workflow_id: &str) -> Option<Value> {
        self.rh_workflows.iter()
            .find(|w| w.get("workflowId").and_then(|i| i.as_str()) == Some(workflow_id))
            .cloned()
    }

    pub fn upsert_rh_workflow(&mut self, workflow: Value) -> Value {
        let wid = workflow.get("workflowId").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if let Some(idx) = self.rh_workflows.iter().position(|w| w.get("workflowId").and_then(|i| i.as_str()) == Some(&wid)) {
            self.rh_workflows[idx] = workflow.clone();
        } else {
            self.rh_workflows.push(workflow.clone());
        }
        self.persist_rh_workflows();
        workflow
    }

    pub fn delete_rh_workflow(&mut self, workflow_id: &str) -> bool {
        let before = self.rh_workflows.len();
        self.rh_workflows.retain(|w| w.get("workflowId").and_then(|i| i.as_str()) != Some(workflow_id));
        let changed = self.rh_workflows.len() != before;
        if changed { self.persist_rh_workflows(); }
        changed
    }

    // ===== ComfyUI 工作流库 (本地工作流) =====

    pub fn list_workflows(&self) -> Vec<Value> { self.workflows.clone() }

    pub fn get_workflow(&self, name: &str) -> Option<Value> {
        self.workflows.iter()
            .find(|w| w.get("name").and_then(|i| i.as_str()) == Some(name))
            .cloned()
    }

    pub fn upsert_workflow(&mut self, name: &str, workflow: Value) -> Value {
        if let Some(idx) = self.workflows.iter().position(|w| w.get("name").and_then(|i| i.as_str()) == Some(name)) {
            let mut w = workflow.clone();
            if let Some(t) = w.as_object_mut() {
                t.insert("name".into(), json!(name));
                t.insert("updated_at".into(), json!(now_ms()));
            }
            self.workflows[idx] = w.clone();
            self.persist_workflows();
            w
        } else {
            let mut w = workflow.clone();
            if let Some(t) = w.as_object_mut() {
                t.insert("name".into(), json!(name));
                t.insert("builtin".into(), json!(false));
                t.insert("updated_at".into(), json!(now_ms()));
            }
            self.workflows.push(w.clone());
            self.persist_workflows();
            w
        }
    }

    pub fn delete_workflow(&mut self, name: &str) -> bool {
        let before = self.workflows.len();
        self.workflows.retain(|w| w.get("name").and_then(|i| i.as_str()) != Some(name));
        let changed = self.workflows.len() != before;
        if changed { self.persist_workflows(); }
        changed
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 线程安全的存储句柄
pub struct SharedStore(pub Mutex<Store>);
