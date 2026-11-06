//! 账号凭证 provider 目录 —— 「粘贴哪家 Key，用哪个原生 Bridge」。
//!
//! provider 只描述协议族与默认上游地址，实际的请求构造**复用既有 Bridge**：
//! - `Openai` → `bridge::openai::make_bridge`（OpenAI 兼容上游）
//! - `Anthropic` → `bridge::anthropic::make_bridge`（x-api-key + anthropic-version）
//! - `Gemini` → `bridge::gemini::make_bridge`（x-goog-api-key）
//!
//! 新增一家 OpenAI 兼容供应商（同一把 Key 换域名）只需在 `PROVIDERS` 里加一条。
//!
//! ## 四层对齐（方案 C，2026-11-06 固化）
//!
//! AIGX 对接 Agent2API 上游（副本 `D:\GitHub\Agent2API-upstream-main`）按
//! **四层**跟进，本文件承载其中两层：
//!   - **注册表层**（本文件 `PROVIDERS` / `SESSION_PROVIDERS` /
//!     `AGENT2API_PROVIDERS` ↔ 上游 `core/providers/mod.rs`）；
//!   - **字段层**（本文件 `CREDENTIAL_FIELDS` ↔ 上游 `core/account_store/`）。
//!
//! 另两层：表单层 `frontend/src/lib/addAccountConfigs.ts`、签到层
//! `src/checkin/claim.rs`，各层比对工具见 `tools/sync_agent2api_*.py`。
//!
//! ## 能力边界
//!
//! AIGX 无实现的上游入口**不显示、不自创、不假装**；注册表条目保留展示名
//! 元数据（落盘契约与导入识别需要），但前端不显示死入口 —— 已有账号走
//! `account_session/import.rs` 导入。上游由闭源插件仓实现的能力标注
//! 「由上游插件实现，AIGX 未落地」（本文件只跟进主仓元数据层）。
//!
//! ## 跟进方式
//!
//! 上游发版 → 跑 `tools/sync_agent2api_providers.py` /
//! `sync_agent2api_capabilities.py` / `sync_agent2api_credential_fields.py`
//! （`--upstream D:\GitHub\Agent2API-upstream-main --check`）diff 四层 →
//! 按能力边界规则更新。完整规则与能力矩阵见
//! `docs/aigx-vs-agent2api-gap.md` 的「上游对齐规则」章节。

/// 默认 provider（= OpenAI 兼容，需账号自带 base_url）。
pub const DEFAULT_PROVIDER_KIND: &str = "openai_compatible";

/// 上游基准清单 —— **Agent2API 的 provider 全集**（`core/providers/mod.rs`
/// 的 `PROVIDERS` 注册表，2026-11-06 核对 3.4.2：24 家 → 29 家）。
///
/// ## 这张表为什么存在
///
/// AIGX 的账号池是照着 Agent2API 的账号体系做的，供应商能力理应同步跟进。
/// 但两份代码各自演进时，「Agent2API 加了一家、AIGX 忘了加」不会有任何报错，
/// 只会在用户导入那边导出的账号时表现为「未知 provider」——静默失配。
///
/// 因此这里固化一份基准清单，并在测试里断言：**基准里的每一家都必须在
/// `PROVIDERS` 里存在**（经 `find` 归一，允许 AIGX 用自己的 kind 名，如
/// `raccoon` → `xiaohuanxiong`）。新增一家时测试会红，提醒同步。
///
/// ## 维护方式
///
/// Agent2API 新增 provider 时：
/// 1. 把它加进本表；
/// 2. 在 `PROVIDERS`（API Key 型）或 `SESSION_PROVIDERS`（登录态型）补条目；
/// 3. 登录态型还要确认 `account_login` 有对应的登录实现，否则账号加不进来。
pub const AGENT2API_PROVIDERS: &[&str] = &[
    "workbuddy",
    "workbuddy-intl",
    "raccoon",
    "catpaw",
    "autoclaw",
    "autoclaw-intl",
    "qoder",
    "qoder-intl",
    "cline-free",
    "cline-pass",
    "accio",
    "accio-cn",
    "zcode",
    "zcode-intl",
    "codearts",
    "trae",
    "loomy",
    "kuku",
    "monkeycode",
    "monkeycode-intl",
    "commandcode",
    "antigravity",
    "officeace",
    "orcaterm",
    // 上游 3.4.2 新增（2026-11-06 同步）：
    "chatglm",
    "modelscope",
    "modelscope-intl",
    "atomcode",
    "lobsterai",
];

/// 上游协议族，决定复用哪个原生 Bridge。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Openai,
    Anthropic,
    Gemini,
}

impl Protocol {
    /// 稳定的协议标识（管理面展示 / 落库参考）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }
}

/// provider 静态描述。
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    /// 稳定的 kind 标识（落库值，勿随意改名）。
    pub kind: &'static str,
    /// 管理面展示名。
    pub label: &'static str,
    /// 协议族。
    pub protocol: Protocol,
    /// 默认上游地址（空 = 必须由账号自己填写）。
    pub default_base_url: &'static str,
}

