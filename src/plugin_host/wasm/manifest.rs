// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Wasm 插件的权限清单（`wasm.json`）读取与校验。
//!
//! ## 与上游的差异（仅依赖形态，校验规则逐条保留）
//!
//! 上游的 `read(dir, &proto::PackManifest)` 拿的是 `provider-protocol` crate
//! 的强类型包清单；AIGX 没有该 crate，且 [`crate::plugin_host::install::PackManifest`]
//! 的 `entry` 是「字符串 or {三元组: 路径}」双形态。本模块因此收
//! `pack: &serde_json::Value`（`pack.json` 的原始 JSON），自己从里面取
//! `entry` / `providers` / `ui` —— 这样 wasm 模块对插件包清单的**唯一**依赖
//! 就是 JSON 形状，integrator 传 `serde_json::to_value(pack)` 即可。
//!
//! ## 校验清单（与上游逐条对应）
//!
//! - 入口目标必须是 `wasm32-unknown-unknown`；`wasm.json` 存在但入口不是
//!   wasm → 报错（「非 Wasm 插件不能声明 Wasm 权限」）
//! - 只允许一个提供商、一个跨平台入口、不带可执行网页（`ui`）
//! - `wasm.json` ≤ 32 KiB、ABI 必须为 [`ABI`]
//! - 能力必须在 [`CAPABILITIES`] 白名单内，且带依赖声明
//! - 内存 8–128 MiB、并发 1–16
//! - 原生辅助：≤8 项、标识字符集受限、必须声明 `native.helpers.v1`
//! - 网络来源：≤32 条、必须是精确 HTTPS origin（无路径/查询/凭据/通配符）
//! - 入口必须是包内普通文件（canonicalize 后仍在包目录内）

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::wire::WireError;

/// 入口目标三元组（与上游同名）。
pub const TARGET: &str = "wasm32-unknown-unknown";

/// 沙箱 ABI 版本。插件导出 `a2a_abi_version()` 必须返回它。
pub const ABI: u32 = 1;

/// 能力白名单（对齐 Agent2API v3.5.0-beta.6 `CAPABILITIES`，逐字一致）。
///
/// 这是**插件可以在 `wasm.json` 里声明**的能力全集。达到全集后，beta.2 的
/// Wasm 插件（声明 compute/scheduling/tasks/desktopLaunch/verification/
/// managedLogin/sms 等新能力）都能通过清单校验；宿主是否**真正服务**某项
/// 能力见 [`SERVED_CAPABILITIES`]（未服务的能力在调用时如实报不支持）。
///
/// beta.4 变更：上游删除了 `credential.cline-free-refresh.v1`（Cline Free
/// 续期限制整体移除），本表同步删除。
/// beta.6 变更：上游删除了 `credential.codearts-refresh.v1`（CodeArts 续期
/// 撤离），本表同步删除；其余项与 beta.4 一致。
pub const CAPABILITIES: &[&str] = &[
    "http.v1",
    "http.timeout.v1",
    "websocket.v1",
    "accounts.cas.v1",
    "settings.v1",
    "sessions.v1",
    "clock.v1",
    "random.v1",
    "locks.v1",
    "catalog.v1",
    "chat.stream.v1",
    "login.forms.v1",
    "checkin.rolling.v1",
    "errors.v1",
    "accounts.identity.v1",
    "catalog.accounts.v1",
    "state.v1",
    "resources.v1",
    "images.v1",
    "tasks.v1",
    "login.interaction.v1",
    "login.callback.v1",
    "login.captcha.geetest.v1",
    "login.captcha.alicom4.v1",
    "native.helpers.v1",
    "settings.schema.v1",
    "login.sms.v1",
    "login.device.v1",
    "resources.fetch-public.v1",
    "protocol.convert.v1",
    "http.account-origin.v1",
    "runtime.platform.v1",
    "verification.account.v1",
    "verification.claim.v1",
    "runtime.architecture.v1",
    "runtime.client-fingerprint.v1",
    "request.context.v1",
    "login.managed.v1",
    "login.loopback.v1",
    "login.captcha.aliyun.v1",
    "resources.inbound-images.v1",
    "login.desktop.chatglm.v1",
    "http.catpaw-turn.v1",
    "claim.doubao-tasks.v1",
    "http.managed-task.v1",
    "http.codearts-session.v1",
    "login.codearts.v1",
    "desktop.credentials.v1",
    "http.doubao-turn.v1",
    "crypto.monkeycode-cap.v1",
    "crypto.deepseek-pow.v1",
    "desktop.launch.v1",
    "resources.cookie-file.v1",
    "credential.kiro-external-refresh.v1",
    "credentials.legacy-import.v1",
];

