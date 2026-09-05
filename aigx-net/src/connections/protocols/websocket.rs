//! WebSocket 协议实现

use super::super::ProtocolHandler;

/// WebSocket 处理器
pub struct WebSocketHandler;

impl ProtocolHandler for WebSocketHandler {
    fn handle(&self, data: &[u8]) -> Vec<u8> {
        // WebSocket 帧处理
        vec![
            0x81, // FIN=1, Opcode=1 (text frame)
            data.len() as u8,
        ]
    }
}