/// 支持的 provider 目录。
///
/// 前半段对齐 Agent2API 的内置/预置供应商（官方与托管端点，凭 API Key 即可转发），
/// 后半段是 AIGX 原有内置项；`openai_compatible` 永远是「自定义地址」的落点。
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        kind: "openai",
        label: "OpenAI",
        protocol: Protocol::Openai,
        default_base_url: "https://api.openai.com/v1",
    },
    ProviderSpec {
        kind: "huggingface",
        label: "HuggingFace",
        protocol: Protocol::Openai,
        default_base_url: "https://router.huggingface.co/v1",
    },
    ProviderSpec {
        kind: "nvidia",
        label: "NVIDIA NIM",
        protocol: Protocol::Openai,
        default_base_url: "https://integrate.api.nvidia.com/v1",
    },
    ProviderSpec {
        kind: "siliconflow",
        label: "硅基流动 SiliconFlow",
        protocol: Protocol::Openai,
        default_base_url: "https://api.siliconflow.cn/v1",
    },
    ProviderSpec {
        kind: "volcengine",
        label: "火山方舟",
        protocol: Protocol::Openai,
        default_base_url: "https://ark.cn-beijing.volces.com/api/v3",
    },
    ProviderSpec {
        kind: "zai",
        label: "智谱 Z.ai Coding",
        protocol: Protocol::Openai,
        default_base_url: "https://api.z.ai/api/coding/paas/v4",
    },
    ProviderSpec {
        kind: "minimax",
        label: "MiniMax",
        protocol: Protocol::Openai,
        default_base_url: "https://api.minimax.io/v1",
    },
    ProviderSpec {
        kind: "moonshot",
        label: "Moonshot Kimi",
        protocol: Protocol::Openai,
        default_base_url: "https://api.moonshot.cn/v1",
    },
    ProviderSpec {
        kind: "ollama",
        label: "Ollama（本地）",
        protocol: Protocol::Openai,
        default_base_url: "http://127.0.0.1:11434/v1",
    },
    ProviderSpec {
        kind: DEFAULT_PROVIDER_KIND,
        label: "OpenAI 兼容（自填地址）",
        protocol: Protocol::Openai,
        default_base_url: "",
    },
    ProviderSpec {
        kind: "deepseek",
        label: "DeepSeek",
        protocol: Protocol::Openai,
        default_base_url: "https://api.deepseek.com",
    },
    ProviderSpec {
        kind: "openrouter",
        label: "OpenRouter",
        protocol: Protocol::Openai,
        default_base_url: "https://openrouter.ai/api/v1",
    },
    ProviderSpec {
        kind: "anthropic",
        label: "Anthropic",
        protocol: Protocol::Anthropic,
        default_base_url: "https://api.anthropic.com",
    },
    ProviderSpec {
        kind: "gemini",
        label: "Google Gemini",
        protocol: Protocol::Gemini,
        default_base_url: "https://generativelanguage.googleapis.com/v1beta",
    },
    // ── Agent2API 内置供应商补充（同为 API Key 直连，OpenAI 兼容协议）──
    ProviderSpec {
        kind: "dashscope",
        label: "阿里云百炼 DashScope",
        protocol: Protocol::Openai,
        default_base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    },
    ProviderSpec {
        kind: "baichuan",
        label: "百川智能",
        protocol: Protocol::Openai,
        default_base_url: "https://api.baichuan-ai.com/v1",
    },
    ProviderSpec {
        kind: "yi",
        label: "零一万物 Yi",
        protocol: Protocol::Openai,
        default_base_url: "https://api.lingyiwanwu.com/v1",
    },
    ProviderSpec {
        kind: "stepfun",
        label: "阶跃星辰 StepFun",
        protocol: Protocol::Openai,
        default_base_url: "https://api.stepfun.com/v1",
    },
    ProviderSpec {
        kind: "hunyuan",
        label: "腾讯混元",
        protocol: Protocol::Openai,
        default_base_url: "https://api.hunyuan.cloud.tencent.com/v1",
    },
    ProviderSpec {
        kind: "groq",
        label: "Groq",
        protocol: Protocol::Openai,
        default_base_url: "https://api.groq.com/openai/v1",
    },
    ProviderSpec {
        kind: "mistral",
        label: "Mistral AI",
        protocol: Protocol::Openai,
        default_base_url: "https://api.mistral.ai/v1",
    },
    ProviderSpec {
        kind: "together",
        label: "Together AI",
        protocol: Protocol::Openai,
        default_base_url: "https://api.together.xyz/v1",
    },
    ProviderSpec {
        kind: "fireworks",
        label: "Fireworks AI",
        protocol: Protocol::Openai,
        default_base_url: "https://api.fireworks.ai/inference/v1",
    },
    ProviderSpec {
        kind: "deepinfra",
        label: "DeepInfra",
        protocol: Protocol::Openai,
        default_base_url: "https://api.deepinfra.com/v1/openai",
    },
    ProviderSpec {
        kind: "hyperbolic",
        label: "Hyperbolic",
        protocol: Protocol::Openai,
        default_base_url: "https://api.hyperbolic.xyz/v1",
    },
    ProviderSpec {
        kind: "nebius",
        label: "Nebius AI Studio",
        protocol: Protocol::Openai,
        default_base_url: "https://api.studio.nebius.ai/v1",
    },
    ProviderSpec {
        kind: "cerebras",
        label: "Cerebras",
        protocol: Protocol::Openai,
        default_base_url: "https://api.cerebras.ai/v1",
    },
    ProviderSpec {
        kind: "sambanova",
        label: "SambaNova",
        protocol: Protocol::Openai,
        default_base_url: "https://api.sambanova.ai/v1",
    },
    ProviderSpec {
        kind: "xai",
        label: "xAI Grok",
        protocol: Protocol::Openai,
        default_base_url: "https://api.x.ai/v1",
    },
    ProviderSpec {
        kind: "perplexity",
        label: "Perplexity",
        protocol: Protocol::Openai,
        default_base_url: "https://api.perplexity.ai",
    },
    ProviderSpec {
        kind: "cohere",
        label: "Cohere",
        protocol: Protocol::Openai,
        default_base_url: "https://api.cohere.com/compatibility/v1",
    },
    ProviderSpec {
        kind: "lmstudio",
        label: "LM Studio（本地）",
        protocol: Protocol::Openai,
        default_base_url: "http://127.0.0.1:1234/v1",
    },
    // ── 登录态型供应商（Cookie / JWT + 专用请求头）──
    // 默认地址抄自 Agent2API 的 adapter 实现（多地区的取国内版站点）。
    // 展示顺序对齐 Agent2API `providers::PROVIDERS`：同一条产品线的两个地区
    // 相邻排列（国内版在前），找「国际版」不用整段扫描。
    // ── 拆家的两家（Cline 两池 / WorkBuddy 两地）──────────────────────────
    // 与 Agent2API 一致：Cline 分 Free / Pass 两个池（上游模型带计费通道前缀，
    // 池是账号属性），WorkBuddy 分国内 / 国际两个站点。国内版保持裸 id（存量
    // 账号的落盘契约），另一版取 `<id>-intl` / `<id>-free|pass`。
    ProviderSpec {
        kind: "workbuddy",
        label: "WorkBuddy 国内版",
        protocol: Protocol::Openai,
        default_base_url: "https://copilot.tencent.com",
    },
    ProviderSpec {
        kind: "workbuddy-intl",
        label: "WorkBuddy 国际版",
        protocol: Protocol::Openai,
        default_base_url: "https://www.workbuddy.ai",
    },
    ProviderSpec {
        kind: "xiaohuanxiong",
        label: "小浣熊",
        protocol: Protocol::Openai,
        default_base_url: "https://xiaohuanxiong.com/api/web/llm/v2",
    },
    ProviderSpec {
        kind: "catpaw",
        label: "CatPaw",
        protocol: Protocol::Openai,
        default_base_url: "https://ai.catpaw.meituan.com",
    },
    ProviderSpec {
        kind: "autoclaw",
        label: "AutoClaw 国内版",
        protocol: Protocol::Openai,
        default_base_url:
            "https://autoglm-acceleration-api.zhipuai.cn/autoclaw-proxy/proxy/autoclaw",
    },
    ProviderSpec {
        kind: "autoclaw-intl",
        label: "AutoClaw 国际版",
        protocol: Protocol::Openai,
        default_base_url: "https://autoglm-api.autoglm.ai/autoclaw-proxy/proxy/autoclaw",
    },
    ProviderSpec {
        kind: "qoder",
        label: "Qoder 中国版",
        protocol: Protocol::Openai,
        default_base_url: "https://gateway.qoder.com.cn",
    },
    ProviderSpec {
        kind: "qoder-intl",
        label: "Qoder 国际版",
        protocol: Protocol::Openai,
        default_base_url: "https://api3.qoder.sh",
    },
    ProviderSpec {
        kind: "cline",
        label: "Cline",
        protocol: Protocol::Openai,
        default_base_url: "https://api.cline.bot/api/v1",
    },
    ProviderSpec {
        kind: "cline-free",
        label: "Cline Free",
        protocol: Protocol::Openai,
        default_base_url: "https://api.cline.bot/api/v1",
    },
    ProviderSpec {
        kind: "cline-pass",
        label: "Cline Pass",
        protocol: Protocol::Openai,
        default_base_url: "https://api.cline.bot/api/v1",
    },
    ProviderSpec {
        kind: "accio",
        label: "Accio",
        protocol: Protocol::Openai,
        default_base_url: "https://phoenix-gw.alibaba.com",
    },
    ProviderSpec {
        kind: "accio-cn",
        label: "Accio 国内版",
        protocol: Protocol::Openai,
        default_base_url: "https://phoenix-gw.alibaba.com",
    },
    ProviderSpec {
        kind: "zcode",
        label: "ZCode 国内版",
        protocol: Protocol::Openai,
        default_base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
    },
    ProviderSpec {
        kind: "zcode-intl",
        label: "ZCode 国际版",
        protocol: Protocol::Openai,
        default_base_url: "https://api.z.ai/api/coding/paas/v4",
    },
    ProviderSpec {
        kind: "codearts",
        label: "华为 CodeArts",
        protocol: Protocol::Openai,
        default_base_url: "https://snap-access.cn-north-4.myhuaweicloud.com",
    },
    ProviderSpec {
        kind: "trae",
        label: "Trae",
        protocol: Protocol::Openai,
        default_base_url: "https://trae-api-cn.mchost.guru",
    },
    ProviderSpec {
        kind: "loomy",
        label: "Loomy",
        protocol: Protocol::Openai,
        default_base_url: "https://loomyad.xunfei.cn/api/v1",
    },
    ProviderSpec {
        kind: "kuku",
        label: "KukuAI",
        protocol: Protocol::Openai,
        default_base_url: "https://kuku.baidu.com",
    },
    ProviderSpec {
        kind: "monkeycode",
        label: "MonkeyCode",
        protocol: Protocol::Openai,
        default_base_url: "https://monkeycode-ai.com",
    },
    ProviderSpec {
        kind: "monkeycode-intl",
        label: "MonkeyCode 国际版",
        protocol: Protocol::Openai,
        default_base_url: "https://monkeycode-ai.net",
    },
    ProviderSpec {
        kind: "commandcode",
        label: "Command Code",
        protocol: Protocol::Openai,
        default_base_url: "https://api.commandcode.ai",
    },
    ProviderSpec {
        kind: "antigravity",
        label: "Antigravity",
        protocol: Protocol::Openai,
        default_base_url: "https://daily-cloudcode-pa.googleapis.com",
    },
    ProviderSpec {
        kind: "officeace",
        label: "OfficeAce 果办",
        protocol: Protocol::Openai,
        default_base_url: "",
    },
    ProviderSpec {
        kind: "orcaterm",
        label: "OrcaTerm",
        protocol: Protocol::Openai,
        default_base_url: "https://lightai.cloud.tencent.com",
    },
    // ── 上游 3.4.2 新增的 5 家（2026-11-06 同步）────────────────────
    // 智谱清言 / ModelScope 两站 / AtomCode / LobsterAI。
    // 默认地址抄自各家公开站点（上游实现已搬进闭源插件仓，端点知识
    // 不再随主仓发布；用户可按账号覆盖）。
    ProviderSpec {
        kind: "chatglm",
        label: "智谱清言",
        protocol: Protocol::Openai,
        default_base_url: "https://chatglm.cn/api/paas/v4",
    },
    ProviderSpec {
        kind: "modelscope",
        label: "ModelScope 国内版",
        protocol: Protocol::Openai,
        default_base_url: "https://modelscope.cn",
    },
    ProviderSpec {
        kind: "modelscope-intl",
        label: "ModelScope 国际版",
        protocol: Protocol::Openai,
        default_base_url: "https://modelscope.ai",
    },
    ProviderSpec {
        kind: "atomcode",
        label: "AtomCode",
        protocol: Protocol::Openai,
        default_base_url: "https://atomgit.com",
    },
    ProviderSpec {
        kind: "lobsterai",
        label: "LobsterAI",
        protocol: Protocol::Openai,
        default_base_url: "https://lobsterai.youdao.com",
    },
];