/// 宿主**实际落地的能力**（其余声明在 [`CAPABILITIES`] 里只做前向兼容放行，
/// 调用时返回「未支持」而非假装支持）。
///
/// 与 `services.rs` 的能力闸门逐条对应：每个方法在分派前都会
/// `permission(name)` 校验，未授权 → `forbidden`；未在 [`CAPABILITIES`]
/// 白名单 → 清单校验阶段即拒绝加载。
pub const SERVED_CAPABILITIES: &[&str] = &[
    "http.v1",
    "websocket.v1",
    "accounts.cas.v1",
    "settings.v1",
    "sessions.v1",
    "clock.v1",
    "random.v1",
    "locks.v1",
    "catalog.v1",
    "catalog.accounts.v1",
    "chat.stream.v1",
    "state.v1",
    "resources.v1",
    "tasks.v1",
    "native.helpers.v1",
    "settings.schema.v1",
    "accounts.identity.v1",
    "login.forms.v1",
    "login.interaction.v1",
    "login.callback.v1",
    "login.captcha.geetest.v1",
    // 注意：`checkin.rolling.v1` **不在**已服务表里。上游把它当作「插件执行签到调用」
    // 的闸门（`invoke_checkin` 路径）；AIGX 的签到在原生 `src/checkin/*` 里完成，
    // wasm 宿主没有对应的 checkin 执行入口。声明放行（`CAPABILITIES`）保留前向兼容，
    // 但不在这里虚报已服务——否则插件会以为签到走宿主，实际拿到 NO_METHOD。
    "errors.v1",
    // `verification.claim.v1` 闸两样东西：租约（`verification.lease.*`，AIGX 已实现）
    // 与三个验证执行动作（`captcha.push` / `claim.verification.poll|complete`，AIGX
    // 没有验证执行器）。租约是真服务的，所以这项留在已服务表；三个动作在
    // `contract()` 里由 `servedActions`（= `verification.account.v1`）单独标 false。
    "verification.claim.v1",
    "http.managed-task.v1",
    "http.account-origin.v1",
    "crypto.deepseek-pow.v1",
    // 本机客户端登录态读取（`desktop.credentials.read`）：只读固定来源，服务
    // workbuddy / raccoon / kiro / kuku / loomy / catpaw 等本机账号池导入。
    "desktop.credentials.v1",
    // 运行环境两项（`runtime.platform` / `runtime.architecture`）：只回本进程常量。
    "runtime.platform.v1",
    "runtime.architecture.v1",
    // 请求上下文（`request.context`）：仅对话请求有值，其余调用如实 forbidden。
    "request.context.v1",
    // Kiro 外部 IdP 续期：端点由插件写进凭据，宿主用注入 egress 换证 + CAS 防重。
    "credential.kiro-external-refresh.v1",
    // 插件侧公网图片下载：只产本次调用资源，不支持的文件类型/跳转一律失败。
    "resources.fetch-public.v1",
];

/// 某项能力是否由宿主实际服务。
pub fn served(capability: &str) -> bool {
    SERVED_CAPABILITIES.contains(&capability)
}

