// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/provider_settings/mod.rs
//!
//! 插件「提供商设置」—— 声明解析 + 存储读写 + 并发校验（CAS）。
//!
//! ## 谁能编辑什么
//!
//! 插件在握手里为 provider 声明 `settings_schema`（字段清单，见
//! [`schema`]）。运维经管理面「提供商设置」页编辑这些字段，值落 KV
//! （键 `plugin.{storage_provider}.{field}`）。**插件自己不能覆盖**这些
//! 值（`settings.set` 对用户声明字段拒绝——防插件绕过界面改用户配置）。
//!
//! ## 存储形状（AIGX 简化版）
//!
//! A2A 把设置塞进 config 的 `raw` 段（`config::with_raw/update_raw_fields`）。
//! AIGX 的 `AppConfig` 是强类型 + TOML，塞自由键会污染 schema。改为
//! **独立 KV 键** `plugin:provider-settings`：单键存 `{provider: {field:
//! value}}`，与脚本包的 `plugin:script-packs` 平行。整份读写（条目量小）。
//!
//! ## 并发（revision CAS）
//!
//! 每次 `snapshot`/`configure` 算一个 `revision`（声明 + 有效值 + 宿主
//! 代际的 sha256）。`configure(provider, revision, values)` 只有 revision
//! 仍等于当前值才写入——防两个管理页/两个标签页互相覆盖（A2A 同立场）。
//!
//! ## 公开签名（plugin-api 依赖，勿改）
//!
//! ```rust
//! pub fn snapshot() -> serde_json::Value;               // {settings: [...]}
//! pub async fn configure(provider, revision, values) -> Result<Value, ProviderSettingsError>;
//! ```
//!
//! 错误用 [`ProviderSettingsError`]（带 `status_code` + `message`），
//! plugin-api 直接转 HTTP 错误。

pub mod schema;

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::storage::FileStore;

pub use schema::Schema;

/// 存储键（单键持有全部 provider 的设置）。
const KV_KEY: &str = "plugin:provider-settings";

/// 全局存储句柄（main.rs 注入；与 usage 模块同手法，免穿构造链）。
static STORE: OnceLock<Arc<FileStore>> = OnceLock::new();

/// 全局宿主注册表（provider → 声明的 Schema + 宿主代际）。
///
/// 由 `ScriptHostManager` 在宿主重建成功后写入（见
/// [`register_host_schemas`]）；`snapshot/configure` 只读它，不再回查
/// 运行中的宿主（设置页不因宿主临时不可用而整页报错）。
static SCHEMAS: OnceLock<parking_lot::RwLock<BTreeMap<String, RegisteredSchema>>> = OnceLock::new();

fn schemas() -> &'static parking_lot::RwLock<BTreeMap<String, RegisteredSchema>> {
    SCHEMAS.get_or_init(|| parking_lot::RwLock::new(BTreeMap::new()))
}

/// 一个 provider 的注册态：声明 + 所属宿主代际 + 所属包 id。
#[derive(Clone, Debug)]
pub struct RegisteredSchema {
    /// 声明的设置 schema。
    pub schema: Schema,
    /// 所属包 id（`storage_provider` 共用命名空间时用来分组）。
    pub package: String,
    /// provider 展示名。
    pub label: String,
    /// 宿主代际（重建后递增；revision 计入它 → 重建后旧 revision 失效）。
    pub generation: u64,
}

/// 注入存储（main.rs 启动时调一次；重复调用忽略）。
pub fn install(store: Arc<FileStore>) {
    let _ = STORE.set(store);
}

/// 注册一批 provider 的设置声明（宿主重建成功后调用）。
///
/// 覆盖式：先清空旧表再写入——宿主重建后 provider 集合可能变化（包卸载/
/// 停用），残留的旧声明会让设置页显示已不存在的 provider。
pub fn register_host_schemas(entries: Vec<(String, RegisteredSchema)>) {
    let mut table = schemas().write();
    table.clear();
    for (provider, entry) in entries {
        table.insert(provider, entry);
    }
}

/// 清空全部注册（宿主关闭时调）。
pub fn clear_host_schemas() {
    schemas().write().clear();
}

// ── 错误类型 ────────────────────────────────────────────────────────

/// 提供商设置操作错误（带 HTTP 状态码，plugin-api 直接映射）。
#[derive(Debug, Clone)]
pub struct ProviderSettingsError {
    /// HTTP 状态码。
    pub status_code: u16,
    /// 面向用户的文案。
    pub message: String,
}