/// 登录态型供应商的鉴权形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAuth {
    /// `Authorization: Bearer {credential}`（JWT）
    Bearer,
    /// 凭证原样作为 `Cookie` 头发送
    Cookie,
    /// 专用请求头 + `{credential}` 模板注入
    Headers,
}

impl SessionAuth {
    /// 稳定标识（管理面展示 / 前端提示用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::Cookie => "cookie",
            Self::Headers => "headers",
        }
    }
}

/// 登录态型供应商元数据。
///
/// 值抄自 Agent2API 的各家 adapter（默认端点、身份头、鉴权形态）。
#[derive(Debug, Clone, Copy)]
pub struct SessionMeta {
    pub auth: SessionAuth,
    /// 默认上游地址（空 = 由账号自填）
    pub base_url: &'static str,
    /// 默认对话路径（空 = Bridge 默认的 `/chat/completions`）
    pub chat_path: &'static str,
    /// 专用请求头模板（`{credential}` 会被替换成凭证明文）
    pub headers: &'static [(&'static str, &'static str)],
    /// 凭证粘贴提示（管理面展示）
    pub credential_hint: &'static str,
    /// 上游是否为 OpenAI 兼容形态（`false` = 需要私有协议/签名，简单转发必失败）
    pub openai_compatible: bool,
}

