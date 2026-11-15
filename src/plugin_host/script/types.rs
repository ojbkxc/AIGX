//! 脚本插件的共享类型契约 —— 七个实现模块共用的数据结构。
//!
//! 这些类型是**团队契约**：`types` 由 script-core 维护，pkg-installer
//! （安装落盘）、magpie-catalog（市场目录）、js-host（Bun 侧宿主）、
//! runtime/operations（生命周期）都按这里的字段名读写。改字段 = 改契约，
//! 必须同步通知全部下游。
//!
//! ## 存储布局（与 pkg-installer 对齐）
//!
//! ```text
//! {data_dir}/script-plugins/          ← script_plugins_root(data_dir)
//! ├── packages/{script-{sha256(id)[..24]}}/{version}/  ← npm 风格目录包（package.json + 入口）
//! ├── options/{package_id}.json         ← 运维配置（≤ MAX_SETTINGS_BYTES）
//! └── bun/                              ← Bun 运行时（bun-runtime 成员负责）
//! ```
//!
//! ## 四类来源（ScriptSource，lead 2026-11-15 裁决）
//!
//! `magpie`（registry.json 内置清单）/ `npm`（npm registry 直装）/
//! `github`（仓库 zip / tarball）/ `local`（运维本机目录）。
//! magpie 双源 URL 由 magpie-catalog 成员持有，本模块只存「条目 → 包」
//! 的映射结果。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 脚本插件根目录名（相对 `{data_dir}`）。
///
/// 与原生包的 `{data_dir}/plugins` 分开：两套宿主的生命周期/目录结构
/// 完全不同（每包一个二进制进程 vs 一个 Bun 承载全部 npm 目录包），
/// 混放会让「卸载即删目录」误伤另一类宿主的资产。
pub const SCRIPT_PLUGINS_DIR: &str = "script-plugins";

/// 脚本插件数量上限。
///
/// 每个启用的脚本包都要进 Bun init_payload 并占 owners 表项；无上限的
/// 目录清单会线性放大握手载荷与路由表。上游形态同为小集合（市场目录
/// 数十项），64 足够且超限时安装直接拒绝（operations 强制）。
pub const MAX_SCRIPT_PACKAGES: usize = 64;

/// 单包 options 的 JSON 字节上限。
///
/// options 经管理面编辑、随 init_payload 全量下发给 Bun；无上限的
/// 配置体既拖慢每次握手，也给了「把数据当配置塞」的滥用面。
pub const MAX_SETTINGS_BYTES: usize = 256 << 10;

/// 脚本插件根目录：`{data_dir}/script-plugins`。
pub fn script_plugins_root(data_dir: &Path) -> PathBuf {
    data_dir.join(SCRIPT_PLUGINS_DIR)
}

/// 脚本来源。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum ScriptSource {
    /// Magpie 市场清单（registry.json / dist index.json）。
    #[default]
    Magpie,
    /// npm registry 直装（裸包名或 @scope/name）。
    Npm,
    /// GitHub 仓库（zip/tarball 直装）。
    Github,
    /// 运维本机目录。
    Local,
}

impl ScriptSource {
    /// 线上小写字符串（与上游 source 字段同形：`magpie` / `npm` / `github` / `local`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Magpie => "magpie",
            Self::Npm => "npm",
            Self::Github => "github",
            Self::Local => "local",
        }
    }
}

/// 发布者三分类（Magpie 目录的分级口径）。
///
/// 排序展示时 official < recommended < third-party（rank 越小越靠前）。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub enum ScriptPublisher {
    /// `@magpie-community/*` 且 community=true 的官方包。
    #[default]
    Official,
    /// registry.json 里其余 provider 包（社区推荐）。
    Recommended,
    /// GitHub / 本地直装、不在 registry 的包。
    ThirdParty,
}

impl ScriptPublisher {
    /// 展示排序权重（official 0 < recommended 1 < third-party 2）。
    pub fn rank(&self) -> u8 {
        match self {
            Self::Official => 0,
            Self::Recommended => 1,
            Self::ThirdParty => 2,
        }
    }

    /// 线上小写字符串（`official` / `recommended` / `third-party`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Official => "official",
            Self::Recommended => "recommended",
            Self::ThirdParty => "third-party",
        }
    }
}

