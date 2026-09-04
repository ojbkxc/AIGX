//! IP 管理模块 — 全局 IP 白名单/黑名单过滤。
//!
//! 参照 burncloud `crates/database/crates/router/src/token.rs` 的
//! `is_ip_allowed` + `crates/service/crates/ip/` 的 IP 服务设计，
//! 在 AIGX 中实现全局 IP 过滤（不限于 token 级）：
//!
//! - **白名单**：若非空，则仅允许列表内 IP 访问（CIDR/精确匹配）
//! - **黑名单**：拒绝列表内 IP 访问
//! - 同时存在时：先白名单（允许），再黑名单（拒绝）
//! - 支持 IPv4 CIDR（如 `192.168.0.0/24`），IPv6 仅精确匹配
//!
//! 不依赖 `ipnet` crate（避免新增依赖），IPv4 CIDR 用位运算自实现。
//! 持久化复用 `FileStore`（与 `ApiKeyStore` 同模式）。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::storage::FileStore;

/// IP 过滤规则 — 单条 IP 或 CIDR 表示法。
///
/// 参照 burncloud `is_ip_allowed` 的逗号分隔列表，扩展为结构化规则。
/// 支持形式：
/// - 精确 IPv4：`192.168.1.1`
/// - IPv4 CIDR：`192.168.0.0/24`
/// - 精确 IPv6：`::1`、`2001:db8::1`
/// - IPv6 CIDR：暂不支持（精确匹配兜底）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpRule {
    /// 规则内容（IP 或 CIDR 字符串）
    pub pattern: String,
    /// 可选备注（人类可读说明）
    #[serde(default)]
    pub note: String,
}

/// IP 过滤配置 — 白名单 + 黑名单。
///
/// 持久化到 FileStore，通过 `IpFilterStore` 加载/保存。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpFilter {
    /// 白名单（None 或空 = 不限制）
    #[serde(default)]
    pub whitelist: Vec<IpRule>,
    /// 黑名单（None 或空 = 不限制）
    #[serde(default)]
    pub blacklist: Vec<IpRule>,
    /// 是否启用全局过滤
    #[serde(default)]
    pub enabled: bool,
}

impl IpFilter {
    /// 检查 IP 是否被允许访问。
    ///
    /// 逻辑（参照 burncloud `is_ip_allowed` 扩展）：
    /// 1. 未启用 → 允许
    /// 2. 白名单非空 → 必须匹配白名单
    /// 3. 黑名单非空 → 不能匹配黑名单
    /// 4. 同时存在 → 先白名单（允许），再黑名单（拒绝）
    pub fn allows_ip(&self, ip: &str) -> bool {
        if !self.enabled {
            return true;
        }

        // 白名单非空：必须匹配
        if !self.whitelist.is_empty() && !self.whitelist.iter().any(|r| matches_ip(ip, &r.pattern))
        {
            return false;
        }

        // 黑名单非空：不能匹配
        if !self.blacklist.is_empty() && self.blacklist.iter().any(|r| matches_ip(ip, &r.pattern)) {
            return false;
        }

        true
    }

    /// 添加白名单规则
    pub fn add_whitelist(&mut self, pattern: impl Into<String>, note: impl Into<String>) {
        self.whitelist.push(IpRule {
            pattern: pattern.into(),
            note: note.into(),
        });
    }

    /// 添加黑名单规则
    pub fn add_blacklist(&mut self, pattern: impl Into<String>, note: impl Into<String>) {
        self.blacklist.push(IpRule {
            pattern: pattern.into(),
            note: note.into(),
        });
    }

    /// 移除白名单规则（按 pattern 匹配）
    pub fn remove_whitelist(&mut self, pattern: &str) -> bool {
        let before = self.whitelist.len();
        self.whitelist.retain(|r| r.pattern != pattern);
        self.whitelist.len() != before
    }

    /// 移除黑名单规则（按 pattern 匹配）
    pub fn remove_blacklist(&mut self, pattern: &str) -> bool {
        let before = self.blacklist.len();
        self.blacklist.retain(|r| r.pattern != pattern);
        self.blacklist.len() != before
    }
}

