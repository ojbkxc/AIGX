//! 多配置记录的 KV 持久化与 CRUD。
//!
//! 存储键：`client_configs`（单键保存四个目标的全部记录，形状
//! `{ "claude-code": [Record], ... }`）。读改写全程持有
//! [`super::mutation_lock`]，保证「同一目标同一时刻只有一份启用」在
//! 进程内是原子的。
//!
//! 记录里的 `api_key` 一律是 `enc1:` 信封（AES-256-GCM，密钥在库外），
//! 落地前经 [`seal_or_keep`]，读出后**不解密** —— 只有预览/应用计划需要
//! 明文时才就地解密，且明文不出现在任何接口响应里（响应只回 `hasApiKey`）。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::storage::FileStore;

use super::targets::{self, Target};
use super::MAX_CONFIGS_PER_TARGET;

/// KV 键（单键存全部目标，避免遗留孤儿键）。
pub const KV_KEY: &str = "client_configs";

/// 一份命名配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub target: String,
    pub name: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub base_url: String,
    /// `enc1:` 信封或空串（空 = 沿用目标文件里已有的凭据 / DSH 引用名）
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub models: Vec<Value>,
    #[serde(default)]
    pub options: Value,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub last_applied_at: i64,
}

impl Record {
    /// 接口投影：**不含**凭据字段与选项里的敏感值。
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "target": self.target,
            "name": self.name,
            "enabled": self.enabled,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
            "baseUrl": self.base_url,
            "hasApiKey": !self.api_key.is_empty(),
            "model": self.model,
            "models": self.models,
            "options": self.options,
            "note": self.note,
            "lastAppliedAt": self.last_applied_at,
        })
    }
}

/// 全部目标的记录表。
pub type AllRecords = serde_json::Map<String, Value>;

/// 读取全部记录（KV 损坏时返回错误，绝不静默清空）。
pub fn load_all(store: &FileStore) -> Result<AllRecords, String> {
    match store.get::<Value>(KV_KEY) {
        Ok(None) => Ok(AllRecords::new()),
        Ok(Some(Value::Object(map))) => Ok(map),
        Ok(Some(_)) => Err("客户端配置存储不是对象，已停止读写以免丢数据".into()),
        Err(e) => Err(format!("读取客户端配置失败：{e}")),
    }
}

/// 读取某目标的记录列表。
pub fn list_for(store: &FileStore, target: Target) -> Result<Vec<Record>, String> {
    let all = load_all(store)?;
    records_from(&all, target)
}

fn records_from(all: &AllRecords, target: Target) -> Result<Vec<Record>, String> {
    let Some(Value::Array(items)) = all.get(target.id()) else {
        if all.contains_key(target.id()) {
            return Err(format!("{} 的配置存储不是数组，已停止读写", target.id()));
        }
        return Ok(Vec::new());
    };
    items
        .iter()
        .cloned()
        .map(|item| {
            serde_json::from_value::<Record>(item)
                .map_err(|e| format!("{} 的配置记录无法解析：{e}", target.id()))
        })
        .collect()
}

/// 读改写：`mutate` 返回被修改的目标与结果值。全程持锁 + 单次落盘。
pub fn mutate_all<R>(
    store: &FileStore,
    mutate: impl FnOnce(&mut Vec<(Target, Vec<Record>)>) -> Result<R, String>,
) -> Result<R, String> {
    let _guard = super::mutation_lock()?;
    let mut all = load_all(store)?;
    let mut targets: Vec<(Target, Vec<Record>)> = Vec::new();
    for target in Target::ALL {
        targets.push((target, records_from(&all, target)?));
    }
    let result = mutate(&mut targets)?;
    for (target, records) in targets {
        let items = records
            .into_iter()
            .map(|record| serde_json::to_value(record).map_err(|e| format!("序列化配置失败：{e}")))
            .collect::<Result<Vec<_>, _>>()?;
        all.insert(target.id().to_string(), Value::Array(items));
    }
    store
        .put(KV_KEY, &Value::Object(all))
        .map_err(|e| format!("保存客户端配置失败：{e}"))?;
    Ok(result)
}

/// 只读访问某目标（持锁，避免读到半程数据）。
pub fn with_target<R>(
    store: &FileStore,
    target: Target,
    read: impl FnOnce(&[Record]) -> Result<R, String>,
) -> Result<R, String> {
    let _guard = super::mutation_lock()?;
    let all = load_all(store)?;
    let records = records_from(&all, target)?;
    read(&records)
}

