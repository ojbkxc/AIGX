//! 客户端/来源识别（对照 Agent2API `core/client` 的非插件能力）。
//!
//! 只读请求头，输出规范化的客户端名与版本；**不参与任何转发/计费决策**。
//!
//! ## 识别优先级（高 → 低）
//!
//! 1. **显式声明**：`X-Client-Name`（兼容 `X-Aigx-Client` / `X-Agent2api-Client`），
//!    可选 `X-Client-Version`。声明值命中内置别名时归一为规范名（如
//!    `claude-cli` → `Claude Code`）；否则按自定义客户端名保留。
//! 2. **自定义规则**：管理面维护的 UA/请求头规则，按 `priority` 升序（数值小
//!    者先）逐条匹配，首条命中即返回；支持前缀/包含/正则（正则编译失败
//!    的规则在加载时即被跳过，匹配阶段绝不 panic）。
//! 3. **内置 UA 规则**：常见 AI 客户端的产品令牌（`claude-cli/1.2.3`、
//!    `codex_cli_rs/0.4`、`OpenAI/Python 1.30` 等），命中后从 UA 提取版本。
//! 4. 全部未命中 → [`UNKNOWN_CLIENT`]（`unknown`）。
//!
//! ## 持久化与生效
//!
//! 自定义规则整份 JSON 落 FileStore KV（键 [`RULES_KEY`]），启动时
//! [`install`] 载入进程级快照；管理面每次写库后**原子替换快照**，下一个
//! 请求立即使用新规则，不重启、不缓存旧配置。
//!
//! ## 只读红线
//!
//! - 只读 UA 与管理员显式允许的请求头；规则不能读取 `Authorization` /
//!   `Cookie` / API Key 等凭证头（保存时拒绝）。
//! - 所有输入按字符数截断，控制字符替换为空格；正则用 `regex` crate
//!   （线性时间）并限制编译体积，避免灾难性回溯。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use axum::http::HeaderMap;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};

use crate::storage::FileStore;

/// 未识别客户端的固定名字（报表/日志对账锚点）。
pub const UNKNOWN_CLIENT: &str = "unknown";

/// 自定义规则 KV 键（整份 JSON 读写）。
pub const RULES_KEY: &str = "client_id_rules";

/// 显式声明客户端名的请求头（小写）。
pub const DECLARATION_HEADER: &str = "x-client-name";
/// 显式声明客户端版本的请求头（小写）。
pub const DECLARATION_VERSION_HEADER: &str = "x-client-version";
/// 声明头的兼容别名：先匹配 `x-client-name`，再依次尝试这些。
pub const DECLARATION_HEADER_ALIASES: &[&str] = &["x-aigx-client", "x-agent2api-client"];

/// 规则条数上限（管理面软上限，防误用刷爆 KV）。
pub const MAX_RULES: usize = 200;
/// 单条规则名称/目标名长度上限。
const MAX_NAME_CHARS: usize = 64;
/// 匹配内容长度上限（正则另限 256）。
const MAX_PATTERN_CHARS: usize = 256;
/// 正则编译体积上限（字节）：防一条规则吃掉整个进程内存。
const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// UA / 请求头读取长度上限（字符）。
const MAX_HEADER_CHARS: usize = 1024;
/// 显式声明名的长度上限（字符）。
const MAX_DECLARED_CHARS: usize = 128;

/// 禁止自定义规则读取的凭证类请求头（小写）。
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "proxy-authorization",
    "x-auth-token",
    "x-access-token",
    "www-authenticate",
];

// ── 内置客户端目录 ──────────────────────────────────────────────────

/// 一个内置客户端的 UA 指纹与声明别名。
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinClient {
    /// 稳定 id（小写短横线）
    pub id: &'static str,
    /// 规范化展示名（写入 `RequestLog.client_name`）
    pub name: &'static str,
    /// 显式声明可接受的别名（小写，含 id/name 本身）
    pub aliases: &'static [&'static str],
    /// UA 产品令牌（大小写不敏感、按 token 边界匹配）
    pub products: &'static [&'static str],
}