/// 检查 IP 是否匹配规则（精确或 CIDR）。
///
/// 参照 burncloud `is_ip_allowed` 的精确匹配，扩展为 CIDR 支持：
/// - 含 `/` → CIDR 匹配（仅 IPv4）
/// - 不含 `/` → 精确匹配
fn matches_ip(ip: &str, pattern: &str) -> bool {
    if pattern.contains('/') {
        matches_cidr(ip, pattern)
    } else {
        ip == pattern
    }
}

/// IPv4 CIDR 匹配（自实现，不依赖 ipnet crate）。
///
/// 格式：`a.b.c.d/prefix`（prefix 0-32）。
/// 算法：将 IP 和 CIDR 地址转为 u32，用掩码位运算比较。
fn matches_cidr(ip: &str, cidr: &str) -> bool {
    let (cidr_addr, prefix) = match parse_cidr(cidr) {
        Some(v) => v,
        None => return false,
    };
    let ip_addr: u32 = match parse_ipv4(ip) {
        Some(v) => v,
        None => return false,
    };
    if prefix == 0 {
        // /0 匹配所有 IPv4
        return true;
    }
    let mask: u32 = if prefix >= 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    };
    (ip_addr & mask) == (cidr_addr & mask)
}

/// 解析 CIDR 字符串为 (IPv4, prefix)
fn parse_cidr(s: &str) -> Option<(u32, u8)> {
    let (addr_str, prefix_str) = s.split_once('/')?;
    let addr = parse_ipv4(addr_str)?;
    let prefix: u8 = prefix_str.parse().ok()?;
    if prefix > 32 {
        return None;
    }
    Some((addr, prefix))
}

/// 解析 IPv4 地址为 u32
fn parse_ipv4(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut result: u32 = 0;
    for part in parts {
        let octet: u32 = part.parse().ok()?;
        if octet > 255 {
            return None;
        }
        result = (result << 8) | octet;
    }
    Some(result)
}

/// IP 过滤存储 — 持久化 `IpFilter` 到 FileStore。
///
/// 参照 `ApiKeyStore` 的模式：RwLock + FileStore。
pub struct IpFilterStore {
    store: Arc<FileStore>,
    filter: Arc<RwLock<IpFilter>>,
}

/// 存储键名
const STORAGE_KEY: &str = "ip_filter";

