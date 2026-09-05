//! QUIC 协议实现（简化版本）

// 在实际项目中，这里应该使用 quinn 库来实现
// 目前提供接口定义用于未来的实现

use super::super::ProtocolHandler;

/// QUIC 处理器
pub struct QuicHandler;

impl ProtocolHandler for QuicHandler {
    fn handle(&self, data: &[u8]) -> Vec<u8> {
        // QUIC 分包和加密
        data.to_vec()
    }
}