/// 内置客户端目录。顺序即匹配优先级（更具体的产品令牌在前）。
pub const BUILTIN_CLIENTS: &[BuiltinClient] = &[
    BuiltinClient {
        id: "claude-code",
        name: "Claude Code",
        aliases: &[
            "claude-code",
            "claude code",
            "claude-cli",
            "claude_cli",
            "claudecode",
        ],
        products: &["claude-cli", "claude-code", "claude_cli"],
    },
    BuiltinClient {
        id: "codex",
        name: "Codex",
        aliases: &[
            "codex",
            "codex-cli",
            "codex_cli",
            "codex_cli_rs",
            "codex desktop",
            "codex-vscode",
        ],
        products: &[
            "codex_cli_rs",
            "codex-cli",
            "codex_cli",
            "codex_vscode",
            "Codex Desktop",
            "codex-tui",
            "codex",
        ],
    },
    BuiltinClient {
        id: "cline",
        name: "Cline",
        aliases: &["cline", "cline-cli", "cline-sdk"],
        products: &["cline-cli", "cline-sdk", "cline"],
    },
    BuiltinClient {
        id: "cursor",
        name: "Cursor",
        aliases: &["cursor", "cursor-agent"],
        products: &["cursor-agent", "Cursor"],
    },
    BuiltinClient {
        id: "continue",
        name: "Continue",
        aliases: &["continue", "continue.dev", "continue-cli"],
        products: &["continue.dev", "continue-cli", "continue"],
    },
    BuiltinClient {
        id: "openai-sdk",
        name: "OpenAI SDK",
        aliases: &[
            "openai sdk",
            "openai-python",
            "openai-node",
            "openai-js",
            "openai",
        ],
        products: &[
            "openai-python",
            "openai-node",
            "openai-js",
            "OpenAI/Python",
            "OpenAI/NodeJS",
            "OpenAI/JS",
        ],
    },
    BuiltinClient {
        id: "anthropic-sdk",
        name: "Anthropic SDK",
        aliases: &[
            "anthropic sdk",
            "anthropic-python",
            "anthropic-node",
            "anthropic",
        ],
        products: &[
            "anthropic-sdk-python",
            "anthropic-sdk-typescript",
            "Anthropic/Python",
            "Anthropic/JS",
        ],
    },
    BuiltinClient {
        id: "langchain",
        name: "LangChain",
        aliases: &[
            "langchain",
            "langchain-core",
            "langchainjs",
            "langchain-js",
            "langchain-python",
        ],
        products: &[
            "langchain-core",
            "langchainjs",
            "langchain-js",
            "langchain-python",
            "langchain",
        ],
    },
    BuiltinClient {
        id: "cherry-studio",
        name: "Cherry Studio",
        aliases: &["cherry studio", "cherrystudio", "cherry-studio"],
        products: &["Cherry Studio", "CherryStudio"],
    },
    BuiltinClient {
        id: "github-copilot",
        name: "GitHub Copilot",
        aliases: &["github copilot", "github-copilot", "copilot"],
        products: &["GitHubCopilotChat", "GitHub Copilot"],
    },
    BuiltinClient {
        id: "zed",
        name: "Zed",
        aliases: &["zed"],
        products: &["Zed"],
    },
    BuiltinClient {
        id: "qwen-code",
        name: "Qwen Code",
        aliases: &["qwen code", "qwen-code", "qwencli"],
        products: &["qwen-code", "QwenCode", "qwencli"],
    },
    BuiltinClient {
        id: "opencode",
        name: "OpenCode",
        aliases: &["opencode"],
        products: &["opencode"],
    },
    BuiltinClient {
        id: "kimi-code",
        name: "Kimi Code",
        aliases: &["kimi code", "kimi-cli", "kimi-code"],
        products: &["kimi-cli", "kimi-code", "KimiCLI"],
    },
    BuiltinClient {
        id: "aider",
        name: "Aider",
        aliases: &["aider"],
        products: &["aider"],
    },
    BuiltinClient {
        id: "roo-code",
        name: "Roo Code",
        aliases: &["roo code", "roo-code", "roocode"],
        products: &["Roo-Code", "RooCode"],
    },
    BuiltinClient {
        id: "windsurf",
        name: "Windsurf",
        aliases: &["windsurf"],
        products: &["Windsurf"],
    },
    BuiltinClient {
        id: "gemini-cli",
        name: "Gemini CLI",
        aliases: &["gemini cli", "gemini-cli"],
        products: &["gemini-cli", "gemini_cli"],
    },
    BuiltinClient {
        id: "chatbox",
        name: "Chatbox",
        aliases: &["chatbox"],
        products: &["Chatbox"],
    },
    BuiltinClient {
        id: "lobe-chat",
        name: "LobeChat",
        aliases: &["lobechat", "lobe-chat", "lobe chat"],
        products: &["LobeChat"],
    },
];

// ── 识别结果 ────────────────────────────────────────────────────────