/// `wasm.json` 的磁盘形状（`deny_unknown_fields`，与上游一致）。
///
/// beta.2 在清单里新增了 `account_origin` / `desktop_launch` / `managed_login`
/// / `managed_task` / `concurrency_hard_limit` 等声明键。本结构体**接受**
/// 这些键（前向兼容，缺失即 `None`），但宿主目前只消费核心字段；扩展声明
/// 由对应子系统（若已实现）在自己的模块里按原始 JSON 解读。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub abi: u32,
    pub requires: Vec<String>,
    pub origins: Vec<String>,
    // ── beta.2 清单扩展（前向兼容放行；宿主是否消费见各子系统）──────────
    #[serde(default)]
    pub account_origin: Option<serde_json::Value>,
    #[serde(default)]
    pub desktop_launch: Option<serde_json::Value>,
    #[serde(default)]
    pub managed_login: Option<serde_json::Value>,
    #[serde(default)]
    pub managed_task: Option<serde_json::Value>,
    #[serde(default)]
    pub concurrency_hard_limit: Option<usize>,
    #[serde(default)]
    pub helpers: Vec<String>,
    #[serde(default = "memory_default")]
    pub memory_mib: usize,
    #[serde(default = "concurrency_default")]
    pub concurrency: usize,
    #[serde(default)]
    pub verified: bool,
}

fn memory_default() -> usize {
    64
}

fn concurrency_default() -> usize {
    8
}

/// 一个通过校验的 Wasm 插件声明（= 上游 `manifest::Options`）。
///
/// `provider` 来自包清单的 `providers[0].id`；其余来自 `wasm.json`。
#[derive(Clone, Debug)]
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct WasmManifest {
    pub provider: String,
    pub abi: u32,
    pub requires: Vec<String>,
    pub origins: Vec<String>,
    pub helpers: Vec<String>,
    pub memory_mib: usize,
    pub concurrency: usize,
    pub verified: bool,
    // beta.2 清单扩展声明（原始 JSON，供对应子系统按需解读）。
    pub account_origin: Option<serde_json::Value>,
    pub desktop_launch: Option<serde_json::Value>,
    pub managed_login: Option<serde_json::Value>,
    pub managed_task: Option<serde_json::Value>,
    pub concurrency_hard_limit: Option<usize>,
}

impl WasmManifest {
    /// 插件是否声明了某项能力。
    pub fn allows(&self, capability: &str) -> bool {
        self.requires.iter().any(|item| item == capability)
    }

    /// 校验出站地址：必须 HTTPS、无凭据/片段，且 origin 在已授权来源内。
    ///
    /// `websocket=true` 时允许 `wss://`（内部换算成 https 再比对 origin）。
    pub fn check_url(&self, value: &str, websocket: bool) -> Result<(), WireError> {
        let mut url = url::Url::parse(value).map_err(|_| super::wire::error("插件请求地址无效"))?;
        if websocket && url.scheme() == "wss" {
            url.set_scheme("https")
                .map_err(|_| super::wire::error("插件长连接地址无效"))?;
        }
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !self.origins.contains(&url.origin().ascii_serialization())
        {
            return Err(WireError::with_code(
                "插件请求地址未获网络授权",
                "forbidden",
            ));
        }
        Ok(())
    }
}

/// 包清单是否把入口声明成 wasm 目标。
pub fn is_wasm(pack: &serde_json::Value) -> bool {
    pack.get("entry")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|entry| entry.contains_key(TARGET))
}

/// 取 wasm 入口路径（对象形态 `{TARGET: path}`）。
fn wasm_entry(pack: &serde_json::Value) -> Option<&str> {
    pack.get("entry")
        .and_then(serde_json::Value::as_object)
        .and_then(|entry| entry.get(TARGET))
        .and_then(serde_json::Value::as_str)
}