/// 登录态型供应商目录：与 `PROVIDERS` 中的同名 kind 对应。
///
/// 这些供应商不是 API Key 直连，而是从网页端登录态里取 Cookie / JWT，
/// 再叠加各自的专用请求头。凭证统一存 `Credential.secret`，
/// 由 `bridge::openai::AuthMode` 决定怎么发送。
pub const SESSION_PROVIDERS: &[(&str, SessionMeta)] = &[
    (
        "cline",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://api.cline.bot/api/v1",
            chat_path: "",
            headers: &[
                ("user-agent", "Cline/3.0.62"),
                ("http-referer", "https://cline.bot"),
                ("x-title", "Cline"),
                ("x-is-multiroot", "false"),
                ("x-client-type", "cline-sdk"),
                ("x-client-version", "3.0.62"),
                ("x-platform", "terminal"),
                ("x-platform-version", "3.0.62"),
                ("x-core-version", "0.0.70"),
            ],
            credential_hint: "粘贴 Cline 账号令牌（形如 workos:<JWT>，含 workos: 前缀）",
            openai_compatible: true,
        },
    ),
    // Cline 两个池：上游模型带计费通道前缀（cline-free/ / cline-pass/），
    // 池是账号属性。表单与转发头与裸 `cline` 完全一致，只是归属不同。
    (
        "cline-free",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://api.cline.bot/api/v1",
            chat_path: "",
            headers: &[
                ("user-agent", "Cline/3.0.62"),
                ("http-referer", "https://cline.bot"),
                ("x-title", "Cline"),
                ("x-is-multiroot", "false"),
                ("x-client-type", "cline-sdk"),
                ("x-client-version", "3.0.62"),
                ("x-platform", "terminal"),
                ("x-platform-version", "3.0.62"),
                ("x-core-version", "0.0.70"),
            ],
            credential_hint: "粘贴 Cline 账号令牌（免费额度池，模型名带 cline-free/ 前缀）",
            openai_compatible: true,
        },
    ),
    (
        "cline-pass",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://api.cline.bot/api/v1",
            chat_path: "",
            headers: &[
                ("user-agent", "Cline/3.0.62"),
                ("http-referer", "https://cline.bot"),
                ("x-title", "Cline"),
                ("x-is-multiroot", "false"),
                ("x-client-type", "cline-sdk"),
                ("x-client-version", "3.0.62"),
                ("x-platform", "terminal"),
                ("x-platform-version", "3.0.62"),
                ("x-core-version", "0.0.70"),
            ],
            credential_hint:
                "粘贴 Cline 账号令牌（订阅池，模型名带 cline-pass/ 前缀，需要账号有对应订阅）",
            openai_compatible: true,
        },
    ),
    (
        "workbuddy",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://copilot.tencent.com",
            chat_path: "/v2/chat/completions",
            headers: &[
                ("user-agent", "WorkBuddy/5.5.4 WorkBuddy/5.5.4 CLI/2.137.1"),
                ("x-ide-type", "WorkBuddy"),
                ("x-ide-name", "WorkBuddy"),
                ("x-ide-version", "5.5.4"),
                ("x-product", "SaaS"),
                ("x-agent-intent", "craft"),
                ("x-agent-purpose", "conversation"),
                ("x-agent-type", "main"),
                ("x-private-data", "true"),
                ("x-requested-with", "XMLHttpRequest"),
                ("x-codebuddy-request", "1"),
                ("origin", "https://copilot.tencent.com"),
                ("referer", "https://copilot.tencent.com/"),
                ("accept-language", "zh-CN"),
            ],
            credential_hint: "粘贴 WorkBuddy 的访问令牌（auth.accessToken）",
            openai_compatible: true,
        },
    ),
    // WorkBuddy 国际版（www.workbuddy.ai）：同一套协议，站点与产品标识不同。
    // 请求头里的 product / platform 与国内版不同（WorkBuddy AI / workbuddy-ai）。
    (
        "workbuddy-intl",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://www.workbuddy.ai",
            chat_path: "/v2/chat/completions",
            headers: &[
                ("user-agent", "WorkBuddy/5.5.4 WorkBuddy/5.5.4 CLI/2.137.1"),
                ("x-ide-type", "WorkBuddy"),
                ("x-ide-name", "WorkBuddy"),
                ("x-ide-version", "5.5.4"),
                ("x-product", "WorkBuddy AI"),
                ("x-agent-intent", "craft"),
                ("x-agent-purpose", "conversation"),
                ("x-agent-type", "main"),
                ("x-private-data", "true"),
                ("x-requested-with", "XMLHttpRequest"),
                ("x-codebuddy-request", "1"),
                ("origin", "https://www.workbuddy.ai"),
                ("referer", "https://www.workbuddy.ai/app"),
                ("accept-language", "en-US"),
            ],
            credential_hint: "粘贴 WorkBuddy 国际版（www.workbuddy.ai）的访问令牌",
            openai_compatible: true,
        },
    ),
    (
        "xiaohuanxiong",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://xiaohuanxiong.com/api/web/llm/v2",
            chat_path: "",
            headers: &[],
            credential_hint: "粘贴小浣熊的 JWT（Authorization: Bearer）",
            openai_compatible: true,
        },
    ),
    (
        "catpaw",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "https://ai.catpaw.meituan.com",
            chat_path: "/conversation",
            headers: &[("x-passport-token", "{credential}")],
            credential_hint: "粘贴 CatPaw 的 passport token（x-passport-token）",
            openai_compatible: false,
        },
    ),
    (
        "qoder",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://gateway.qoder.com.cn",
            chat_path: "/algo/api/v2/service/pro/sse/agent_chat_generation",
            headers: &[],
            credential_hint: "粘贴 Qoder 的访问令牌（access token，COSY 签名由网关自动完成）",
            openai_compatible: false,
        },
    ),
    // Qoder 国际版：同一套设备授权协议，只有站点主机不同。
    // 国际版有两台推理主机（api3 认设备流令牌 dt-、api2 认 PAT 换来的 jt-），
    // 这里取 PAT 路（与国内版同一入口语义）。
    (
        "qoder-intl",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://api3.qoder.sh",
            chat_path: "/algo/api/v2/service/pro/sse/agent_chat_generation",
            headers: &[],
            credential_hint: "粘贴 Qoder 国际版的访问令牌（PAT，COSY 签名由网关自动完成）",
            openai_compatible: false,
        },
    ),
    (
        "accio",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://phoenix-gw.alibaba.com",
            chat_path: "/api/adk/llm/generateContent",
            headers: &[("x-language", "zh-CN")],
            credential_hint: "粘贴 Accio 的 accessToken（上游要求放在请求体 token 字段）",
            openai_compatible: false,
        },
    ),
    // Accio 国内版（www.accio-ai.com）：与 x-package-region 头一起区分区域。
    (
        "accio-cn",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://phoenix-gw.alibaba.com",
            chat_path: "/api/adk/llm/generateContent",
            headers: &[("x-language", "zh-CN"), ("x-package-region", "CN")],
            credential_hint: "粘贴 Accio 国内版（www.accio-ai.com）的 accessToken",
            openai_compatible: false,
        },
    ),
    (
        "zcode",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
            chat_path: "",
            headers: &[],
            credential_hint: "粘贴 ZCode 的访问令牌（Bearer）",
            openai_compatible: true,
        },
    ),
    // ZCode 国际版（api.z.ai）：与国内版是两套独立账号与套餐。
    (
        "zcode-intl",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://api.z.ai/api/coding/paas/v4",
            chat_path: "",
            headers: &[],
            credential_hint: "粘贴 ZCode 国际版（api.z.ai）的访问令牌（Bearer）",
            openai_compatible: true,
        },
    ),
    (
        "codearts",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "https://snap-access.cn-north-4.myhuaweicloud.com",
            chat_path: "/api/v2/chat/completions",
            headers: &[("x-auth-token", "{credential}"), ("x-language", "zh-cn")],
            credential_hint: "粘贴华为云凭证 AK:SK[:SecurityToken]（STS 三元组，冒号或换行分隔）",
            openai_compatible: false,
        },
    ),
    (
        "trae",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "https://trae-api-cn.mchost.guru",
            chat_path: "/api/agent/v3/llm_utils_chat",
            headers: &[
                ("authorization", "Cloud-IDE-JWT {credential}"),
                ("x-cloudide-token", "{credential}"),
                ("x-ide-token", "{credential}"),
                ("x-app-id", "6eefa01c-1036-4c7e-9ca5-d891f63bfcd8"),
                ("x-app-version", "default"),
                ("x-app-version-code", "1"),
                ("x-ide-version", "1.0.0"),
                ("x-ide-version-code", "1"),
                ("x-ide-version-type", "stable"),
                ("x-device-type", "windows"),
                ("user-agent", "Trae/1.0 antigravity-cockpit-tools"),
            ],
            credential_hint: "粘贴 Trae 的 accessToken（Cloud-IDE-JWT）",
            openai_compatible: false,
        },
    ),
    (
        "loomy",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "https://loomyad.xunfei.cn/api/v1",
            chat_path: "",
            headers: &[
                ("token", "{credential}"),
                ("authorization", "Bearer {credential}"),
            ],
            credential_hint: "粘贴 Loomy 的 session 令牌（token 头）",
            openai_compatible: true,
        },
    ),
    (
        "kuku",
        SessionMeta {
            auth: SessionAuth::Cookie,
            base_url: "https://kuku.baidu.com",
            chat_path: "/wenchain/genflowpro/sse/getchatcontent",
            headers: &[],
            credential_hint: "粘贴 Kuku 的整段 Cookie（BDUSS / STOKEN）",
            openai_compatible: false,
        },
    ),
    (
        "monkeycode",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "https://monkeycode-ai.com",
            chat_path: "/api/v1/users/tasks",
            headers: &[("cookie", "monkeycode_ai_session={credential}")],
            credential_hint: "粘贴 MonkeyCode 的 session 值（monkeycode_ai_session）",
            openai_compatible: false,
        },
    ),
    // MonkeyCode 国际版（monkeycode-ai.net）：同一套协议、同一个 session
    // cookie 名，只有站点不同（见 Agent2API `monkeycode/region.rs` 的说明）。
    (
        "monkeycode-intl",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "https://monkeycode-ai.net",
            chat_path: "/api/v1/users/tasks",
            headers: &[("cookie", "monkeycode_ai_session={credential}")],
            credential_hint: "粘贴 MonkeyCode 国际版（monkeycode-ai.net）的 session 值",
            openai_compatible: false,
        },
    ),
    (
        "commandcode",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://api.commandcode.ai",
            chat_path: "/alpha/generate",
            headers: &[],
            credential_hint: "粘贴 Command Code 的 API Key（user_ 开头）",
            openai_compatible: false,
        },
    ),
    (
        "antigravity",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://daily-cloudcode-pa.googleapis.com",
            chat_path: "/v1internal:streamGenerateContent?alt=sse",
            headers: &[],
            credential_hint: "粘贴 Google OAuth access token（Bearer）",
            openai_compatible: false,
        },
    ),
    (
        "officeace",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "",
            chat_path: "/v2/chat/completions",
            headers: &[("authorization", "Basic {credential}")],
            credential_hint: "粘贴 Basic 凭证（model_app_key:model_app_secret 的 base64）",
            openai_compatible: true,
        },
    ),
    (
        "orcaterm",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://lightai.cloud.tencent.com",
            chat_path: "/assistant/chat?name=orcaterm&mode=0&csrfCode=&sseresume=true",
            headers: &[],
            credential_hint: "粘贴 OrcaTerm 的 oauth_access_token（Bearer JWT）",
            openai_compatible: false,
        },
    ),
    (
        "autoclaw",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "https://autoglm-acceleration-api.zhipuai.cn/autoclaw-proxy/proxy/autoclaw",
            chat_path: "",
            headers: &[("x-authorization", "Bearer {credential}")],
            credential_hint: "粘贴 AutoClaw 的 accessToken",
            openai_compatible: true,
        },
    ),
    // AutoClaw 国际版（autoglm-api.autoglm.ai）：与国内版同一套协议与签名指纹
    // （appId/appKey 两地逐字相同），只有站点不同。
    (
        "autoclaw-intl",
        SessionMeta {
            auth: SessionAuth::Headers,
            base_url: "https://autoglm-api.autoglm.ai/autoclaw-proxy/proxy/autoclaw",
            chat_path: "",
            headers: &[("x-authorization", "Bearer {credential}")],
            credential_hint: "粘贴 AutoClaw 国际版的 accessToken",
            openai_compatible: true,
        },
    ),
    // ── 上游 3.4.2 新增的 5 家（2026-11-06 同步）────────────────────
    // 字段形状抄自上游 account_store 的存量记录投影：
    //   chatglm     = accessToken/refreshToken 顶层 + isGuest 标记
    //   modelscope  = accessToken（ms- 令牌）+ userId/HavanaId
    //   atomcode    = accessToken/refreshToken 顶层
    //   lobsterai   = accessToken/refreshToken/uuid/firstKeyfrom/latestKeyfrom
    // 默认地址抄自各家公开站点（上游实现已搬进闭源插件仓）。
    (
        "chatglm",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://chatglm.cn/api/paas/v4",
            chat_path: "",
            headers: &[],
            credential_hint: "粘贴智谱清言的访问令牌（JWT，三段点分）",
            openai_compatible: true,
        },
    ),
    (
        "modelscope",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://modelscope.cn",
            chat_path: "/api/v1/chat/completions",
            headers: &[],
            credential_hint: "粘贴 ModelScope 的 API 令牌（ms- 开头）",
            openai_compatible: true,
        },
    ),
    // ModelScope 国际版（modelscope.ai）：与国内版同一套协议，站点不同。
    (
        "modelscope-intl",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://modelscope.ai",
            chat_path: "/api/v1/chat/completions",
            headers: &[],
            credential_hint: "粘贴 ModelScope 国际版的 API 令牌（ms- 开头）",
            openai_compatible: true,
        },
    ),
    (
        "atomcode",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://atomgit.com",
            chat_path: "",
            headers: &[],
            credential_hint: "粘贴 AtomCode 的访问令牌（accessToken）",
            openai_compatible: true,
        },
    ),
    (
        "lobsterai",
        SessionMeta {
            auth: SessionAuth::Bearer,
            base_url: "https://lobsterai.youdao.com",
            chat_path: "",
            headers: &[],
            credential_hint:
                "粘贴 LobsterAI 的访问令牌（accessToken；续期需 uuid/keyfrom，一般由网页登录下发）",
            openai_compatible: true,
        },
    ),
];