/// 一次识别的结果（写入请求日志的两个字段 + 判定来源）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientIdentity {
    /// 规范化客户端名（未识别 = [`UNKNOWN_CLIENT`]）
    pub name: String,
    /// 客户端版本（能提取则填，否则空串）
    pub version: String,
    /// 判定来源：`declared` / `custom_rule` / `builtin_ua` / `unknown`
    pub source: &'static str,
}

impl ClientIdentity {
    fn unknown() -> Self {
        Self {
            name: UNKNOWN_CLIENT.to_string(),
            version: String::new(),
            source: "unknown",
        }
    }

    pub fn is_unknown(&self) -> bool {
        self.source == "unknown"
    }
}

/// 把识别结果写入请求日志。
///
/// 数据面所有 `RequestLog` 构造点统一走这里，避免各路径口径漂移。
pub fn attach(log: &mut crate::log::RequestLog, identity: &ClientIdentity) {
    log.client_name = identity.name.clone();
    log.client_version = identity.version.clone();
}

// ── 自定义规则 ──────────────────────────────────────────────────────

/// 规则匹配来源。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleTarget {
    /// 匹配 `User-Agent`
    #[default]
    Ua,
    /// 匹配指定请求头（`header` 字段）
    Header,
}

/// 规则匹配方式（前缀/包含大小写不敏感；正则按规则原文）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    #[default]
    Prefix,
    Contains,
    Regex,
}

fn default_enabled() -> bool {
    true
}

/// 一条自定义识别规则（管理面 CRUD 的存储形状）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRule {
    /// 规则 id（空串在保存/加载时自动补 uuid）
    #[serde(default)]
    pub id: String,
    /// 规则名称（管理面展示用）
    #[serde(default)]
    pub name: String,
    /// 是否启用
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 匹配来源：UA / Header
    #[serde(default)]
    pub target: RuleTarget,
    /// target=Header 时读取的请求头（小写）；target=Ua 时忽略
    #[serde(default)]
    pub header: String,
    /// 匹配方式
    #[serde(default)]
    pub match_mode: MatchMode,
    /// 匹配内容
    #[serde(default)]
    pub pattern: String,
    /// 命中后输出的客户端名
    #[serde(default)]
    pub client_name: String,
    /// 优先级：数值越小越先匹配
    #[serde(default)]
    pub priority: i32,
}

/// 已编译规则（正则预编译，加载/保存后不再每次编译）。
#[derive(Clone)]
struct CompiledRule {
    rule: ClientRule,
    regex: Option<regex::Regex>,
}

/// 进程级规则快照（`Arc` 整体替换 = 原子生效）。
#[derive(Clone, Default)]
pub struct CompiledRules {
    rules: Vec<CompiledRule>,
}

impl CompiledRules {
    /// 校验并编译一批规则；任何一条非法即整批拒绝（管理面保存路径）。
    pub fn compile(rules: Vec<ClientRule>) -> Result<Self, String> {
        if rules.len() > MAX_RULES {
            return Err(format!("客户端识别规则最多 {MAX_RULES} 条"));
        }
        let mut out = Vec::with_capacity(rules.len());
        let mut ids = std::collections::HashSet::new();
        for rule in rules {
            let compiled = compile_one(rule)?;
            if !ids.insert(compiled.rule.id.clone()) {
                return Err(format!("规则 id 重复：{}", compiled.rule.id));
            }
            out.push(compiled);
        }
        out.sort_by_key(|rule| rule.rule.priority);
        Ok(Self { rules: out })
    }

    /// 宽容加载（KV 里的历史/手改数据）：非法规则跳过并告警，不阻断启动。
    fn compile_lossy(rules: Vec<ClientRule>) -> Self {
        let mut out: Vec<CompiledRule> = rules
            .into_iter()
            .filter_map(|rule| match compile_one(rule) {
                Ok(compiled) => Some(compiled),
                Err(error) => {
                    tracing::warn!("客户端识别规则已跳过：{error}");
                    None
                }
            })
            .collect();
        out.sort_by_key(|rule| rule.rule.priority);
        Self { rules: out }
    }

    /// 当前规则原文（管理面展示/回写用）。
    pub fn raw_rules(&self) -> Vec<ClientRule> {
        self.rules.iter().map(|item| item.rule.clone()).collect()
    }