/// 包清单里的提供商 id 列表（只取 `id` 字符串）。
fn provider_ids(pack: &serde_json::Value) -> Vec<&str> {
    pack.get("providers")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(serde_json::Value::as_str))
                .collect()
        })
        .unwrap_or_default()
}

/// 包清单是否声明了可执行网页（`ui` 字段或包内 `ui/` 目录）。
fn has_ui(pack: &serde_json::Value, dir: &Path) -> bool {
    match pack.get("ui") {
        Some(serde_json::Value::Null) | None => dir.join("ui").exists(),
        Some(serde_json::Value::Array(items)) => !items.is_empty() || dir.join("ui").exists(),
        Some(_) => true,
    }
}

/// 读取并校验 `<dir>/wasm.json`，返回 `None` 表示这不是 Wasm 插件。
pub fn read_manifest(dir: &Path, pack: &serde_json::Value) -> Result<Option<WasmManifest>, String> {
    if !is_wasm(pack) {
        if dir.join("wasm.json").exists() {
            return Err("非 Wasm 插件不能声明 Wasm 权限".into());
        }
        return Ok(None);
    }
    let entries = pack
        .get("entry")
        .and_then(serde_json::Value::as_object)
        .map_or(0, serde_json::Map::len);
    let providers = provider_ids(pack);
    if entries != 1 || providers.len() != 1 {
        return Err("Wasm 插件包必须只声明一个提供商和一个跨平台入口".into());
    }
    if has_ui(pack, dir) {
        return Err("Wasm 插件只能使用主程序的声明式界面，不能携带可执行网页".into());
    }
    let text = std::fs::read(dir.join("wasm.json")).map_err(|_| "Wasm 插件缺少 wasm.json")?;
    if text.len() > 32 * 1024 {
        return Err("Wasm 权限清单过大".into());
    }
    let spec: Manifest = serde_json::from_slice(&text).map_err(|_| "Wasm 权限清单格式无效")?;
    if spec.abi != ABI {
        return Err(format!("插件需要 Wasm 接口 {}，当前支持 {}", spec.abi, ABI));
    }
    if spec.requires.len() > CAPABILITIES.len() {
        return Err("Wasm 能力声明过多".into());
    }
    for capability in &spec.requires {
        if !CAPABILITIES.contains(&capability.as_str()) {
            return Err(format!("当前网关不支持插件所需能力：{capability}"));
        }
    }
    let requires = |name: &str| spec.requires.iter().any(|item| item == name);
    for (capability, dependencies) in [
        ("settings.schema.v1", &["settings.v1"][..]),
        (
            "catalog.accounts.v1",
            &["catalog.v1", "accounts.cas.v1"][..],
        ),
        ("accounts.identity.v1", &["accounts.cas.v1"][..]),
        // beta.7 收窄：`desktop.launch.v1` 只要求 `accounts.cas.v1`，不再隐含
        // 要求 `accounts.identity.v1`（上游 v3.5.0-beta.7 同口径）。
        ("desktop.launch.v1", &["accounts.cas.v1"][..]),
        ("login.interaction.v1", &["login.forms.v1", "state.v1"][..]),
        ("login.captcha.geetest.v1", &["login.interaction.v1"][..]),
    ] {
        if requires(capability) && dependencies.iter().any(|dependency| !requires(dependency)) {
            return Err(format!("Wasm 能力 {capability} 缺少依赖声明"));
        }
    }
    if !(8..=128).contains(&spec.memory_mib) || !(1..=16).contains(&spec.concurrency) {
        return Err("Wasm 资源声明超出允许范围（内存 8–128 MiB，并发 1–16）".into());
    }
    if spec.helpers.len() > 8
        || spec.helpers.iter().any(|helper| {
            helper.is_empty()
                || helper.len() > 80
                || !helper
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        })
        || !spec.helpers.is_empty() && !requires("native.helpers.v1")
    {
        return Err("Wasm 原生辅助能力声明无效".into());
    }
    // 上限 64（原 32）：多区域 OIDC provider（如 kiro 声明 41 个：kiro.dev 的
    // app/auth/management/runtime + 各 AWS 区域 oidc 端点）是合法需求，32 会误伤。
    // 仍严格逐条校验（https、无通配符、无路径），64 个明确 HTTPS origin 不构成滥用。
    if spec.origins.len() > 64 {
        return Err("Wasm 网络地址声明过多".into());
    }
    for origin in &spec.origins {
        let url = url::Url::parse(origin).map_err(|_| "Wasm 网络地址格式无效")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || url.origin().ascii_serialization() != *origin
        {
            return Err("Wasm 网络权限必须是明确的 HTTPS 来源，不允许通配符或路径".into());
        }
    }
    let entry = wasm_entry(pack).ok_or("Wasm 入口不存在")?;
    if !entry.ends_with(".wasm") {
        return Err("Wasm 入口必须是 .wasm 文件".into());
    }
    checked_entry(dir, entry)?;
    Ok(Some(WasmManifest {
        provider: providers[0].to_string(),
        abi: spec.abi,
        requires: spec.requires,
        origins: spec.origins,
        helpers: spec.helpers,
        memory_mib: spec.memory_mib,
        concurrency: spec.concurrency,
        verified: spec.verified,
        account_origin: spec.account_origin,
        desktop_launch: spec.desktop_launch,
        managed_login: spec.managed_login,
        managed_task: spec.managed_task,
        concurrency_hard_limit: spec.concurrency_hard_limit,
    }))
}

