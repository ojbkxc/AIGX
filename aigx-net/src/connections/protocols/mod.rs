//! 协议实现模块
//!
//! 具体的协议传输层实现：TCP / KCP / WebSocket / QUIC

pub mod kcp;
pub mod quic;
pub mod tcp;
pub mod websocket;

use super::Protocol;
use crate::connections::connection_pool::ProtocolHandler;

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