    /// 按优先级逐条匹配请求头。
    fn match_headers(&self, headers: &HeaderMap) -> Option<ClientIdentity> {
        for item in &self.rules {
            if !item.rule.enabled {
                continue;
            }
            let header = match item.rule.target {
                RuleTarget::Ua => "user-agent",
                RuleTarget::Header => item.rule.header.as_str(),
            };
            let Some(value) = read_header(headers, header) else {
                continue;
            };
            let Some(end) = match_end(&item.rule, item.regex.as_ref(), &value) else {
                continue;
            };
            let name = normalize_client_name(&item.rule.client_name)
                .unwrap_or_else(|| item.rule.client_name.clone());
            return Some(ClientIdentity {
                name,
                version: version_at(&value, end),
                source: "custom_rule",
            });
        }
        None
    }
}

/// 单条规则的校验 + 正则编译。
fn compile_one(mut rule: ClientRule) -> Result<CompiledRule, String> {
    rule.id = rule.id.trim().to_string();
    if rule.id.is_empty() {
        rule.id = uuid::Uuid::new_v4().to_string();
    }
    if !valid_rule_id(&rule.id) {
        return Err("规则 id 只能包含字母、数字与 - _ :，且不超过 80 个字符".into());
    }
    rule.name = rule.name.trim().to_string();
    if !valid_text(&rule.name, MAX_NAME_CHARS) {
        return Err("规则名称不能为空、不能含控制字符，最长 64 个字符".into());
    }
    rule.pattern = rule.pattern.trim().to_string();
    if !valid_text(&rule.pattern, MAX_PATTERN_CHARS) {
        return Err("匹配内容不能为空、不能含控制字符，最长 256 个字符".into());
    }
    rule.client_name = rule.client_name.trim().to_string();
    if !valid_text(&rule.client_name, MAX_NAME_CHARS) {
        return Err("目标客户端名不能为空、不能含控制字符，最长 64 个字符".into());
    }
    if !(-1000..=1000).contains(&rule.priority) {
        return Err("优先级取值范围为 -1000..=1000（数值越小越先匹配）".into());
    }
    if rule.target == RuleTarget::Header {
        rule.header = rule.header.trim().to_ascii_lowercase();
        if rule.header.is_empty() || rule.header.len() > 64 || !valid_header_name(&rule.header) {
            return Err("请求头名不合法（小写字母/数字/-/_，最长 64）".into());
        }
        if SENSITIVE_HEADERS.contains(&rule.header.as_str()) {
            return Err("识别规则不能读取认证/Cookie 等敏感请求头".into());
        }
    } else {
        rule.header.clear();
    }
    let regex = match rule.match_mode {
        MatchMode::Regex => Some(
            regex::RegexBuilder::new(&rule.pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
                .map_err(|error| format!("正则表达式非法：{error}"))?,
        ),
        MatchMode::Prefix | MatchMode::Contains => None,
    };
    Ok(CompiledRule { rule, regex })
}

/// 规则匹配：返回命中在原文中的结束字节位置（版本提取锚点）。
fn match_end(rule: &ClientRule, regex: Option<&regex::Regex>, value: &str) -> Option<usize> {
    match rule.match_mode {
        MatchMode::Prefix => {
            let end = rule.pattern.len();
            (value.len() >= end
                && value.is_char_boundary(end)
                && value[..end].eq_ignore_ascii_case(&rule.pattern))
            .then_some(end)
        }
        MatchMode::Contains => {
            let haystack = value.to_ascii_lowercase();
            let needle = rule.pattern.to_ascii_lowercase();
            haystack.find(&needle).map(|start| start + needle.len())
        }
        MatchMode::Regex => regex?.find(value).map(|matched| matched.end()),
    }
}

// ── 全局规则存储 ────────────────────────────────────────────────────

/// 整份 KV 值（revision 供管理面判断并发修改）。
#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredRules {
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    rules: Vec<ClientRule>,
}

/// 规则存储：KV + 进程级快照。
pub struct ClientRulesStore {
    store: Arc<FileStore>,
    snapshot: RwLock<Arc<CompiledRules>>,
    revision: AtomicU64,
}

static GLOBAL: OnceLock<ClientRulesStore> = OnceLock::new();

fn mutation_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

impl ClientRulesStore {
    fn load(store: Arc<FileStore>) -> Self {
        let stored: StoredRules = match store.get(RULES_KEY) {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                tracing::warn!("读取客户端识别规则失败，按空规则启动：{error}");
                StoredRules::default()
            }
        };
        Self {
            store,
            snapshot: RwLock::new(Arc::new(CompiledRules::compile_lossy(stored.rules))),
            revision: AtomicU64::new(stored.revision),
        }
    }
}

/// 启动时安装规则存储（`main` 里调用一次；重复调用忽略后者）。
pub fn install(store: Arc<FileStore>) {
    let engine = ClientRulesStore::load(store);
    if GLOBAL.set(engine).is_err() {
        tracing::warn!("客户端识别规则存储已初始化，忽略重复 install");
    }
}

