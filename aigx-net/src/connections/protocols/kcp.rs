//! KCP 协议实现（简化版本）

// 在实际项目中，这里应该使用 `cargo install kcp-rs` 等库来实现
// 目前提供接口定义用于未来的实现

use super::super::ProtocolHandler;

/// KCP 处理器
pub struct KcpHandler;

impl ProtocolHandler for KcpHandler {
    fn handle(&self, data: &[u8]) -> Vec<u8> {
        // KCP 加密/重新打包
        data.to_vec()
    }
}
