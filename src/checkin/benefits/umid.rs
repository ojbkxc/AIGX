//! Qoder 国际版 UMID 组件 —— **机器级**（一次安装服务所有国际版账号）。
//!
//! 来源：Agent2API `providers/qoder/risk.rs`。背景：Qoder 国际版服务端
//! **只对带风控头的请求**下发「每天领 100 Credits」的活动；不带风控头时
//! 活动列表里永远只有促销项。那三个头（`Cosy-MachineToken` /
//! `Cosy-MachineCode` / `Cosy-MachineType`）由 Qoder 自带的 **UMID 程序
//! `runtime-info`** 在本机生成 —— 它是官方客户端随包分发的一个可执行文件，
//! 调用后首行输出一段 JSON。
//!
//! ## AIGX 的现状与取舍
//!
//! 我们**不分发**那个二进制：它属于上游闭源组件，网关打包分发既不合规也不
//! 安全（来源不可控、无法审计）。所以这里只做两件事：
//!
//! 1. **探测**：看本机是否已存在 `runtime-info`（官方客户端装在服务器上时
//!    可能就有），存在则报「可用」，此时国际版账号的每日领取能正常工作；
//! 2. **指路**：不存在时返回明确说明 —— 需要管理员自行准备该组件并放到
//!    `AIGX_UMID_PATH` 指向的位置，我们绝不代为下载。
//!
//! 这样界面不会出现一个点了没反应的按钮，也不会把「无法自动完成」伪装成
//! 失败重试。

use serde_json::{json, Value};

use super::BenefitResult;

/// 允许管理员覆盖组件路径的环境变量名。
pub const UMID_PATH_ENV: &str = "AIGX_UMID_PATH";

/// 已知的候选路径（官方客户端默认安装位置）。
fn candidates() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Ok(custom) = std::env::var(UMID_PATH_ENV) {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            paths.push(std::path::PathBuf::from(trimmed));
        }
    }
    // Linux 服务器上官方客户端/Hub 的常见落点
    for guess in [
        "/usr/local/bin/runtime-info",
        "/opt/qoder/runtime-info",
        "/root/.qoder/runtime-info",
        "/usr/lib/qoder/runtime-info",
    ] {
        paths.push(std::path::PathBuf::from(guess));
    }
    paths
}

/// 探测组件可用性（不执行它，只看文件在不在）。
pub fn detect() -> Option<std::path::PathBuf> {
    candidates().into_iter().find(|path| path.is_file())
}

/// 状态视图（本通道唯一动作；不做「安装」）。
pub fn status() -> BenefitResult {
    let data: Value = match detect() {
        Some(path) => json!({
            "available": true,
            "path": path.display().to_string(),
            "note": "已检测到 UMID 组件，国际版账号的每日 100 Credits 活动可正常领取",
        }),
        None => json!({
            "available": false,
            "env": UMID_PATH_ENV,
            "note": "未检测到 UMID 组件。Qoder 国际版需要它生成风控头才能领「每日 100 Credits」；\
                     请自行准备官方客户端的 runtime-info 并把路径写入该环境变量后重启服务。\
                     AIGX 不分发该闭源组件。",
        }),
    };
    if data["available"].as_bool() == Some(true) {
        BenefitResult::done("UMID 组件已就绪").with_data(data)
    } else {
        BenefitResult::skipped("UMID 组件未安装（需管理员自行准备）").with_data(data)
    }
}