/// 一个已安装的脚本插件包（网关侧元数据，落 KV `plugin:script-packs`）。
///
/// 与原生包 `PackMeta` 平行但独立：原生包是「一个二进制 + providers 声明」，
/// 脚本包是「npm 目录 + OpenCode auth.provider 导出」，字段集合不同，
/// 不共用结构避免一方加字段把另一方语义搅浑。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ScriptPackage {
    /// 包 id（npm 包名，如 `@magpie-community/kiro`）。
    pub id: String,
    /// 展示名（目录 label / package.json 的 description）。
    pub label: String,
    /// 版本（semver 字符串，原样保留）。
    pub version: String,
    /// 来源。
    pub source: ScriptSource,
    /// 发布者三分类。
    pub publisher: ScriptPublisher,
    /// 包内脚本导出的 provider 声明（OpenCode v1 `auth.provider` 形态）。
    pub providers: Vec<ScriptProviderInfo>,
    /// 入口相对路径（相对包版本目录；缺省由 js-host 按 package.json 推导）。
    pub entry: Option<String>,
    /// 安装源定位（magpie=`{name}`；github=`{repo}{@ref|/tag}`；local=路径）。
    pub source_ref: String,
    /// 仓库主页（目录条目带出，管理面展示）。
    pub repository: Option<String>,
    /// 目录摘要（管理面展示）。
    pub summary: Option<String>,
    /// 安装时刻（unix 秒）。
    pub installed_at: i64,
    /// 运维是否启用（装了不等于要跑——与原生包同口径）。
    pub enabled: bool,
    /// 显式信任标记。**脚本不是 OS 沙箱**：只有 trusted=true 才允许被
    /// enable——operations 强制，这里存的是运维的选择。
    pub trusted: bool,
    /// 运维配置（options，逐键对照 settings_schema；≤ MAX_SETTINGS_BYTES）。
    pub options: ScriptSettingsOptions,
}

impl ScriptPackage {
    /// 磁盘目录名：`script-{sha256(id) 前 24 位}`。
    ///
    /// **为什么不用包名当目录**：npm scoped 名形如 `@magpie-community/kiro`，
    /// 直接拼进路径会造出 `packages/@magpie-community/kiro/...` 两级嵌套，
    /// 且 `@` 在部分文件系统/备份工具里需要转义。哈希名是定长、安全字符集、
    /// 无嵌套，与上游 `script-{sha256(name)[..24]}` 的口径一致。
    ///
    /// 这是**磁盘目录名的唯一计算处**——storage / runtime / js-host 都必须
    /// 走 `version_dir()`，不要自己拼。
    pub fn package_dir_name(&self) -> String {
        let digest = Sha256::digest(self.id.as_bytes());
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        format!("script-{}", &hex[..24])
    }

    /// 包版本目录：`{script_root}/packages/script-{hash}/{version}`。
    ///
    /// **Local 来源例外**：本机目录是原地登记（不复制，见 `install.rs` 的
    /// 「为什么不复制」），其包根就是 `source_ref` 指向的路径，不是解包目录。
    /// 统一入口收在这里，宿主/校验/删除都只调本方法，不会出现「一处按解包
    /// 目录找、一处按原路径找」的分裂。
    ///
    /// version 入路径前做字符集消毒：合法 semver 字符集是 `[A-Za-z0-9.+-]`，
    /// 其余一律替换成 `_`——防止 `../` 之类的版本串把目录写出根外。安装链路
    /// 会在落盘前拒绝非法 version，这里是第二道防线。
    ///
    /// **连续两个及以上的 `.` 也算非法**：`/` 被替换成 `_` 后 `../..` 会
    /// 变成单个路径段（不构成穿越），但版本串恰好是 `..` / `...` 时整个
    /// 路径段就是上跳目录——必须一起消掉。合法 semver 不会出现连续点。
    pub fn version_dir(&self, script_root: &Path) -> PathBuf {
        // 本机目录：包根就是登记时记下的绝对路径。
        if matches!(self.source, ScriptSource::Local) && !self.source_ref.is_empty() {
            return PathBuf::from(&self.source_ref);
        }
        let chars: Vec<char> = self.version.chars().collect();
        let mut sanitized = String::with_capacity(chars.len() + 1);
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            if c == '.' {
                let mut j = i;
                while j < chars.len() && chars[j] == '.' {
                    j += 1;
                }
                // 连续点 → 单下划线（`..` / `...` 不能成为目录上跳）
                sanitized.push(if j - i >= 2 { '_' } else { '.' });
                i = j;
                continue;
            }
            sanitized.push(if c.is_ascii_alphanumeric() || matches!(c, '+' | '-') {
                c
            } else {
                '_'
            });
            i += 1;
        }
        // 全是点时消毒结果只剩 `_`/`.`：补一个非点字符，避免空目录名/纯点段
        if sanitized.is_empty() || sanitized.chars().all(|c| c == '.') {
            sanitized.push('_');
        }
        let version = sanitized;
        script_root
            .join("packages")
            .join(self.package_dir_name())
            .join(version)
    }

    /// 入口绝对路径（entry 未声明时返回 None，由 js-host 推导）。
    pub fn entry_path(&self, script_root: &Path) -> Option<PathBuf> {
        self.entry
            .as_deref()
            .map(|e| self.version_dir(script_root).join(e))
    }
}