impl IpFilterStore {
    /// 创建新的 IP 过滤存储
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            filter: Arc::new(RwLock::new(IpFilter::default())),
        }
    }

    /// 从存储加载 IP 过滤配置
    pub fn load(&self) -> Result<(), anyhow::Error> {
        if let Some(filter) = self.store.get::<IpFilter>(STORAGE_KEY)? {
            *self.filter.write() = filter;
            tracing::info!(
                "Loaded IP filter: {} whitelist, {} blacklist rules, enabled={}",
                self.filter.read().whitelist.len(),
                self.filter.read().blacklist.len(),
                self.filter.read().enabled
            );
        }
        Ok(())
    }

    /// 保存 IP 过滤配置到存储
    fn save(&self) -> Result<(), anyhow::Error> {
        let filter = self.filter.read().clone();
        self.store.put(STORAGE_KEY, &filter)?;
        Ok(())
    }

    /// 获取当前过滤配置快照
    pub fn get(&self) -> IpFilter {
        self.filter.read().clone()
    }

    /// 检查 IP 是否允许访问
    pub fn allows_ip(&self, ip: &str) -> bool {
        self.filter.read().allows_ip(ip)
    }

    /// 启用/禁用全局 IP 过滤
    pub fn set_enabled(&self, enabled: bool) -> Result<(), anyhow::Error> {
        self.filter.write().enabled = enabled;
        self.save()
    }

    /// 添加白名单规则
    pub fn add_whitelist(
        &self,
        pattern: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<(), anyhow::Error> {
        self.filter.write().add_whitelist(pattern, note);
        self.save()
    }

    /// 添加黑名单规则
    pub fn add_blacklist(
        &self,
        pattern: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<(), anyhow::Error> {
        self.filter.write().add_blacklist(pattern, note);
        self.save()
    }

    /// 移除白名单规则
    pub fn remove_whitelist(&self, pattern: &str) -> Result<bool, anyhow::Error> {
        let removed = self.filter.write().remove_whitelist(pattern);
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// 移除黑名单规则
    pub fn remove_blacklist(&self, pattern: &str) -> Result<bool, anyhow::Error> {
        let removed = self.filter.write().remove_blacklist(pattern);
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// 替换整个过滤配置（管理 API 用）
    pub fn replace(&self, filter: IpFilter) -> Result<(), anyhow::Error> {
        *self.filter.write() = filter;
        self.save()
    }
}

/// IP 过滤错误（供调用方映射 HTTP 状态码）
#[derive(Debug, Clone, thiserror::Error)]
pub enum IpFilterError {
    #[error("IP '{0}' is blocked by blacklist")]
    Blacklisted(String),
    #[error("IP '{0}' is not in whitelist")]
    NotWhitelisted(String),
}

/// 检查 IP 是否允许，返回结构化错误。
///
/// 供请求处理中间件调用（参照 `ApiKeyError::IpNotAllowed`）。
pub fn check_ip(filter: &IpFilter, ip: &str) -> Result<(), IpFilterError> {
    if !filter.enabled {
        return Ok(());
    }
    // 黑名单优先检查（拒绝）
    if !filter.blacklist.is_empty() && filter.blacklist.iter().any(|r| matches_ip(ip, &r.pattern)) {
        return Err(IpFilterError::Blacklisted(ip.to_string()));
    }
    // 白名单检查（允许）
    if !filter.whitelist.is_empty() && !filter.whitelist.iter().any(|r| matches_ip(ip, &r.pattern))
    {
        return Err(IpFilterError::NotWhitelisted(ip.to_string()));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn parse_ipv4_correct() {
        assert_eq!(parse_ipv4("0.0.0.0"), Some(0));
        assert_eq!(parse_ipv4("255.255.255.255"), Some(u32::MAX));
        assert_eq!(parse_ipv4("192.168.1.1"), Some(0xC0A80101));
        assert_eq!(parse_ipv4("10.0.0.1"), Some(0x0A000001));
    }

    #[test]
    fn parse_ipv4_invalid() {
        assert_eq!(parse_ipv4("256.0.0.1"), None);
        assert_eq!(parse_ipv4("1.2.3"), None);
        assert_eq!(parse_ipv4("1.2.3.4.5"), None);
        assert_eq!(parse_ipv4("abc"), None);
    }

    #[test]
    fn parse_cidr_correct() {
        assert_eq!(parse_cidr("0.0.0.0/0"), Some((0, 0)));
        assert_eq!(parse_cidr("192.168.0.0/16"), Some((0xC0A80000, 16)));
        assert_eq!(parse_cidr("10.0.0.0/8"), Some((0x0A000000, 8)));
        assert_eq!(parse_cidr("1.2.3.4/32"), Some((0x01020304, 32)));
    }

    #[test]
    fn parse_cidr_invalid() {
        assert_eq!(parse_cidr("1.2.3.4/33"), None);
        assert_eq!(parse_cidr("1.2.3.4"), None);
        assert_eq!(parse_cidr("256.0.0.0/8"), None);
    }

    #[test]
    fn matches_cidr_exact_24() {
        assert!(matches_cidr("192.168.1.1", "192.168.1.0/24"));
        assert!(matches_cidr("192.168.1.255", "192.168.1.0/24"));
        assert!(!matches_cidr("192.168.2.1", "192.168.1.0/24"));
    }

    #[test]
    fn matches_cidr_exact_16() {
        assert!(matches_cidr("192.168.0.1", "192.168.0.0/16"));
        assert!(matches_cidr("192.168.255.255", "192.168.0.0/16"));
        assert!(!matches_cidr("192.169.0.1", "192.168.0.0/16"));
    }

    #[test]
    fn matches_cidr_zero_prefix() {
        // /0 匹配所有 IPv4
        assert!(matches_cidr("1.2.3.4", "0.0.0.0/0"));
        assert!(matches_cidr("255.255.255.255", "0.0.0.0/0"));
    }

    #[test]
    fn matches_cidr_full_prefix() {
        // /32 精确匹配
        assert!(matches_cidr("1.2.3.4", "1.2.3.4/32"));
        assert!(!matches_cidr("1.2.3.5", "1.2.3.4/32"));
    }

    #[test]
    fn matches_ip_exact() {
        assert!(matches_ip("192.168.1.1", "192.168.1.1"));
        assert!(!matches_ip("192.168.1.2", "192.168.1.1"));
    }

    #[test]
    fn matches_ip_cidr() {
        assert!(matches_ip("192.168.1.1", "192.168.1.0/24"));
        assert!(!matches_ip("192.168.2.1", "192.168.1.0/24"));
    }

    #[test]
    fn ip_filter_disabled_allows_all() {
        let filter = IpFilter {
            whitelist: vec![IpRule {
                pattern: "1.2.3.4".to_string(),
                note: String::new(),
            }],
            blacklist: vec![IpRule {
                pattern: "5.6.7.8".to_string(),
                note: String::new(),
            }],
            enabled: false,
        };
        assert!(filter.allows_ip("9.9.9.9"));
        assert!(filter.allows_ip("5.6.7.8"));
    }

    #[test]
    fn ip_filter_whitelist_only() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        filter.add_whitelist("192.168.0.0/24", "internal");
        assert!(filter.allows_ip("192.168.0.1"));
        assert!(!filter.allows_ip("10.0.0.1"));
    }

    #[test]
    fn ip_filter_blacklist_only() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        filter.add_blacklist("10.0.0.0/8", "blocked");
        assert!(!filter.allows_ip("10.1.2.3"));
        assert!(filter.allows_ip("192.168.1.1"));
    }

    #[test]
    fn ip_filter_whitelist_and_blacklist() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        // 白名单：192.168.0.0/16
        filter.add_whitelist("192.168.0.0/16", "internal");
        // 黑名单：192.168.1.0/24
        filter.add_blacklist("192.168.1.0/24", "blocked subnet");

        // 在白名单内且不在黑名单内 → 允许
        assert!(filter.allows_ip("192.168.2.1"));
        // 在白名单内但在黑名单内 → 拒绝
        assert!(!filter.allows_ip("192.168.1.1"));
        // 不在白名单内 → 拒绝
        assert!(!filter.allows_ip("10.0.0.1"));
    }

    #[test]
    fn ip_filter_empty_lists_allow_all() {
        let filter = IpFilter {
            whitelist: vec![],
            blacklist: vec![],
            enabled: true,
        };
        assert!(filter.allows_ip("1.2.3.4"));
        assert!(filter.allows_ip("5.6.7.8"));
    }

    #[test]
    fn ip_filter_remove_whitelist() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        filter.add_whitelist("1.2.3.4", "");
        assert!(filter.remove_whitelist("1.2.3.4"));
        assert!(filter.whitelist.is_empty());
        assert!(!filter.remove_whitelist("1.2.3.4"));
    }

    #[test]
    fn ip_filter_remove_blacklist() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        filter.add_blacklist("5.6.7.8", "");
        assert!(filter.remove_blacklist("5.6.7.8"));
        assert!(filter.blacklist.is_empty());
    }

    #[test]
    fn check_ip_returns_ok_when_disabled() {
        let filter = IpFilter::default();
        assert!(check_ip(&filter, "1.2.3.4").is_ok());
    }

    #[test]
    fn check_ip_returns_blacklisted_error() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        filter.add_blacklist("10.0.0.0/8", "");
        let err = check_ip(&filter, "10.1.2.3").unwrap_err();
        assert!(matches!(err, IpFilterError::Blacklisted(_)));
    }

    #[test]
    fn check_ip_returns_not_whitelisted_error() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        filter.add_whitelist("192.168.0.0/16", "");
        let err = check_ip(&filter, "10.0.0.1").unwrap_err();
        assert!(matches!(err, IpFilterError::NotWhitelisted(_)));
    }

    #[test]
    fn check_ip_allows_valid_ip() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        filter.add_whitelist("192.168.0.0/16", "");
        assert!(check_ip(&filter, "192.168.1.1").is_ok());
    }

    #[test]
    fn ip_rule_serialization() {
        let rule = IpRule {
            pattern: "192.168.0.0/24".to_string(),
            note: "internal network".to_string(),
        };
        let json = serde_json::to_string(&rule).unwrap();
        let decoded: IpRule = serde_json::from_str(&json).unwrap();
        assert_eq!(rule, decoded);
    }

    #[test]
    fn ip_filter_serialization() {
        let mut filter = IpFilter::default();
        filter.enabled = true;
        filter.add_whitelist("192.168.0.0/16", "internal");
        filter.add_blacklist("10.0.0.0/8", "blocked");

        let json = serde_json::to_string(&filter).unwrap();
        let decoded: IpFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, decoded);
    }
}