/// 新增记录（同一目标至多一份启用：`enabled = true` 时其余自动停用）。
#[allow(clippy::too_many_arguments)]
pub fn create(
    store: &FileStore,
    target: Target,
    name: &str,
    enabled: bool,
    base_url: &str,
    api_key_sealed: &str,
    model: &str,
    models: Vec<Value>,
    options: Value,
    note: &str,
) -> Result<Record, String> {
    let now = super::now_ms();
    let record = Record {
        id: format!("cfg-{}", uuid::Uuid::new_v4().simple()),
        target: target.id().into(),
        name: name.trim().to_string(),
        enabled,
        created_at: now,
        updated_at: now,
        base_url: base_url.trim().trim_end_matches('/').to_string(),
        api_key: api_key_sealed.to_string(),
        model: model.trim().to_string(),
        models,
        options,
        note: note.trim().to_string(),
        last_applied_at: 0,
    };
    let to_save = record.clone();
    mutate_all(store, |targets| {
        let records = slot(targets, target)?;
        if records.len() >= MAX_CONFIGS_PER_TARGET {
            return Err(format!(
                "{} 的配置已达上限 {MAX_CONFIGS_PER_TARGET} 份，请先删除不用的配置",
                target.label()
            ));
        }
        if records.iter().any(|item| item.name == to_save.name) {
            return Err(format!("同名配置「{}」已存在", to_save.name));
        }
        if to_save.enabled {
            for item in records.iter_mut() {
                item.enabled = false;
                item.updated_at = now;
            }
        }
        records.push(to_save.clone());
        Ok(())
    })?;
    Ok(record)
}

/// 更新记录（未传的字段保持原值；`api_key = None` 表示不改）。
///
/// `enabled = Some(true)` 时同一目标的其余记录自动停用；`Some(false)` 停用。
pub struct Update<'a> {
    pub name: Option<&'a str>,
    pub enabled: Option<bool>,
    pub base_url: Option<&'a str>,
    pub api_key_sealed: Option<&'a str>,
    pub clear_api_key: bool,
    pub model: Option<&'a str>,
    pub models: Option<Vec<Value>>,
    pub options: Option<Value>,
    pub note: Option<&'a str>,
}

pub fn update(store: &FileStore, target: Target, id: &str, change: Update<'_>) -> Result<Record, String> {
    let now = super::now_ms();
    let mut saved = None;
    mutate_all(store, |targets| {
        let records = slot(targets, target)?;
        let index = records
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| "配置不存在".to_string())?;
        if let Some(name) = change.name {
            let name = name.trim();
            if records
                .iter()
                .enumerate()
                .any(|(i, item)| i != index && item.name == name)
            {
                return Err(format!("同名配置「{name}」已存在"));
            }
        }
        let mut record = records[index].clone();
        if let Some(name) = change.name {
            record.name = name.trim().to_string();
        }
        if let Some(base_url) = change.base_url {
            record.base_url = base_url.trim().trim_end_matches('/').to_string();
        }
        if change.clear_api_key {
            record.api_key.clear();
        } else if let Some(sealed) = change.api_key_sealed {
            record.api_key = sealed.to_string();
        }
        if let Some(model) = change.model {
            record.model = model.trim().to_string();
        }
        if let Some(models) = change.models {
            record.models = models;
        }
        if let Some(options) = change.options {
            record.options = options;
        }
        if let Some(note) = change.note {
            record.note = note.trim().to_string();
        }
        record.updated_at = now;
        if let Some(enabled) = change.enabled {
            record.enabled = enabled;
            if enabled {
                for (i, item) in records.iter_mut().enumerate() {
                    if i != index && item.enabled {
                        item.enabled = false;
                        item.updated_at = now;
                    }
                }
            }
        }
        records[index] = record.clone();
        saved = Some(record);
        Ok(())
    })?;
    saved.ok_or_else(|| "配置不存在".to_string())
}

/// 切换启用（同一目标同一时刻只启用一份；传 `None` 只是停用）。
pub fn set_enabled(store: &FileStore, target: Target, id: &str, enabled: bool) -> Result<Record, String> {
    update(
        store,
        target,
        id,
        Update {
            name: None,
            enabled: Some(enabled),
            base_url: None,
            api_key_sealed: None,
            clear_api_key: false,
            model: None,
            models: None,
            options: None,
            note: None,
        },
    )
}

/// 删除记录。若删除的是启用中的那份，同一目标不会有别的自动顶上
/// （明确留空 = 不启用任何配置，比「悄悄换成另一份」可预测）。
pub fn delete(store: &FileStore, target: Target, id: &str) -> Result<Record, String> {
    let mut removed = None;
    mutate_all(store, |targets| {
        let records = slot(targets, target)?;
        let index = records
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| "配置不存在".to_string())?;
        removed = Some(records.remove(index));
        Ok(())
    })?;
    removed.ok_or_else(|| "配置不存在".to_string())
}

