//! 用户分组 — 用户组 + 分组计费倍率 + 组内模型权限。
//!
//! 参照 new-api user.go 的 group 字段 + controller/group.go 的分组倍率设计。
//! 与 pricing 模块配合：最终费用 = 基础费用 * model_ratio * group_ratio，
//! 其中 group_ratio 由本模块的 UserGroup.ratio 提供（也可在 pricing 的 RatioConfig 中配置）。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 用户分组定义。
///
/// 参照 new-api group：name/ratio/allowed_models。ratio 为计费倍率（1.0=原价）。
/// allowed_models 为组内模型权限白名单（None 表示不限，Some(空) 表示全禁）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserGroup {
    /// 分组名（唯一键）
    pub name: String,
    /// 计费倍率（1.0=原价）
    #[serde(default = "default_ratio")]
    pub ratio: f64,
    /// 组内模型权限白名单（None=不限）
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    /// 描述
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_ratio() -> f64 {
    1.0
}

impl UserGroup {
    pub fn new(name: impl Into<String>, ratio: f64) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            name: name.into(),
            ratio,
            allowed_models: None,
            description: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// 组内是否允许使用指定模型（None=不限，Some 则需包含）
    pub fn allows_model(&self, model: &str) -> bool {
        match &self.allowed_models {
            None => true,
            Some(list) => list.iter().any(|m| m == model),
        }
    }
}

/// 用户分组存储。
///
/// key 前缀 `user_group:`，以分组名为键。默认分组 "default" 自动创建。
pub struct UserGroupStore {
    groups: RwLock<HashMap<String, UserGroup>>,
    store: Arc<FileStore>,
}

impl UserGroupStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            groups: RwLock::new(HashMap::new()),
            store,
        };
        let _ = s.load();
        s.ensure_default();
        s
    }

    /// 从存储加载分组
    pub fn load(&self) -> anyhow::Result<()> {
        let keys = self.store.list("user_group:")?;
        let mut groups = HashMap::with_capacity(keys.len());
        for key in &keys {
            if let Some(g) = self.store.get::<UserGroup>(key)? {
                groups.insert(g.name.clone(), g);
            }
        }
        *self.groups.write() = groups;
        Ok(())
    }

    /// 确保默认分组存在
    fn ensure_default(&self) {
        if !self.groups.read().contains_key("default") {
            let g = UserGroup::new("default", 1.0);
            if let Err(e) = self.store.put("user_group:default", &g) {
                tracing::error!("Failed to persist default user_group: {}", e);
            }
            self.groups.write().insert("default".to_string(), g);
        }
    }

    fn persist(&self, g: &UserGroup) -> anyhow::Result<()> {
        self.store.put(&format!("user_group:{}", g.name), g)?;
        Ok(())
    }

    /// 列出所有分组
    pub fn list(&self) -> Vec<UserGroup> {
        let mut all: Vec<UserGroup> = self.groups.read().values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all
    }

    /// 获取分组
    pub fn get(&self, name: &str) -> Option<UserGroup> {
        self.groups.read().get(name).cloned()
    }

    /// 新增/更新分组（upsert，以 name 为键）
    pub fn upsert(&self, mut group: UserGroup) -> anyhow::Result<UserGroup> {
        if group.name.is_empty() {
            anyhow::bail!("group name cannot be empty");
        }
        let now = chrono::Utc::now().timestamp();
        if group.created_at == 0 {
            group.created_at = now;
        }
        group.updated_at = now;
        self.persist(&group)?;
        self.groups
            .write()
            .insert(group.name.clone(), group.clone());
        Ok(group)
    }

    /// 删除分组（default 不可删）
    pub fn remove(&self, name: &str) -> anyhow::Result<()> {
        if name == "default" {
            anyhow::bail!("cannot delete default group");
        }
        self.store.delete(&format!("user_group:{name}"))?;
        self.groups.write().remove(name);
        Ok(())
    }

    /// 获取分组的计费倍率（不存在返回 1.0）
    pub fn ratio(&self, name: &str) -> f64 {
        self.groups.read().get(name).map(|g| g.ratio).unwrap_or(1.0)
    }

    /// 检查分组是否允许使用指定模型
    pub fn allows_model(&self, group: &str, model: &str) -> bool {
        match self.groups.read().get(group) {
            Some(g) => g.allows_model(model),
            None => true, // 未知分组不限
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> UserGroupStore {
        UserGroupStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn default_group_exists() {
        let s = store();
        assert!(s.get("default").is_some());
        assert_eq!(s.ratio("default"), 1.0);
    }

    #[test]
    fn upsert_and_ratio() {
        let s = store();
        s.upsert(UserGroup::new("vip", 0.5)).unwrap();
        assert_eq!(s.ratio("vip"), 0.5);
    }

    #[test]
    fn cannot_delete_default() {
        let s = store();
        assert!(s.remove("default").is_err());
    }

    #[test]
    fn allowed_models() {
        let s = store();
        let mut g = UserGroup::new("restricted", 1.0);
        g.allowed_models = Some(vec!["gpt-4".to_string()]);
        s.upsert(g).unwrap();
        assert!(s.allows_model("restricted", "gpt-4"));
        assert!(!s.allows_model("restricted", "gpt-3.5"));
    }
}