/// 查登录态型供应商元数据（非登录态型返回 None）。
pub fn session_meta(kind: &str) -> Option<&'static SessionMeta> {
    let lower = kind.trim().to_ascii_lowercase();
    SESSION_PROVIDERS
        .iter()
        .find(|(k, _)| *k == lower)
        .map(|(_, meta)| meta)
}

/// 全部 provider（管理面下拉用）。
pub fn all() -> &'static [ProviderSpec] {
    PROVIDERS
}

/// 按 kind（含常见别名）查找 provider。
///
/// 先按 kind 精确命中（这样 `openai` 这类既是内置 kind 又是历史别名的值
/// 不会被重定向），再走别名表兼容旧配置。
pub fn find(kind: &str) -> Option<&'static ProviderSpec> {
    let lower = kind.trim().to_ascii_lowercase();
    if let Some(spec) = PROVIDERS.iter().find(|p| p.kind == lower) {
        return Some(spec);
    }
    let canonical: &str = match lower.as_str() {
        "" => DEFAULT_PROVIDER_KIND,
        "openai-compatible" | "oaicompat" => DEFAULT_PROVIDER_KIND,
        "claude" => "anthropic",
        "google" | "google_gemini" => "gemini",
        "hf" | "hugging_face" => "huggingface",
        "nim" => "nvidia",
        "silicon" => "siliconflow",
        "volces" | "ark" | "doubao" | "bytedance" => "volcengine",
        "z.ai" | "zhipu" | "glm" => "zai",
        "kimi" => "moonshot",
        "qwen" | "aliyun" | "bailian" => "dashscope",
        "lingyiwanwu" | "01.ai" => "yi",
        "step" => "stepfun",
        "tencent" | "hunyuan-turbos" => "hunyuan",
        "groqcloud" => "groq",
        "together_ai" => "together",
        "fireworks_ai" => "fireworks",
        "deep-infra" => "deepinfra",
        "nebius_ai" => "nebius",
        "samba" => "sambanova",
        "grok" => "xai",
        "pplx" => "perplexity",
        // Agent2API 把小浣熊叫 `raccoon`，AIGX 落盘 id 是 `xiaohuanxiong`：
        // 两个方向都认（导入 Agent2API 导出的账号时靠这条归一）
        "raccoon" => "xiaohuanxiong",
        other => other,
    };
    PROVIDERS.iter().find(|p| p.kind == canonical)
}