impl ProviderSettingsError {
    pub fn new(status_code: u16, message: impl Into<String>) -> Self {
        Self {
            status_code,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }
}

impl std::fmt::Display for ProviderSettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ProviderSettingsError {}

// ── 存储读写 ────────────────────────────────────────────────────────

type SettingsMap = BTreeMap<String, Map<String, Value>>;

fn read_all() -> Result<SettingsMap, ProviderSettingsError> {
    let Some(store) = STORE.get() else {
        return Err(ProviderSettingsError::new(503, "设置存储尚未就绪"));
    };
    store
        .get::<SettingsMap>(KV_KEY)
        .map_err(|e| ProviderSettingsError::new(500, format!("读取插件设置失败: {e}")))
        .map(|v| v.unwrap_or_default())
}

fn write_all(map: &SettingsMap) -> Result<(), ProviderSettingsError> {
    let Some(store) = STORE.get() else {
        return Err(ProviderSettingsError::new(503, "设置存储尚未就绪"));
    };
    store
        .put(KV_KEY, map)
        .map_err(|e| ProviderSettingsError::new(500, format!("保存插件设置失败: {e}")))
}

/// 读某 provider 已存的值（按声明字段顺序投影，缺失给 null）。
fn stored_values(all: &SettingsMap, schema: &Schema, provider: &str) -> Map<String, Value> {
    let owner = schema.owner(provider);
    let stored = all.get(owner);
    schema
        .fields
        .iter()
        .map(|field| {
            (
                field.key.clone(),
                stored
                    .and_then(|m| m.get(&field.key))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// 计算 revision（声明 + 值 + 代际的 sha256）。
fn revision(schema: &Schema, values: &Map<String, Value>, generation: u64) -> String {
    let input = json!({
        "schema": schema,
        "values": values,
        "generation": generation,
    });
    let digest = Sha256::digest(input.to_string().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 渲染一个 provider 的设置行（schema + values + effective + invalid + revision）。
fn render(provider: &str, entry: &RegisteredSchema, values: Map<String, Value>) -> Value {
    // 公开值只回标量（防插件把大对象塞进设置撑爆界面）
    let public: Map<String, Value> = values
        .iter()
        .map(|(key, value)| {
            let scalar =
                value.is_null() || value.is_boolean() || value.is_number() || value.is_string();
            (
                key.clone(),
                if scalar && value.to_string().len() <= 16 * 1024 {
                    value.clone()
                } else {
                    Value::Null
                },
            )
        })
        .collect();
    let (effective, invalid) = entry.schema.effective(&values);
    json!({
        "provider": provider,
        "label": entry.label,
        "package": entry.package,
        "schema": entry.schema,
        "values": public,
        "effective": effective,
        "invalid": invalid,
        "revision": revision(&entry.schema, &values, entry.generation),
    })
}

// ── 公开 API（plugin-api 依赖）──────────────────────────────────────

/// 全部 provider 的设置快照：`{settings: [{provider,label,schema,values,…}]}`。
///
/// 同步函数（只读内存表 + 一次 KV 读）；plugin-api 在 handler 里直接调。
pub fn snapshot() -> Value {
    let table = schemas().read().clone();
    let all = read_all().unwrap_or_default();
    let mut settings: Vec<Value> = table
        .iter()
        .map(|(provider, entry)| {
            let values = stored_values(&all, &entry.schema, provider);
            render(provider, entry, values)
        })
        .collect();
    settings.sort_by(|a, b| a["provider"].as_str().cmp(&b["provider"].as_str()));
    json!({ "settings": settings })
}

/// 保存某 provider 的设置（revision CAS）。
///
/// `values` 是 `{field: value}` 补丁（只改给出的字段，未给的保持原值）。
/// `revision` 必须等于当前 revision，否则 409（设置已变化）。
pub async fn configure(
    provider: &str,
    revision_expected: &str,
    values: &Value,
) -> Result<Value, ProviderSettingsError> {
    let entry = schemas()
        .read()
        .get(provider)
        .cloned()
        .ok_or_else(|| ProviderSettingsError::new(404, "插件没有声明可编辑的提供商设置"))?;
    let patch = values
        .as_object()
        .filter(|p| !p.is_empty() && p.len() <= entry.schema.fields.len())
        .ok_or_else(|| ProviderSettingsError::bad_request("插件设置必须是非空对象"))?;
    // 先逐字段归一（未知字段拒绝）
    let mut normalized: BTreeMap<String, Value> = BTreeMap::new();
    for (name, value) in patch {
        let field = entry
            .schema
            .fields
            .iter()
            .find(|f| &f.key == name)
            .ok_or_else(|| ProviderSettingsError::bad_request("不能修改插件未声明的设置"))?;
        normalized.insert(
            name.clone(),
            field
                .normalize(value)
                .map_err(ProviderSettingsError::bad_request)?,
        );
    }
    // CAS：读当前值算 revision，必须与期望一致
    let mut all = read_all()?;
    let current_values = stored_values(&all, &entry.schema, provider);
    let current_revision = revision(&entry.schema, &current_values, entry.generation);
    if current_revision != revision_expected {
        return Err(ProviderSettingsError::new(
            409,
            "插件或设置已变化，请刷新后再保存",
        ));
    }
    // 写入（owner 命名空间下逐字段合并）
    let owner = entry.schema.owner(provider).to_string();
    let slot = all.entry(owner).or_default();
    for (name, value) in &normalized {
        if value.is_null() {
            slot.remove(name);
        } else {
            slot.insert(name.clone(), value.clone());
        }
    }
    write_all(&all)?;
    let updated = stored_values(&all, &entry.schema, provider);
    let mut result = render(provider, &entry, updated);
    result["saved"] = json!(true);
    Ok(result)
}

/// 某 provider 是否有可编辑的设置（管理面据此决定是否显示「设置」入口）。
pub fn available(provider: &str) -> bool {
    schemas().read().contains_key(provider)
}

/// provider 的字段键集合（供 `settings.set` 拒绝覆盖用户声明字段）。
pub fn user_owned(provider: &str, field: &str) -> bool {
    schemas()
        .read()
        .get(provider)
        .is_some_and(|entry| entry.schema.has_field(field))
}

/// 存储命名空间（多个 provider 共用 `storage_provider` 时同键）。
pub fn storage_provider(provider: &str, field: &str) -> String {
    schemas()
        .read()
        .get(provider)
        .filter(|entry| entry.schema.has_field(field))
        .map(|entry| entry.schema.owner(provider).to_string())
        .unwrap_or_else(|| provider.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 全局表 + 全局 store 是进程级单例，测试必须串行（`register_host_schemas`
    /// 是覆盖式清空，并行跑会互相抹掉对方注册的 provider）。
    ///
    /// 用 tokio Mutex：async 测试里持有 std guard 跨 await 会被 clippy 判
    /// `await_holding_lock`（这是真的——串行锁本来就该跨 await，但 std 锁
    /// 跨 await 是反模式，换成 async 锁语义更正确）。
    fn serial() -> &'static tokio::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn with_store<R>(f: impl FnOnce() -> R) -> R {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let dir = tempfile::TempDir::new().unwrap();
            let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
            std::mem::forget(dir);
            install(store);
        });
        f()
    }

    fn register(provider: &str) {
        let schema_value = json!({
            "version": 1,
            "title": "设置",
            "fields": [
                { "key": "region", "label": "区域", "type": "string", "max_length": 8, "default": "cn" },
                { "key": "retries", "label": "重试", "type": "number", "minimum": 0.0, "maximum": 10.0 }
            ]
        });
        let schema = schema::parse(provider, &schema_value).unwrap().unwrap();
        register_host_schemas(vec![(
            provider.to_string(),
            RegisteredSchema {
                schema,
                package: "pkg".into(),
                label: "测试家".into(),
                generation: 1,
            },
        )]);
    }

    #[test]
    fn snapshot_includes_registered_provider() {
        let _serial = serial().blocking_lock();
        with_store(|| {
            register("snap-provider");
            let snap = snapshot();
            let settings = snap["settings"].as_array().unwrap();
            assert!(settings.iter().any(|s| s["provider"] == "snap-provider"));
            let row = settings
                .iter()
                .find(|s| s["provider"] == "snap-provider")
                .unwrap();
            assert_eq!(row["label"], "测试家");
            assert!(row["revision"].as_str().is_some());
            assert!(row["schema"]["fields"].is_array());
        });
    }

    #[tokio::test]
    async fn configure_cas_roundtrip() {
        let _serial = serial().lock().await;
        with_store(|| ());
        register("cas-provider");
        let snap = snapshot();
        let row = snap["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["provider"] == "cas-provider")
            .unwrap()
            .clone();
        let rev = row["revision"].as_str().unwrap().to_string();
        // 正确 revision → 写入成功
        let out = configure("cas-provider", &rev, &json!({"region": "us"}))
            .await
            .unwrap();
        assert_eq!(out["saved"], true);
        assert_eq!(out["values"]["region"], "us");
        // 旧 revision 再写 → 409
        let err = configure("cas-provider", &rev, &json!({"region": "eu"}))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, 409);
    }

    #[tokio::test]
    async fn configure_rejects_unknown_field_and_bad_value() {
        let _serial = serial().lock().await;
        with_store(|| ());
        register("reject-provider");
        let snap = snapshot();
        let row = snap["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["provider"] == "reject-provider")
            .unwrap()
            .clone();
        let rev = row["revision"].as_str().unwrap().to_string();
        // 未声明字段
        let err = configure("reject-provider", &rev, &json!({"nope": 1}))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, 400);
        // 越界数字
        let err = configure("reject-provider", &rev, &json!({"retries": 99}))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, 400);
        // 未知 provider → 404
        let err = configure("nope-provider", &rev, &json!({"region": "x"}))
            .await
            .unwrap_err();
        assert_eq!(err.status_code, 404);
    }

    #[test]
    fn user_owned_detects_declared_fields() {
        let _serial = serial().blocking_lock();
        with_store(|| ());
        register("owned-provider");
        assert!(user_owned("owned-provider", "region"));
        assert!(!user_owned("owned-provider", "undeclared"));
        assert!(!user_owned("missing-provider", "region"));
    }
}
