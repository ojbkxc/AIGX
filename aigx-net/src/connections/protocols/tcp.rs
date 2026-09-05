//! TCP 协议实现

use super::super::ProtocolHandler;

/// TCP处理器实现
pub struct TcpHandler;

impl ProtocolHandler for TcpHandler {
    fn handle(&self, data: &[u8]) -> Vec<u8> {
        // TCP 原样传输
        data.to_vec()
    }
}