/// provider 落库前的归一化 kind（未知名保持原样，由 adapter 报错暴露）。
pub fn normalize_kind(kind: &str) -> String {
    find(kind)
        .map(|p| p.kind.to_string())
        .unwrap_or_else(|| kind.trim().to_string())
}

// ── 凭证字段模板（Agent2API 账号结构同步）─────────────────────────────
//
// Agent2API 的账号记录是**扁平 JSON**：除通用键（accessToken/refreshToken/
// expiresAt/userId —— AIGX 由 `Credential.secret` / `refresh_secret` /
// `expires_at` 承担）外，各家还有自己的具名字段（trae 的 deviceId/
// machineId/uid、codearts 的 secret_access_key/security_token、officeace
// 的双层凭据……）。这些落在 AIGX `Credential.fields`。
//
// 这张表声明「每家该有哪些具名字段」，用于：
// - 导入 Agent2API 账号记录时按表搬运（漏搬 = 转发时才发现缺字段）；
// - 管理面展示「这家要填哪些字段」；
// - `tools/sync_agent2api_credential_fields.py` 与上游
//   `providers/*/credentials.rs` 对拍，发现漂移（CI upstream-drift）。

/// 一个具名凭证字段（键名与 Agent2API 落库记录一致）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialField {
    /// 落库键名（与 Agent2API 记录一致，如 `deviceId` / `secret_access_key`）。
    pub name: &'static str,
    /// 转发/续期链路缺了它会直接失败（上游 `valid()` 的判定；管理面据此标必填）。
    pub required: bool,
    /// 用途一句话（管理面展示；同步工具从上游字段注释提取）。
    pub hint: &'static str,
}