/// 包声明的一家 provider（OpenCode v1 `auth.provider` 的网关侧投影）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ScriptProviderInfo {
    /// provider id（如 `kiro`）——owners 表与 claimed 集合的键。
    pub id: String,
    /// 展示名。
    pub label: String,
    /// 上游协议（`anthropic` / `responses` / `gemini` / `chat`），SSE 转换器按此分派。
    pub upstream: Option<String>,
    /// 该家支持的模型（可空——运行期 `models.list` 动态拉）。
    pub models: Vec<String>,
}

/// 一个脚本包的运维配置。
///
/// 整体是一个 JSON 对象（键 = settings_schema 里声明的字段名）；本结构
/// 只包一层以便带字节上限校验（≤ MAX_SETTINGS_BYTES，settings 模块强制）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ScriptSettingsOptions {
    /// 逐键配置（键名与包的 settings_schema 对应）。
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// 设置 schema 里的一个字段声明（包自报，管理面据此渲染表单）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ScriptSettingField {
    /// 字段键名。
    pub key: String,
    /// 展示名。
    pub label: String,
    /// 值类型（`string` / `number` / `boolean` / `select` …原样透传）。
    pub kind: Option<String>,
    /// 是否必填。
    pub required: bool,
    /// 默认值。
    pub default: Option<serde_json::Value>,
    /// select 的候选项。
    pub choices: Vec<String>,
    /// 说明文案。
    pub description: Option<String>,
}

/// Magpie 目录条目（market 双源 → Entry 的网关侧形状）。
///
/// magpie-catalog 成员负责拉取/缓存/分级；本结构是「目录条目 → 可安装包」
/// 的映射载体，operations 的 install(magpie) 从这里取 source_ref。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ScriptCatalogEntry {
    /// npm 包名（`@magpie-community/…` 或第三方名）。
    pub name: String,
    /// 展示名。
    pub label: String,
    /// 包声明覆盖的 provider id。
    pub providers: Vec<String>,
    /// 发布者三分类（magpie-catalog 按 official/recommended 规则打标；
    /// GitHub 直装不在 registry，落 ThirdParty）。
    pub publisher: ScriptPublisher,
    /// 来源。
    pub source: ScriptSource,
    /// 安装源定位（与 [`ScriptPackage::source_ref`] 同口径）。
    pub source_ref: String,
    /// 摘要。
    pub summary: Option<String>,
    /// 仓库主页。
    pub repository: Option<String>,
    /// Star 数（目录排序参考）。
    pub stars: Option<u64>,
    /// dist tgz 下载地址（Magpie 官方包的**权威**分发源）。
    ///
    /// 实测：`@magpie-community/*` 官方包在 npm registry 抽样 5 个里 4 个 404
    /// （2026-11-15），dist 分支的 tgz 才是它们在架的分发渠道。安装链路必用
    /// 这个字段，不能只靠 npm。非 magpie 来源为 None。
    #[serde(default)]
    pub archive_url: Option<String>,
    /// `archive_url` 的 SRI 完整性声明（`sha512-…`），安装时校验用。
    ///
    /// 与 `archive_url` 配对出现；缺它时安装必须拒绝（无校验的包不装）。
    #[serde(default)]
    pub integrity: Option<String>,
}