/// 是否已安装（测试/诊断用）。
pub fn installed() -> bool {
    GLOBAL.get().is_some()
}

/// 当前规则快照副本。
pub fn current_rules() -> Vec<ClientRule> {
    GLOBAL
        .get()
        .map(|store| store.snapshot.read().raw_rules())
        .unwrap_or_default()
}

/// 当前规则版本号（每次成功保存 +1）。
pub fn revision() -> u64 {
    GLOBAL
        .get()
        .map(|store| store.revision.load(Ordering::Acquire))
        .unwrap_or(0)
}

/// 管理面写规则：读改写全程持锁，持久化成功后原子替换快照（即时生效）。
pub fn mutate_rules<F>(mutate: F) -> Result<(u64, Vec<ClientRule>), String>
where
    F: FnOnce(&mut Vec<ClientRule>) -> Result<(), String>,
{
    let store = GLOBAL
        .get()
        .ok_or_else(|| "客户端识别规则存储未初始化".to_string())?;
    let _guard = mutation_lock().lock();
    let mut rules = store.snapshot.read().raw_rules();
    mutate(&mut rules)?;
    let compiled = CompiledRules::compile(rules)?;
    let revision = store.revision.load(Ordering::Acquire) + 1;
    store
        .store
        .put(
            RULES_KEY,
            &StoredRules {
                revision,
                rules: compiled.raw_rules(),
            },
        )
        .map_err(|error| format!("客户端识别规则持久化失败：{error}"))?;
    *store.snapshot.write() = Arc::new(compiled.clone());
    store.revision.store(revision, Ordering::Release);
    Ok((revision, compiled.raw_rules()))
}

/// 识别请求头（数据面入口调用）。
pub fn identify(headers: &HeaderMap) -> ClientIdentity {
    match GLOBAL.get() {
        Some(store) => identify_with(headers, &store.snapshot.read()),
        None => identify_with(headers, &CompiledRules::default()),
    }
}

/// 纯识别函数（快照由调用方给；单测直接构造）。
pub fn identify_with(headers: &HeaderMap, rules: &CompiledRules) -> ClientIdentity {
    // 1) 显式声明（最高优先级）
    for header in
        std::iter::once(DECLARATION_HEADER).chain(DECLARATION_HEADER_ALIASES.iter().copied())
    {
        if let Some(raw) = read_header(headers, header) {
            if let Some(identity) = declared_identity(&raw, headers) {
                return identity;
            }
        }
    }
    // 2) 自定义规则（按优先级）
    if let Some(identity) = rules.match_headers(headers) {
        return identity;
    }
    // 3) 内置 UA 规则
    let user_agent = read_header(headers, "user-agent").unwrap_or_default();
    builtin_identity(&user_agent)
}

/// 显式声明的解析与归一。
fn declared_identity(raw: &str, headers: &HeaderMap) -> Option<ClientIdentity> {
    let (raw_name, inline_version) = split_declared(raw)?;
    let name = normalize_client_name(&raw_name)?;
    let version = read_header(headers, DECLARATION_VERSION_HEADER)
        .filter(|value| is_version_token(value))
        .or(inline_version)
        .or_else(|| {
            let ua = read_header(headers, "user-agent")?;
            version_for_name(&ua, &raw_name, &name)
        })
        .unwrap_or_default();
    Some(ClientIdentity {
        name,
        version,
        source: "declared",
    })
}

/// `Name/1.2.3` → (`Name`, Some(`1.2.3`))；无版本分隔返回 (原文, None)。
fn split_declared(raw: &str) -> Option<(String, Option<String>)> {
    let text = bounded_text(raw, MAX_DECLARED_CHARS);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some((name, version)) = text.rsplit_once('/') {
        let version = version.trim();
        if !name.trim().is_empty() && is_version_token(version) {
            return Some((name.trim().to_string(), Some(version.to_string())));
        }
    }
    Some((text.to_string(), None))
}

/// 声明名归一：命中内置别名 → 规范名；否则按「安全展示名」校验后保留。
fn normalize_client_name(raw: &str) -> Option<String> {
    let text = bounded_text(raw, MAX_DECLARED_CHARS);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    for client in BUILTIN_CLIENTS {
        if eq_ci(client.id, text) || eq_ci(client.name, text) {
            return Some(client.name.to_string());
        }
        if client.aliases.iter().any(|alias| eq_ci(alias, text)) {
            return Some(client.name.to_string());
        }
    }
    // 自定义名：允许字母数字与常见分隔符（含中文），拒绝控制字符（bounded 已处理）。
    let valid = text.chars().count() <= MAX_NAME_CHARS
        && text.chars().any(|c| !c.is_whitespace())
        && text
            .chars()
            .all(|c| !c.is_control() && !matches!(c, '<' | '>' | '"' | '\'' | '\\'));
    valid.then(|| text.to_string())
}