/// 各供应商的凭证字段模板（按归一后的 kind 索引）。
///
/// **BEGIN/END 标记之间是生成区**：由
/// `tools/sync_agent2api_credential_fields.py --write` 与上游对拍重写，
/// 手工改动会被下次同步冲掉（要改就改工具或上游）。
// BEGIN AGENT2API-CREDENTIAL-FIELDS
pub const CREDENTIAL_FIELDS: &[(&str, &[CredentialField])] = &[
    (
        "accio",
        &[
            CredentialField {
                name: "deviceId",
                required: false,
                hint: "推理请求的设备指纹（本网关按账号生成）",
            },
            CredentialField {
                name: "email",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "mode",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "nickname",
                required: false,
                hint: "",
            },
        ],
    ),
    (
        "antigravity",
        &[
            CredentialField {
                name: "email",
                required: false,
                hint: "账号邮箱（可能为空 —— 展示名与身份来源）",
            },
            CredentialField {
                name: "name",
                required: false,
                hint: "展示名（记录的备注名）",
            },
            CredentialField {
                name: "projectId",
                required: false,
                hint: "`cloudaicompanionProject`（可能为空 —— 由 `project.rs` 发现）",
            },
        ],
    ),
    (
        "codearts",
        &[
            CredentialField {
                name: "domain_id",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "login_type",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "secret_access_key",
                required: true,
                hint: "",
            },
            CredentialField {
                name: "security_token",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "user_name",
                required: false,
                hint: "",
            },
        ],
    ),
    (
        "commandcode",
        &[
            CredentialField {
                name: "apiKey",
                required: false,
                hint: "上游 API Key（`user_` 开头的明文）",
            },
            CredentialField {
                name: "name",
                required: false,
                hint: "展示名（记录的备注名）",
            },
        ],
    ),
    (
        "loomy",
        &[
            CredentialField {
                name: "name",
                required: false,
                hint: "展示名（记录的备注名）",
            },
            CredentialField {
                name: "phone",
                required: false,
                hint: "手机号（可选，展示用）",
            },
        ],
    ),
    (
        "monkeycode",
        &[
            CredentialField {
                name: "imageId",
                required: false,
                hint: "VM 镜像 UUID（创建任务必需；可能为空 —— 见模块头）",
            },
            CredentialField {
                name: "name",
                required: false,
                hint: "展示名（记录的备注名）",
            },
        ],
    ),
    (
        "officeace",
        &[
            CredentialField {
                name: "accessKeyId",
                required: false,
                hint: "控制面临时凭据（手工只导入网关凭据时全空）",
            },
            CredentialField {
                name: "modelAppKey",
                required: false,
                hint: "网关凭据：转发用的那一对",
            },
            CredentialField {
                name: "modelAppSecret",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "projectId",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "secretAccessKey",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "securityToken",
                required: false,
                hint: "",
            },
        ],
    ),
    (
        "qoder",
        &[
            CredentialField {
                name: "email",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "machineId",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "mode",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "nickname",
                required: false,
                hint: "",
            },
        ],
    ),
    (
        "trae",
        &[
            CredentialField {
                name: "apiHost",
                required: false,
                hint: "OAuth/积分那条链的 host（`ExchangeToken` 的第一候选）。",
            },
            CredentialField {
                name: "authClientId",
                required: false,
                hint: "这张 refreshToken **当初是在哪个 OAuth 应用里 mint 的**（上游在",
            },
            CredentialField {
                name: "deviceId",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "devicePrivateKey",
                required: false,
                hint: "登录时那套设备密钥的私钥（PKCS#8 PEM）。",
            },
            CredentialField {
                name: "devicePublicKey",
                required: false,
                hint: "登录时那套设备密钥的**公钥**（SPKI PEM，随 ExchangeToken 上传过的那份）。",
            },
            CredentialField {
                name: "domain",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "enterpriseId",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "machineId",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "nickname",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "uid",
                required: false,
                hint: "",
            },
            CredentialField {
                name: "variant",
                required: false,
                hint: "",
            },
        ],
    ),
];
// END AGENT2API-CREDENTIAL-FIELDS