/// Bun 进程握手载荷（runtime 发 `init` 时的 `params.init`）。
///
/// 字段名是 wire 契约：js-host 侧按 `packages` / `reserved` / `claimed` /
/// `catalog_path` 四个键读取，改名 = 断协议。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ScriptInitPayload {
    /// 全部已启用脚本包的包目录绝对路径（Bun 逐个加载）。
    pub packages: Vec<String>,
    /// 保留集合：原生 provider + 内置订阅 + 所有非脚本宿主已声明 provider。
    /// 脚本对这些家**没有**接管权（与原生包 `is_plugin_owned` 同立场）。
    pub reserved: Vec<String>,
    /// 声明集合：已启用脚本包声明的 provider（含与 reserved 的交集——
    /// 交集部分实际由保留方接管，脚本侧据此自知让位）。
    pub claimed: Vec<String>,
    /// Magpie 目录缓存文件路径（Bun 侧市场查询用；可为空 = 目录未拉取）。
    pub catalog_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_roundtrip_and_str() {
        for (src, s) in [
            (ScriptSource::Magpie, "magpie"),
            (ScriptSource::Npm, "npm"),
            (ScriptSource::Github, "github"),
            (ScriptSource::Local, "local"),
        ] {
            assert_eq!(src.as_str(), s);
            let json = serde_json::to_string(&src).unwrap();
            let back: ScriptSource = serde_json::from_str(&json).unwrap();
            assert_eq!(back, src);
        }
    }

    #[test]
    fn publisher_rank_order() {
        assert!(ScriptPublisher::Official.rank() < ScriptPublisher::Recommended.rank());
        assert!(ScriptPublisher::Recommended.rank() < ScriptPublisher::ThirdParty.rank());
    }

    #[test]
    fn script_source_defaults_to_magpie() {
        // 目录条目来源缺省按 magpie 处理（registry 双源是主通路）
        let v: ScriptSource = serde_json::from_str("null").unwrap_or_default();
        assert_eq!(v, ScriptSource::Magpie);
    }

    #[test]
    fn package_paths_join_hash_and_version() {
        let pkg = ScriptPackage {
            id: "@magpie-community/kiro".into(),
            version: "1.2.0".into(),
            entry: Some("dist/index.js".into()),
            ..Default::default()
        };
        let root = Path::new("/data/script-plugins");
        // 目录名必须是 hash（scoped 包名不能直接进路径：会造出嵌套目录）
        let dir_name = pkg.package_dir_name();
        assert!(
            dir_name.starts_with("script-") && dir_name.len() == 31,
            "目录名应为 script- + 24 位 hex，实际 {dir_name}"
        );
        assert!(!dir_name.contains('/'));
        assert!(!dir_name.contains('@'));
        assert_eq!(
            pkg.version_dir(root),
            root.join("packages").join(&dir_name).join("1.2.0")
        );
        assert_eq!(
            pkg.entry_path(root).unwrap(),
            root.join("packages")
                .join(&dir_name)
                .join("1.2.0")
                .join("dist/index.js")
        );
        // 未声明 entry → None（js-host 按 package.json 推导）
        let pkg = ScriptPackage::default();
        assert!(pkg.entry_path(root).is_none());
    }

    #[test]
    fn version_dir_returns_source_ref_for_local() {
        // Local 包是原地登记：包根 = source_ref，不是解包目录。
        // 若这里退化成解包路径，本机包会「加载不到」（目录从不存在）。
        let pkg = ScriptPackage {
            id: "dev-pkg".into(),
            version: "1.0.0".into(),
            source: ScriptSource::Local,
            source_ref: "/home/dev/my-plugin".into(),
            ..Default::default()
        };
        let root = Path::new("/data/script-plugins");
        assert_eq!(pkg.version_dir(root), PathBuf::from("/home/dev/my-plugin"));
        // 其它来源不受影响
        let pkg = ScriptPackage {
            source: ScriptSource::Magpie,
            source_ref: "magpie:@x/y".into(),
            ..Default::default()
        };
        assert!(pkg.version_dir(root).starts_with(root.join("packages")));
    }

    #[test]
    fn version_dir_sanitizes_path_traversal() {
        // 版本串里的 `../` 不能让目录写出 packages 根外。
        let pkg = ScriptPackage {
            id: "evil-pkg".into(),
            version: "../../../etc/cron.d/x".into(),
            ..Default::default()
        };
        let root = Path::new("/data/script-plugins");
        let dir = pkg.version_dir(root);
        let s = dir.to_string_lossy();
        assert!(!s.contains(".."), "版本串的 .. 必须被消毒：{s}");
        // 消毒后仍落在 packages/{hash}/ 之下
        assert!(dir.starts_with(root.join("packages")));
    }

    #[test]
    fn settings_options_json_roundtrip() {
        let mut values = serde_json::Map::new();
        values.insert("region".into(), serde_json::json!("cn"));
        values.insert("retries".into(), serde_json::json!(3));
        let opts = ScriptSettingsOptions { values };
        let json = serde_json::to_string(&opts).unwrap();
        let back: ScriptSettingsOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.values.get("region").unwrap(), "cn");
        assert_eq!(back.values.get("retries").unwrap(), 3);
    }

    #[test]
    fn init_payload_wire_keys() {
        // wire 契约：Bun 侧按这四个键读，字段名不可改
        let payload = ScriptInitPayload {
            packages: vec!["/data/script-plugins/packages/p/1.0.0".into()],
            reserved: vec!["workbuddy".into()],
            claimed: vec!["kiro".into()],
            catalog_path: Some("/data/script-plugins/catalog.json".into()),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert!(json.get("packages").is_some());
        assert!(json.get("reserved").is_some());
        assert!(json.get("claimed").is_some());
        assert!(json.get("catalog_path").is_some());
    }

    #[test]
    fn catalog_entry_defaults_missing_fields() {
        let entry: ScriptCatalogEntry =
            serde_json::from_str(r#"{"name":"@magpie-community/kiro"}"#).unwrap();
        assert_eq!(entry.name, "@magpie-community/kiro");
        assert_eq!(entry.publisher, ScriptPublisher::Official);
        assert_eq!(entry.source, ScriptSource::Magpie);
        assert!(entry.providers.is_empty());
        assert_eq!(entry.stars, None);
    }
}