/// 内置 UA 识别。
fn builtin_identity(user_agent: &str) -> ClientIdentity {
    for client in BUILTIN_CLIENTS {
        for product in client.products {
            if let Some(start) = product_position(user_agent, product) {
                return ClientIdentity {
                    name: client.name.to_string(),
                    version: version_at(user_agent, start + product.len()),
                    source: "builtin_ua",
                };
            }
        }
    }
    ClientIdentity::unknown()
}

/// 声明名对应的版本提取（先找内置产品令牌，再按声明名原文找）。
fn version_for_name(user_agent: &str, raw_name: &str, canonical: &str) -> Option<String> {
    for client in BUILTIN_CLIENTS {
        let matches = eq_ci(client.name, canonical)
            || client.aliases.iter().any(|alias| eq_ci(alias, raw_name));
        if !matches {
            continue;
        }
        for product in client.products {
            if let Some(start) = product_position(user_agent, product) {
                let version = version_at(user_agent, start + product.len());
                if !version.is_empty() {
                    return Some(version);
                }
            }
        }
    }
    let start = find_ci(user_agent, raw_name)?;
    let version = version_at(user_agent, start + raw_name.len());
    (!version.is_empty()).then_some(version)
}

/// 产品令牌定位：大小写不敏感，且两侧必须是 token 边界。
fn product_position(haystack: &str, product: &str) -> Option<usize> {
    if product.is_empty() {
        return None;
    }
    let lower = haystack.to_ascii_lowercase();
    let needle = product.to_ascii_lowercase();
    let mut from = 0;
    while let Some(offset) = lower[from..].find(&needle) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_token_byte(lower.as_bytes()[start - 1]);
        let after_ok = end == lower.len() || !is_token_byte(lower.as_bytes()[end]);
        if before_ok && after_ok {
            return Some(start);
        }
        from = end;
        if from >= lower.len() {
            break;
        }
    }
    None
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

/// 从 `pos_after`（产品名/匹配内容的结束位置）提取版本号。
///
/// 支持三种形态：
/// - `product/1.2.3` → `1.2.3`
/// - `OpenAI/Python 1.30.0`（空白后紧跟版本号）→ `1.30.0`
/// - `product/` 前缀规则（当前位置紧跟版本号）→ `1.2.3`
fn version_at(value: &str, pos_after: usize) -> String {
    let bytes = value.as_bytes();
    if pos_after == 0 || pos_after > bytes.len() {
        return String::new();
    }
    let mut index = pos_after;
    // 匹配内容自身以 '/' 结尾（如 "MyClient/"）：版本紧跟其后。
    if bytes[index - 1] == b'/' {
        return take_version(bytes, index).unwrap_or_default();
    }
    if index < bytes.len() && bytes[index] == b'/' {
        return take_version(bytes, index + 1).unwrap_or_default();
    }
    // `Product 1.2.3`：跳过一个空白后取版本 token。
    let mut probe = index;
    while probe < bytes.len() && (bytes[probe] == b' ' || bytes[probe] == b'\t') {
        probe += 1;
    }
    if probe > index {
        return take_version(bytes, probe).unwrap_or_default();
    }
    String::new()
}

/// 版本 token：字母数字与 `._+-`，以数字开头、长度 ≤32。
fn take_version(bytes: &[u8], start: usize) -> Option<String> {
    let mut end = start;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'.' | b'-' | b'_' | b'+'))
    {
        end += 1;
    }
    if end == start {
        return None;
    }
    let token = std::str::from_utf8(&bytes[start..end]).ok()?;
    let valid = token.len() <= 32
        && token.as_bytes()[0].is_ascii_digit()
        && token.bytes().any(|byte| byte.is_ascii_digit());
    valid.then(|| token.to_string())
}

fn is_version_token(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 32
        && value.as_bytes()[0].is_ascii_digit()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+'))
}

/// 读取请求头：单值、合法 UTF-8、去控制字符、按字符截断。
fn read_header(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?;
    let text = value.to_str().ok()?;
    let text = bounded_text(text, MAX_HEADER_CHARS);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// 按字符截断并把控制字符替换为空格。
fn bounded_text(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars));
    let mut count = 0usize;
    for ch in text.chars() {
        if count >= max_chars {
            break;
        }
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
        count += 1;
    }
    out
}