/// 查某 provider 的凭证字段模板（kind 先归一；空表 = 单 secret 直连型）。
pub fn credential_fields_for(kind: &str) -> &'static [CredentialField] {
    let canonical = normalize_kind(kind);
    CREDENTIAL_FIELDS
        .iter()
        .find(|(k, _)| *k == canonical)
        .map(|(_, fields)| *fields)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_to_canonical_kind() {
        assert_eq!(find("openai").map(|p| p.kind), Some("openai"));
        assert_eq!(
            find("openai-compatible").map(|p| p.kind),
            Some(DEFAULT_PROVIDER_KIND)
        );
        assert_eq!(find("claude").map(|p| p.kind), Some("anthropic"));
        assert_eq!(find("zhipu").map(|p| p.kind), Some("zai"));
        assert_eq!(find("kimi").map(|p| p.kind), Some("moonshot"));
        assert_eq!(find("").map(|p| p.kind), Some(DEFAULT_PROVIDER_KIND));
    }

    #[test]
    fn builtin_providers_have_default_endpoints() {
        for spec in all() {
            // 自填地址型（OpenAI 兼容 / 登录态型）允许没有默认端点
            if spec.kind == DEFAULT_PROVIDER_KIND || session_meta(spec.kind).is_some() {
                continue;
            }
            assert!(
                spec.default_base_url.starts_with("http"),
                "{} should ship a default endpoint",
                spec.kind
            );
        }
    }

    #[test]
    fn session_providers_are_registered_and_documented() {
        for (kind, meta) in SESSION_PROVIDERS {
            assert!(
                all().iter().any(|p| p.kind == *kind),
                "session provider {kind} missing from PROVIDERS"
            );
            assert!(
                !meta.credential_hint.is_empty(),
                "session provider {kind} needs a credential hint"
            );
            if meta.auth == SessionAuth::Headers {
                assert!(
                    !meta.headers.is_empty(),
                    "headers auth for {kind} needs header templates"
                );
            }
        }
        assert_eq!(
            SESSION_PROVIDERS.len(),
            30,
            "登录态供应商应为 30 家（Agent2API 的 29 家 + AIGX 自有的 cline）"
        );
    }

    #[test]
    fn session_meta_base_url_matches_provider_default() {
        for (kind, meta) in SESSION_PROVIDERS {
            let spec = find(kind).expect("session provider must exist in PROVIDERS");
            assert_eq!(
                meta.base_url, spec.default_base_url,
                "{kind} 的默认地址要与 PROVIDERS 一致"
            );
        }
    }

    #[test]
    fn session_providers_split_openai_compatible_and_private() {
        let compatible = SESSION_PROVIDERS
            .iter()
            .filter(|(_, meta)| meta.openai_compatible)
            .count();
        assert_eq!(compatible, 17, "可直接转发（OpenAI 兼容）的应为 17 家");
        let private: Vec<&str> = SESSION_PROVIDERS
            .iter()
            .filter(|(_, meta)| !meta.openai_compatible)
            .map(|(kind, _)| *kind)
            .collect();
        assert_eq!(private.len(), 13, "私有协议供应商应为 13 家");
        for kind in private {
            assert!(
                crate::bridge::session::supports(kind),
                "{kind} 标记为私有协议，但缺少专用 Bridge"
            );
        }
    }

    #[test]
    fn session_meta_lookup_normalizes_kind() {
        assert!(session_meta("Cline").is_some());
        assert!(session_meta(" trae ").is_some());
        assert!(session_meta("openai").is_none());
    }

    #[test]
    fn credential_fields_cover_session_providers() {
        // 多层凭证家的关键字段必须在模板里（同步工具与上游对拍的底线断言，
        // 详细字段清单由 CI 的 upstream-drift 巡检把关）。
        let expectations: &[(&str, &[&str])] = &[
            ("trae", &["uid", "deviceId", "machineId"]),
            ("codearts", &["secret_access_key", "security_token"]),
            ("officeace", &["modelAppKey", "modelAppSecret"]),
            ("qoder", &["machineId"]),
        ];
        for (kind, required_names) in expectations {
            let fields = credential_fields_for(kind);
            assert!(
                !fields.is_empty(),
                "credential fields for {kind} must not be empty"
            );
            for name in *required_names {
                assert!(
                    fields.iter().any(|f| f.name == *name),
                    "credential fields for {kind} missing {name}"
                );
            }
        }
        // Agent2API 叫 `raccoon`、AIGX 叫 `xiaohuanxiong` —— 模板查询要
        // 走同一套别名归一，导入那边导出的账号才不会查空。
        let raccoon = credential_fields_for("raccoon");
        let xiaohuanxiong = credential_fields_for("xiaohuanxiong");
        assert_eq!(raccoon, xiaohuanxiong);
    }

    #[test]
    fn openai_compatible_needs_explicit_base_url() {
        let spec = find(DEFAULT_PROVIDER_KIND).unwrap();
        assert!(spec.default_base_url.is_empty());
    }

    #[test]
    fn provider_kinds_are_unique() {
        let mut kinds: Vec<&str> = all().iter().map(|p| p.kind).collect();
        let len = kinds.len();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), len, "duplicated provider kind in PROVIDERS");
    }

    #[test]
    fn new_builtin_aliases_resolve() {
        assert_eq!(find("qwen").map(|p| p.kind), Some("dashscope"));
        assert_eq!(find("doubao").map(|p| p.kind), Some("volcengine"));
        assert_eq!(find("grok").map(|p| p.kind), Some("xai"));
        assert_eq!(find("pplx").map(|p| p.kind), Some("perplexity"));
    }

    /// Agent2API 的每一家 provider 都必须能在 AIGX 里找到。
    ///
    /// 这条测试是「供应商同步」的守门人：Agent2API 新增一家而 AIGX 没跟上时，
    /// 这里会红，而不是等到用户导入账号时报「未知 provider」。
    #[test]
    fn every_agent2api_provider_is_known() {
        let mut missing: Vec<&str> = Vec::new();
        for id in AGENT2API_PROVIDERS {
            if find(id).is_none() {
                missing.push(id);
            }
        }
        assert!(
            missing.is_empty(),
            "以下 Agent2API provider 在 AIGX 的 PROVIDERS 里不存在：{missing:?}。
             请补 `PROVIDERS` 条目（API Key 型）或 `SESSION_PROVIDERS`（登录态型）。"
        );
    }

    /// 登录态型 provider 必须同时具备 `PROVIDERS` 条目与 `SESSION_PROVIDERS` 元数据。
    ///
    /// 只补一处会让账号加得进来却无法转发（缺协议族）或无法登录（缺默认端点）。
    #[test]
    fn session_providers_have_both_entries() {
        let mut missing_spec: Vec<&str> = Vec::new();
        let mut missing_meta: Vec<&str> = Vec::new();
        for (kind, _) in SESSION_PROVIDERS {
            if !PROVIDERS.iter().any(|p| p.kind == *kind) {
                missing_spec.push(kind);
            }
            if session_meta(kind).is_none() {
                missing_meta.push(kind);
            }
        }
        assert!(
            missing_spec.is_empty(),
            "SESSION_PROVIDERS 里这些 kind 缺 PROVIDERS 条目：{missing_spec:?}"
        );
        assert!(
            missing_meta.is_empty(),
            "这些 kind 缺 SessionMeta：{missing_meta:?}"
        );
    }
}
