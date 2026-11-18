//! Hub — 分发器模块
//!
//! 参考 aisix 项目的 Hub 架构，将请求分发到合适的 Bridge 实现。
//! 提供两级分发机制：专用提供商 bridge → 通用适配器 family bridge。
//! 使用 Adapter 枚举（替代 String 键）来标识适配器族。
//!
//! 两级分发（dispatch_two_tier）：
//! 1. 先查 specialized_bridges（专用提供商，如 cloudflare）
//! 2. 再查 family_bridges（适配器族，如 openai、anthropic、bedrock、vertex）

use dashmap::DashMap;
use std::sync::Arc;

use crate::bridge::Bridge;

/// 适配器族枚举，参考 aisix Adapter 设计
///
/// 表示上游传输协议族（wire-shape），用于 family_bridges 的查找。
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum Adapter {
    /// OpenAI 兼容协议
    Openai,
    /// Anthropic 协议
    Anthropic,
    /// Azure OpenAI 协议
    AzureOpenai,
    /// AWS Bedrock 协议
    Bedrock,
    /// Vertex AI 协议
    Vertex,
}

impl std::str::FromStr for Adapter {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            "azure" | "azure-openai" | "azure_openai" => Ok(Self::AzureOpenai),
            "bedrock" => Ok(Self::Bedrock),
            "vertex" | "vertex-ai" | "vertex_ai" => Ok(Self::Vertex),
            _ => Err(format!("unknown adapter: {}", s)),
        }
    }
}

/// Hub 分发器
///
/// 持有所有已注册的 Bridge 实现，通过提供商名称或适配器族进行分发。
/// 参考 aisix Hub 的两级分发设计：
/// 1. specialized_bridges — 专用提供商 bridge（如 cloudflare）
/// 2. family_bridges — 适配器族 bridge（如 openai、anthropic）
///
/// DashMap 允许在构造后注册 bridge（用于测试和未来动态重载场景），
/// 无需在查找路径上持有锁。
pub struct Hub {
    /// 专用提供商 bridge
    specialized_bridges: DashMap<String, Arc<dyn Bridge>>,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            specialized_bridges: DashMap::new(),
        }
    }

    /// 注册专用提供商 bridge
    pub fn register_specialized(&self, provider: impl Into<String>, bridge: Arc<dyn Bridge>) {
        self.specialized_bridges.insert(provider.into(), bridge);
    }

    /// 获取专用提供商 bridge
    pub fn get_specialized(&self, provider: &str) -> Option<Arc<dyn Bridge>> {
        self.specialized_bridges.get(provider).map(|r| r.clone())
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}