/// 校验入口路径是包内普通文件，返回 canonicalize 后的绝对路径。
pub fn checked_entry(dir: &Path, entry: &str) -> Result<PathBuf, String> {
    let root = dir.canonicalize().map_err(|_| "插件目录不可读")?;
    let path = dir
        .join(entry)
        .canonicalize()
        .map_err(|_| "插件入口不存在")?;
    if !path.starts_with(root) || !path.is_file() {
        return Err("插件入口必须是包内的普通文件".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn pack(provider: &str) -> serde_json::Value {
        json!({
            "id": "demo",
            "entry": { TARGET: "plugin.wasm" },
            "providers": [{ "id": provider, "label": "Demo" }]
        })
    }

    fn write_plugin(dir: &Path, _wasm: &str, spec: serde_json::Value) {
        fs::write(dir.join("plugin.wasm"), b"\0asm\x01\0\0\0").unwrap();
        fs::write(dir.join("wasm.json"), spec.to_string()).unwrap();
    }

    fn minimal(requires: &[&str]) -> serde_json::Value {
        json!({ "abi": ABI, "requires": requires, "origins": [] })
    }

    #[test]
    fn reads_minimal_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "x", minimal(&["clock.v1"]));
        let m = read_manifest(tmp.path(), &pack("demo"))
            .unwrap()
            .expect("is wasm");
        assert_eq!(m.provider, "demo");
        assert_eq!(m.memory_mib, 64);
        assert_eq!(m.concurrency, 8);
        assert!(m.allows("clock.v1"));
        assert!(!m.allows("http.v1"));
    }

    #[test]
    fn non_wasm_with_wasm_json_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("wasm.json"), "{}").unwrap();
        let pack = json!({ "entry": "bin/x", "providers": [{ "id": "demo" }] });
        assert!(read_manifest(tmp.path(), &pack).is_err());
    }

    #[test]
    fn non_wasm_without_json_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let pack = json!({ "entry": "bin/x", "providers": [{ "id": "demo" }] });
        assert!(read_manifest(tmp.path(), &pack).unwrap().is_none());
    }

    #[test]
    fn unknown_capability_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "x", minimal(&["teleport.v1"]));
        let err = read_manifest(tmp.path(), &pack("demo")).unwrap_err();
        assert!(err.contains("teleport.v1"), "{err}");
    }

    #[test]
    fn deepseek_pow_capability_is_served_and_accepted() {
        // 必须同时出现在白名单与真正服务清单里，否则插件 `requires` 会被拒。
        assert!(
            served("crypto.deepseek-pow.v1"),
            "crypto.deepseek-pow.v1 必须由宿主实际服务"
        );
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "x", minimal(&["crypto.deepseek-pow.v1"]));
        assert!(
            read_manifest(tmp.path(), &pack("deepseek-web"))
                .unwrap()
                .is_some(),
            "deepseek-web 插件声明该能力应能通过清单校验"
        );
    }

    #[test]
    fn capability_dependencies_enforced() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "x", minimal(&["catalog.accounts.v1"]));
        let err = read_manifest(tmp.path(), &pack("demo")).unwrap_err();
        assert!(err.contains("依赖"), "{err}");
        // 依赖齐全则通过
        write_plugin(
            tmp.path(),
            "x",
            minimal(&["catalog.accounts.v1", "catalog.v1", "accounts.cas.v1"]),
        );
        assert!(read_manifest(tmp.path(), &pack("demo")).unwrap().is_some());
    }

    /// beta.7 收窄：`desktop.launch.v1` 只要求 `accounts.cas.v1`（不再要求
    /// `accounts.identity.v1`）。正反两个对照都必须成立，否则「不要求 identity」
    /// 的断言是空的。
    #[test]
    fn desktop_launch_requires_only_accounts_cas() {
        let tmp = tempfile::tempdir().unwrap();
        // 负向：缺 accounts.cas.v1 → 拒绝
        write_plugin(tmp.path(), "x", minimal(&["desktop.launch.v1"]));
        let err = read_manifest(tmp.path(), &pack("demo")).unwrap_err();
        assert!(err.contains("依赖"), "{err}");
        // 正向：accounts.cas.v1 + desktop.launch.v1（无 accounts.identity.v1）→ 通过
        write_plugin(
            tmp.path(),
            "x",
            minimal(&["desktop.launch.v1", "accounts.cas.v1"]),
        );
        assert!(read_manifest(tmp.path(), &pack("demo")).unwrap().is_some());
        // 附带 accounts.identity.v1 也仍通过：identity 不是 desktop.launch 的
        // 硬依赖（给了不报错，不给也不报错）。
        write_plugin(
            tmp.path(),
            "x",
            minimal(&[
                "desktop.launch.v1",
                "accounts.identity.v1",
                "accounts.cas.v1",
            ]),
        );
        assert!(read_manifest(tmp.path(), &pack("demo")).unwrap().is_some());
    }

    #[test]
    fn memory_and_concurrency_bounds() {
        let tmp = tempfile::tempdir().unwrap();
        for (mib, conc, ok) in [
            (8usize, 1usize, true),
            (128, 16, true),
            (7, 8, false),
            (129, 8, false),
            (64, 0, false),
            (64, 17, false),
        ] {
            let spec = json!({ "abi": ABI, "requires": [], "origins": [],
                "memory_mib": mib, "concurrency": conc });
            write_plugin(tmp.path(), "x", spec);
            let got = read_manifest(tmp.path(), &pack("demo"));
            assert_eq!(got.is_ok(), ok, "memory={mib} concurrency={conc}: {got:?}");
        }
    }

    #[test]
    fn abi_must_match() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(
            tmp.path(),
            "x",
            json!({ "abi": 2, "requires": [], "origins": [] }),
        );
        assert!(read_manifest(tmp.path(), &pack("demo")).is_err());
    }

    #[test]
    fn origins_must_be_exact_https_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let good = json!({ "abi": ABI, "requires": ["http.v1"],
            "origins": ["https://api.example.com"] });
        write_plugin(tmp.path(), "x", good);
        let m = read_manifest(tmp.path(), &pack("demo")).unwrap().unwrap();
        assert!(m.check_url("https://api.example.com/v1/x", false).is_ok());
        assert!(m.check_url("https://evil.example.com/x", false).is_err());
        assert!(m.check_url("http://api.example.com/x", false).is_err());

        for bad in [
            "https://api.example.com/",
            "https://api.example.com/path",
            "https://user:pass@api.example.com",
            "https://api.example.com/?q=1",
            "http://api.example.com",
        ] {
            let spec = json!({ "abi": ABI, "requires": ["http.v1"], "origins": [bad] });
            write_plugin(tmp.path(), "x", spec);
            assert!(
                read_manifest(tmp.path(), &pack("demo")).is_err(),
                "should reject origin {bad}"
            );
        }
    }

    #[test]
    fn helpers_require_capability_and_charset() {
        let tmp = tempfile::tempdir().unwrap();
        let spec = json!({ "abi": ABI, "requires": [], "origins": [], "helpers": ["h1"] });
        write_plugin(tmp.path(), "x", spec);
        assert!(read_manifest(tmp.path(), &pack("demo")).is_err());

        let spec = json!({ "abi": ABI, "requires": ["native.helpers.v1"],
            "origins": [], "helpers": ["bad helper"] });
        write_plugin(tmp.path(), "x", spec);
        assert!(read_manifest(tmp.path(), &pack("demo")).is_err());

        let spec = json!({ "abi": ABI, "requires": ["native.helpers.v1"],
            "origins": [], "helpers": ["helper-1"] });
        write_plugin(tmp.path(), "x", spec);
        assert!(read_manifest(tmp.path(), &pack("demo")).unwrap().is_some());
    }

    #[test]
    fn ui_payload_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "x", minimal(&[]));
        let mut with_ui = pack("demo");
        with_ui["ui"] = json!(["index.html"]);
        assert!(read_manifest(tmp.path(), &with_ui).is_err());
        // 包内 ui/ 目录同样拒绝
        let mut plain = pack("demo");
        plain.as_object_mut().unwrap().remove("ui");
        fs::create_dir_all(tmp.path().join("ui")).unwrap();
        assert!(read_manifest(tmp.path(), &plain).is_err());
    }

    #[test]
    fn entry_must_be_wasm_file_inside_pack() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "x", minimal(&[]));
        let mut bad = pack("demo");
        bad["entry"] = json!({ TARGET: "plugin.js" });
        assert!(read_manifest(tmp.path(), &bad).is_err());
        let mut missing = pack("demo");
        missing["entry"] = json!({ TARGET: "missing.wasm" });
        assert!(read_manifest(tmp.path(), &missing).is_err());
    }

    #[test]
    fn multiple_providers_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(tmp.path(), "x", minimal(&[]));
        let mut multi = pack("demo");
        multi["providers"] = json!([{ "id": "a" }, { "id": "b" }]);
        assert!(read_manifest(tmp.path(), &multi).is_err());
    }

    #[test]
    fn capability_whitelist_matches_beta6() {
        // 对齐 Agent2API v3.5.0-beta.6 CAPABILITIES 全集（beta.4 移除
        // credential.cline-free-refresh.v1，beta.6 移除 credential.codearts-refresh.v1），
        // +crypto.deepseek-pow.v1（#62，AIGX 扩展超 beta6）=> 55 项。
        assert_eq!(CAPABILITIES.len(), 55);
        assert!(!CAPABILITIES.contains(&"credential.cline-free-refresh.v1"));
        assert!(!CAPABILITIES.contains(&"credential.codearts-refresh.v1"));
        let unique: std::collections::HashSet<_> = CAPABILITIES.iter().collect();
        assert_eq!(unique.len(), CAPABILITIES.len());
        // 已服务能力必须是白名单子集，不能广告白名单外的东西。
        for capability in SERVED_CAPABILITIES {
            assert!(
                CAPABILITIES.contains(capability),
                "SERVED 含白名单外能力: {capability}"
            );
        }
    }
}
