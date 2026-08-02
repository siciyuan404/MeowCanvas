// 画布/项目数据持久化层。使用 JSON 文件存储,内容以 serde_json::Value 原样保留,
// 确保前端发送的任何字段都能 1:1 还原 (节点 / 连接 / 视口 / 日志等)。
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use uuid::Uuid;

pub struct Store {
    canvases: Vec<Value>,
    projects: Vec<Value>,
    data_dir: PathBuf,
}

impl Store {
    /// 打开或初始化数据目录。data_dir 下的 canvases.json / projects.json 保存所有数据。
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir).context("创建数据目录失败")?;
        let mut s = Store {
            canvases: Vec::new(),
            projects: Vec::new(),
            data_dir: data_dir.to_path_buf(),
        };
        s.canvases = s.load("canvases.json");
        s.projects = s.load("projects.json");
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

    fn save(&self, name: &str, data: &[Value]) {
        let p = self.data_dir.join(name);
        if let Ok(json) = serde_json::to_string_pretty(data) {
            let _ = std::fs::write(p, json);
        }
    }
    fn persist_canvases(&self) { self.save("canvases.json", &self.canvases); }
    fn persist_projects(&self) { self.save("projects.json", &self.projects); }

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

    /// 移入回收站 (软删除)
    pub fn trash_canvas(&mut self, id: &str) -> bool {
        if let Some(c) = self.canvases.iter_mut().find(|c| c.get("id").and_then(|i| i.as_str()) == Some(id)) {
            c["deleted"] = json!(true);
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
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 线程安全的存储句柄
pub struct SharedStore(pub Mutex<Store>);
