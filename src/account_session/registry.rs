//! 账号凭证 provider 目录 —— 「粘贴哪家 Key，用哪个原生 Bridge」。
//!
//! provider 只描述协议族与默认上游地址，实际的请求构造**复用既有 Bridge**：
//! - `Openai` → `bridge::openai::make_bridge`（OpenAI 兼容上游）
//! - `Anthropic` → `bridge::anthropic::make_bridge`（x-api-key + anthropic-version）
//! - `Gemini` → `bridge::gemini::make_bridge`（x-goog-api-key）
//!
//! 新增一家 OpenAI 兼容供应商（同一把 Key 换域名）只需在 `PROVIDERS` 里加一条。

/// 默认 provider（= OpenAI 兼容，需账号自带 base_url）。
pub const DEFAULT_PROVIDER_KIND: &str = "openai_compatible";

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
        label: "Z.ai Coding",
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
];

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
            if spec.kind == DEFAULT_PROVIDER_KIND {
                assert!(spec.default_base_url.is_empty());
            } else {
                assert!(
                    spec.default_base_url.starts_with("http"),
                    "{} should ship a default endpoint",
                    spec.kind
                );
            }
        }
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
}
