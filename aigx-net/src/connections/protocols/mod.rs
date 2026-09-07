//! 协议实现模块
//!
//! 具体的协议传输层实现：TCP / KCP / WebSocket / QUIC

pub mod kcp;
pub mod quic;
pub mod tcp;
pub mod websocket;

use super::Protocol;

/// 协议处理器接口
///
/// 各传输协议（TCP/KCP/WebSocket/QUIC）的帧编解码钩子。
pub trait ProtocolHandler: Send + Sync {
    /// 处理一帧数据：入参为原始字节，返回协议封装/解封后的字节。
    fn handle(&self, data: &[u8]) -> Vec<u8>;
}

/// 获取协议处理器
pub fn create_handler(protocol: Protocol) -> Option<Box<dyn ProtocolHandler>> {
    match protocol {
        Protocol::Tcp => Some(Box::new(tcp::TcpHandler)),
        Protocol::Kcp => Some(Box::new(kcp::KcpHandler)),
        Protocol::WebSocket => Some(Box::new(websocket::WebSocketHandler)),
        Protocol::Quic => Some(Box::new(quic::QuicHandler)),
        _ => None,
    }
}
