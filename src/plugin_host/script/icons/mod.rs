// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/script/icons/mod.rs
//!
//! 脚本插件图标缓存 —— 把 registry 里的图标引用（data URL / https）拉下来、
//! 按内容摘要落盘，供 `GET /plugins/script-icons/{file}` 直接读。
//!
//! ## 公开接口（plugin-api 依赖，勿改签名）
//!
//! ```rust
//! pub fn read(file: &str) -> Option<(Vec<u8>, &'static str)>; // 异步侧自行 spawn_blocking
//! pub fn wake(force: bool);                                   // 触发一次刷新
//! ```
//!
//! ## 刷新模型（简化版）
//!
//! A2A 有一个后台 worker 定期刷新「全部已装包 + 目录推荐」的图标。AIGX
//! 版保留同样的后台 worker（[`spawn`]）：`wake` 立即唤醒，否则每小时一轮。
//! 每轮从 `script::storage` 读已装包，逐个解析 `icon` 与 `provider_icons`。
//!
//! ## 与 A2A 的差异
//!
//! - 不实现「目录推荐图标」（`recommended`）：AIGX 的 Magpie 目录条目
//!   自带 icon 字段，管理面直接用，不必二次抓取；
//! - 缓存根由 [`install`] 注入（`{script_root}/icons`）。

mod cache;
mod download;
mod formats;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::Notify;

pub use cache::{read, source};

struct Worker {
    wake: Notify,
    force: AtomicBool,
}

fn worker() -> &'static Worker {
    static WORKER: OnceLock<Worker> = OnceLock::new();
    WORKER.get_or_init(|| Worker {
        wake: Notify::new(),
        force: AtomicBool::new(false),
    })
}

/// 注入缓存根（`{script_root}/icons`）；main.rs 启动时调一次。
pub fn install(root: std::path::PathBuf) {
    cache::install(root);
}

/// 触发一次图标刷新（`force` = 忽略 TTL 重拉）。
pub fn wake(force: bool) {
    if force {
        worker().force.store(true, Ordering::Release);
    }
    worker().wake.notify_one();
}

/// 启动后台刷新 worker（幂等：重复调用只起一个）。
///
/// 需要 `script_root` 找包目录与图标缓存根；由 `ScriptHostManager::start`
/// 在启动时调一次。
pub fn spawn(script_root: std::path::PathBuf, store: std::sync::Arc<crate::storage::FileStore>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    install(script_root.clone());
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            loop {
                let force = worker().force.swap(false, Ordering::AcqRel);
                refresh(&script_root, &store, force).await;
                tokio::select! {
                    _ = worker().wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(3600)) => {}
                }
            }
        });
    }
}

/// 一轮刷新：遍历已装包，解析 icon + provider_icons。
async fn refresh(
    script_root: &std::path::Path,
    store: &std::sync::Arc<crate::storage::FileStore>,
    force: bool,
) {
    let packages = crate::plugin_host::script::storage::load_packages(store);
    // 清理不再存在的包的卡片绑定
    let keys: std::collections::HashSet<String> = packages.iter().map(|p| p.id.clone()).collect();
    cache::retain_cards(&keys);
    for package in packages {
        // 包级图标
        if let Some(icon) = package.icon.as_deref().and_then(formats::own) {
            if let Some(file) = cache::resolve(&icon, force).await {
                cache::bind(&package.id, Some(&file));
            }
        }
        // provider 级图标
        for (provider, value) in &package.provider_icons {
            if let Some(value) = value.as_str().and_then(formats::own) {
                if let Some(file) = cache::resolve(&value, force).await {
                    cache::bind(&format!("{}:{provider}", package.id), Some(&file));
                }
            }
        }
    }
    let _ = script_root;
}

/// 一个包的图标 URL 片段（`/plugins/script-icons/{file}`）。
pub fn installed(package: &crate::plugin_host::script::types::ScriptPackage) -> Option<String> {
    cache::card(&package.id)
}

/// 一个 provider 的图标 URL 片段。
pub fn provider(
    package: &crate::plugin_host::script::types::ScriptPackage,
    id: &str,
) -> Option<String> {
    cache::card(&format!("{}:{id}", package.id)).or_else(|| installed(package))
}

/// 归一一个图标声明（管理面展示前用）。
pub fn declaration(value: &str) -> Option<String> {
    formats::own(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_accepts_data_and_https() {
        assert!(declaration("data:image/png;base64,AAA").is_some());
        assert!(declaration("https://example.com/a.svg").is_some());
        assert!(declaration("http://example.com/a.svg").is_none());
        assert!(declaration("bogus").is_none());
    }
}
