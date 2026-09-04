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

/// 路由信息，参考 aisix ProviderKey 的 provider + adapter 字段设计
///
/// 组合了提供商标识符和适配器族，用于 dispatch_two_tier 查找。
#[derive(Debug, Clone)]
pub struct ProviderRoute {
    /// 提供商标识符（如 "cloudflare"、"openai"、"deepseek"）
    pub provider: String,
    /// 适配器族（可选），当没有专用提供商时使用适配器族兜底
    pub adapter: Option<Adapter>,
}

impl ProviderRoute {
    pub fn new(provider: impl Into<String>, adapter: Option<Adapter>) -> Self {
        Self {
            provider: provider.into(),
            adapter,
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
    /// 适配器族 bridge（使用 Adapter 枚举键）
    family_bridges: DashMap<Adapter, Arc<dyn Bridge>>,
}

impl Hub {
    pub fn new() -> Self {
        Self {
            specialized_bridges: DashMap::new(),
            family_bridges: DashMap::new(),
        }
    }

    /// 注册专用提供商 bridge
    pub fn register_specialized(&self, provider: impl Into<String>, bridge: Arc<dyn Bridge>) {
        self.specialized_bridges.insert(provider.into(), bridge);
    }

    /// 注册适配器族 bridge
    pub fn register_family(&self, adapter: Adapter, bridge: Arc<dyn Bridge>) {
        self.family_bridges.insert(adapter, bridge);
    }

    /// 获取专用提供商 bridge
    pub fn get_specialized(&self, provider: &str) -> Option<Arc<dyn Bridge>> {
        self.specialized_bridges.get(provider).map(|r| r.clone())
    }

    /// 获取适配器族 bridge
    pub fn get_family(&self, adapter: Adapter) -> Option<Arc<dyn Bridge>> {
        self.family_bridges.get(&adapter).map(|r| r.clone())
    }

    /// 两级分发：先查专用提供商，再查适配器族
    ///
    /// 参考 aisix 的 dispatch_two_tier 模式：
    /// 1. 第一级：专用提供商 bridge（如 cloudflare）
    /// 2. 第二级：适配器族 bridge（如 openai、anthropic）
    ///
    /// 接受 ProviderRoute 作为参数，该结构组合了 provider 和 adapter 字段。
    /// 返回 None 表示两者均未注册，由调用方决定如何报告。
    ///
    /// 参考 aisix 的 dispatch_two_tier(&self, pk: &ProviderKey) -> Option<Arc<dyn Bridge>>。
    pub fn dispatch_two_tier(&self, route: &ProviderRoute) -> Option<Arc<dyn Bridge>> {
        // 第一级：专用提供商 bridge
        if let Some(b) = self.specialized_bridges.get(&route.provider) {
            return Some(b.clone());
        }
        // 第二级：适配器族 bridge
        if let Some(adapter) = &route.adapter {
            if let Some(b) = self.family_bridges.get(adapter) {
                return Some(b.clone());
            }
        }
        None
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}