fn eq_ci(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn valid_text(text: &str, max_chars: usize) -> bool {
    !text.is_empty() && !text.chars().any(char::is_control) && text.chars().count() <= max_chars
}

fn valid_rule_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn rules(items: Vec<ClientRule>) -> CompiledRules {
        CompiledRules::compile(items).expect("规则应通过校验")
    }

    fn rule(
        id: &str,
        target: RuleTarget,
        mode: MatchMode,
        pattern: &str,
        client_name: &str,
        priority: i32,
    ) -> ClientRule {
        ClientRule {
            id: id.to_string(),
            name: format!("规则 {id}"),
            enabled: true,
            target,
            header: if target == RuleTarget::Header {
                "x-custom-client".to_string()
            } else {
                String::new()
            },
            match_mode: mode,
            pattern: pattern.to_string(),
            client_name: client_name.to_string(),
            priority,
        }
    }

    /// 内置规则：Claude Code UA 识别 + 版本提取。
    #[test]
    fn builtin_matches_claude_code_with_version() {
        let identity = identify_with(
            &headers(&[("user-agent", "claude-cli/2.1.0 (external, cli)")]),
            &CompiledRules::default(),
        );
        assert_eq!(identity.name, "Claude Code");
        assert_eq!(identity.version, "2.1.0");
        assert_eq!(identity.source, "builtin_ua");
    }

    /// 内置规则：Codex / Cline / Cursor / Continue / OpenAI SDK / LangChain。
    #[test]
    fn builtin_matches_common_clients() {
        let cases = [
            ("codex_cli_rs/0.48.0 (Linux)", "Codex", "0.48.0"),
            ("Cline/3.2.1", "Cline", "3.2.1"),
            ("Cursor/0.45.11 (darwin arm64)", "Cursor", "0.45.11"),
            ("continue/0.8.5", "Continue", "0.8.5"),
            ("OpenAI/Python 1.30.0", "OpenAI SDK", "1.30.0"),
            ("langchain/0.3.1", "LangChain", "0.3.1"),
        ];
        for (ua, name, version) in cases {
            let identity =
                identify_with(&headers(&[("user-agent", ua)]), &CompiledRules::default());
            assert_eq!(identity.name, name, "UA={ua}");
            assert_eq!(identity.version, version, "UA={ua}");
        }
    }

    /// 普通浏览器 UA → unknown（不把通用平台当客户端）。
    #[test]
    fn browser_ua_is_unknown() {
        let identity = identify_with(
            &headers(&[(
                "user-agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36",
            )]),
            &CompiledRules::default(),
        );
        assert_eq!(identity.name, UNKNOWN_CLIENT);
        assert!(identity.version.is_empty());
        assert_eq!(identity.source, "unknown");
    }

    /// 无请求头 → unknown。
    #[test]
    fn missing_headers_is_unknown() {
        let identity = identify_with(&HeaderMap::new(), &CompiledRules::default());
        assert_eq!(identity.name, UNKNOWN_CLIENT);
    }

    /// 自定义规则优先级：数值小者先命中；同优先级保持插入顺序。
    #[test]
    fn custom_rule_priority_is_respected() {
        let compiled = rules(vec![
            rule(
                "low",
                RuleTarget::Ua,
                MatchMode::Contains,
                "MyTool",
                "低优先级客户端",
                50,
            ),
            rule(
                "high",
                RuleTarget::Ua,
                MatchMode::Prefix,
                "MyTool",
                "高优先级客户端",
                10,
            ),
        ]);
        let identity = identify_with(&headers(&[("user-agent", "MyTool/1.2.3")]), &compiled);
        assert_eq!(identity.name, "高优先级客户端");
        assert_eq!(identity.version, "1.2.3");
        assert_eq!(identity.source, "custom_rule");
    }

    /// 自定义规则整体优先于内置规则。
    #[test]
    fn custom_rule_beats_builtin() {
        let compiled = rules(vec![rule(
            "rename",
            RuleTarget::Ua,
            MatchMode::Prefix,
            "claude-cli",
            "公司内部客户端",
            0,
        )]);
        let identity = identify_with(&headers(&[("user-agent", "claude-cli/2.0.1")]), &compiled);
        assert_eq!(identity.name, "公司内部客户端");
        assert_eq!(identity.version, "2.0.1");
    }

    /// 非法的正则规则不会 panic：保存路径拒绝，加载路径跳过，识别回落到内置/unknown。
    #[test]
    fn invalid_regex_never_panics() {
        let bad = rule(
            "bad",
            RuleTarget::Ua,
            MatchMode::Regex,
            "[unclosed",
            "坏规则",
            0,
        );
        assert!(
            CompiledRules::compile(vec![bad.clone()]).is_err(),
            "非法正则应在保存时被拒绝"
        );

        // 模拟 KV 里残留的坏规则：compile_lossy 跳过，识别不 panic。
        let compiled = CompiledRules::compile_lossy(vec![bad]);
        let identity = identify_with(&headers(&[("user-agent", "claude-cli/1.0.0")]), &compiled);
        assert_eq!(identity.name, "Claude Code");
    }

    /// 显式声明覆盖自定义规则与内置规则。
    #[test]
    fn explicit_declaration_overrides_rules() {
        let compiled = rules(vec![rule(
            "force",
            RuleTarget::Ua,
            MatchMode::Contains,
            "claude-cli",
            "规则命中的名字",
            0,
        )]);
        let identity = identify_with(
            &headers(&[
                ("x-client-name", "Claude Code"),
                ("user-agent", "claude-cli/1.0.0"),
            ]),
            &compiled,
        );
        assert_eq!(identity.name, "Claude Code");
        assert_eq!(identity.source, "declared");
        // 声明未带版本时，从 UA 中同族产品提取。
        assert_eq!(identity.version, "1.0.0");
        // 任意自定义声明名同样压过规则（不被规则改写）。
        let custom = identify_with(
            &headers(&[
                ("x-client-name", "My Company Agent"),
                ("user-agent", "claude-cli/1.0.0"),
            ]),
            &compiled,
        );
        assert_eq!(custom.name, "My Company Agent");
        assert_eq!(custom.source, "declared");
    }

    /// 显式版本头优先于 UA 提取。
    #[test]
    fn declaration_version_header_wins() {
        let identity = identify_with(
            &headers(&[
                ("x-client-name", "My Company Agent"),
                ("x-client-version", "9.9.9"),
                ("user-agent", "MyTool/1.0.0"),
            ]),
            &CompiledRules::default(),
        );
        assert_eq!(identity.name, "My Company Agent");
        assert_eq!(identity.version, "9.9.9");
        assert_eq!(identity.source, "declared");
    }

    /// 请求头规则：按指定头做包含匹配，并提取版本。
    #[test]
    fn header_rule_matches_and_extracts_version() {
        let compiled = rules(vec![rule(
            "hdr",
            RuleTarget::Header,
            MatchMode::Contains,
            "AcmeClient",
            "Acme Client",
            0,
        )]);
        let identity = identify_with(
            &headers(&[("x-custom-client", "brand/AcmeClient-7.8.9")]),
            &compiled,
        );
        assert_eq!(identity.name, "Acme Client");
        assert_eq!(identity.source, "custom_rule");
    }

    /// 规则校验：敏感头、空字段、重复 id 都拒绝。
    #[test]
    fn validation_rejects_unsafe_or_empty_rules() {
        let mut sensitive = rule("s", RuleTarget::Header, MatchMode::Prefix, "x", "X", 0);
        sensitive.header = "authorization".to_string();
        assert!(CompiledRules::compile(vec![sensitive]).is_err());

        let empty_pattern = rule("e", RuleTarget::Ua, MatchMode::Prefix, "  ", "X", 0);
        assert!(CompiledRules::compile(vec![empty_pattern]).is_err());

        let a = rule("dup", RuleTarget::Ua, MatchMode::Prefix, "a", "A", 0);
        let b = rule("dup", RuleTarget::Ua, MatchMode::Prefix, "b", "B", 1);
        assert!(CompiledRules::compile(vec![a, b]).is_err());
    }

    /// 产品令牌边界：不把单词内部的子串当客户端。
    #[test]
    fn product_token_boundary_is_enforced() {
        let identity = identify_with(
            &headers(&[("user-agent", "MyCodexWrapper/1.0.0")]),
            &CompiledRules::default(),
        );
        assert_eq!(identity.name, UNKNOWN_CLIENT);
    }

    /// 版本提取的单测：斜杠、空白、非法版本形态。
    #[test]
    fn version_extraction_shapes() {
        assert_eq!(version_at("foo/1.2.3 (x)", 3), "1.2.3");
        assert_eq!(version_at("OpenAI/Python 1.30.0", 13), "1.30.0");
        assert_eq!(version_at("foo/", 4), "");
        assert_eq!(version_at("foo/bar", 4), "");
        assert!(is_version_token("1.2.3"));
        assert!(!is_version_token("python"));
    }
}