/// 修改应用时间（记录「最近一次成功写入文件」）。
pub fn mark_applied(store: &FileStore, target: Target, id: &str, at: i64) -> Result<(), String> {
    mutate_all(store, |targets| {
        let records = slot(targets, target)?;
        let record = records
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| "配置不存在".to_string())?;
        record.last_applied_at = at;
        Ok(())
    })
}

fn slot<'a>(
    targets: &'a mut [(Target, Vec<Record>)],
    target: Target,
) -> Result<&'a mut Vec<Record>, String> {
    targets
        .iter_mut()
        .find(|(item, _)| *item == target)
        .map(|(_, records)| records)
        .ok_or_else(|| format!("未知目标：{}", target.id()))
}

/// 记录里凭据的明文（仅在预览/应用计划内部使用，绝不回给接口调用方）。
pub fn plaintext_key(record: &Record) -> String {
    if record.api_key.is_empty() {
        return String::new();
    }
    let dir = crate::account_session::secret::data_dir();
    crate::account_session::secret::open_secret(&record.api_key, &dir)
}

/// 落库前的凭据加密（空值保持空）。
pub fn seal_key(plain: &str) -> String {
    let dir = crate::account_session::secret::data_dir();
    crate::account_session::secret::seal_or_keep(plain.trim(), &dir)
}

/// 目标定义 + 记录列表（`GET /api/admin/client-configs` 的响应体）。
pub fn view(store: &FileStore, target: Option<Target>) -> Result<Value, String> {
    let all = load_all(store)?;
    let targets = match target {
        Some(target) => vec![target],
        None => Target::ALL.to_vec(),
    };
    let mut items = Vec::new();
    for target in targets {
        let records = records_from(&all, target)?;
        items.push(json!({
            "id": target.id(),
            "label": target.label(),
            "format": target.format().as_str(),
            "configPath": target.config_path().display().to_string(),
            "files": targets::target_definitions()
                .into_iter()
                .find(|def| def.get("id").and_then(Value::as_str) == Some(target.id()))
                .and_then(|def| def.get("files").cloned())
                .unwrap_or_else(|| json!([])),
            "configs": records.iter().map(Record::view).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({ "targets": items }))
}

/// 预览令牌的 KV 键（每目标单键，只存最近一次签发的令牌）。
pub fn token_key(target: Target) -> String {
    format!("client_config_preview:{}", target.id())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewToken {
    pub target: String,
    pub record_id: String,
    pub fingerprint: String,
    pub expires_at: i64,
}

pub fn save_token(store: &FileStore, target: Target, token: &PreviewToken) -> Result<(), String> {
    store
        .put(&token_key(target), token)
        .map_err(|e| format!("保存预览令牌失败：{e}"))
}

pub fn take_token(store: &FileStore, target: Target) -> Result<Option<PreviewToken>, String> {
    store
        .get::<PreviewToken>(&token_key(target))
        .map_err(|e| format!("读取预览令牌失败：{e}"))
}

/// 清掉过期令牌（每次签发前顺手清理）。
pub fn purge_expired_token(store: &FileStore, target: Target) -> Result<(), String> {
    if let Some(token) = take_token(store, target)? {
        if token.expires_at < super::now_ms() {
            store
                .delete(&token_key(target))
                .map_err(|e| format!("清理预览令牌失败：{e}"))?;
        }
    }
    Ok(())
}

/// 无条件清掉令牌（应用成功后调用，保证一令牌一用）。
pub fn clear_token(store: &FileStore, target: Target) -> Result<(), String> {
    // 各后端 delete 对「键不存在」都是幂等成功（SQLite DELETE 0 行、JSON
    // 后端先 exists 判断），无需额外容错。
    store
        .delete(&token_key(target))
        .map_err(|e| format!("清理预览令牌失败：{e}"))
}

/// 按记录 id 找到所属目标与记录（id 全局唯一，四个目标共用一次扫描）。
pub fn find(store: &FileStore, id: &str) -> Result<Option<(Target, Record)>, String> {
    let _guard = super::mutation_lock()?;
    let all = load_all(store)?;
    for target in Target::ALL {
        if let Some(record) = records_from(&all, target)?
            .into_iter()
            .find(|record| record.id == id)
        {
            return Ok(Some((target, record)));
        }
    }
    Ok(None)
}