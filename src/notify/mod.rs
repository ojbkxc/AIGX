//! 通知系统：Telegram Bot + SMTP 邮件
//!
//! 参照 VFaka 的 aff-notify crate（Telegram + SMTP）实现统一通知服务。
//! - Telegram：通过 reqwest 调用 Bot API（不引入新依赖）
//! - SMTP：原生 TCP 实现（AUTH LOGIN + 明文），不引入 lettre。
//!   适用于本地邮件中继（postfix/exim）或内网 SMTP。
//!   生产环境若需 TLS（端口 465/587），后续可在 Cargo.toml 引入 lettre 补全。
//!
//! 事件类型：充值成功 / 额度不足 / 渠道故障 / 提现请求。

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── 配置 ─────────────────────────────────────────────────────────────

/// 通知配置（可序列化到 TOML，#[serde(default)] 兼容旧配置）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifyConfig {
    /// 总开关
    #[serde(default)]
    pub enabled: bool,
    // Telegram
    #[serde(default)]
    pub telegram_bot_token: String,
    #[serde(default)]
    pub telegram_chat_id: String,
    // SMTP
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default)]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_username: String,
    #[serde(default)]
    pub smtp_password: String,
    #[serde(default)]
    pub smtp_from: String,
}

impl NotifyConfig {
    /// Telegram 是否已配置
    pub fn telegram_ready(&self) -> bool {
        !self.telegram_bot_token.is_empty() && !self.telegram_chat_id.is_empty()
    }

    /// SMTP 是否已配置
    pub fn smtp_ready(&self) -> bool {
        !self.smtp_host.is_empty() && self.smtp_port > 0
    }
}

// ── 事件 ─────────────────────────────────────────────────────────────

/// 通知事件枚举
#[derive(Debug, Clone)]
pub enum NotifyEvent {
    /// 充值成功
    PaymentSuccess {
        user_email: String,
        amount: f64,
        quota: i64,
    },
    /// 额度不足
    QuotaLow { user_email: String, remaining: i64 },
    /// 渠道故障
    ChannelFailure { channel_name: String, error: String },
    /// 提现请求
    #[allow(dead_code)]
    WithdrawRequest { user_email: String, amount: f64 },
}

// ── 服务 ─────────────────────────────────────────────────────────────

/// 通知服务：持有 NotifyConfig（运行时可更新）+ reqwest Client
pub struct NotifyService {
    config: RwLock<NotifyConfig>,
    client: reqwest::Client,
}

impl NotifyService {
    pub fn new(config: NotifyConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config: RwLock::new(config),
            client,
        }
    }

    /// 获取当前配置快照
    pub async fn get_config(&self) -> NotifyConfig {
        self.config.read().await.clone()
    }

    /// 更新配置
    pub async fn update_config(&self, config: NotifyConfig) {
        *self.config.write().await = config;
    }

    // ── Telegram ─────────────────────────────────────────────────────

    /// 发送 Telegram 消息（HTML parse_mode）
    pub async fn send_telegram(&self, message: &str) -> Result<(), String> {
        let cfg = self.config.read().await.clone();
        self.send_telegram_with(&cfg, message).await
    }

    async fn send_telegram_with(&self, cfg: &NotifyConfig, message: &str) -> Result<(), String> {
        if !cfg.telegram_ready() {
            return Err("Telegram bot_token or chat_id not configured".into());
        }
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            cfg.telegram_bot_token
        );
        let payload = serde_json::json!({
            "chat_id": cfg.telegram_chat_id,
            "text": message,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });
        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("Telegram request failed: {}", e))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Telegram API error: {}", body));
        }
        Ok(())
    }

    // ── SMTP ─────────────────────────────────────────────────────────

    /// 发送邮件（原生 TCP SMTP + AUTH LOGIN）
    ///
    /// 注：当前为明文 SMTP（非 TLS），适用于本地邮件中继或内网 SMTP（端口 25）。
    /// 若需 TLS（465/587），后续可在 Cargo.toml 引入 lettre 补全。
    pub async fn send_email(&self, to: &str, subject: &str, body: &str) -> Result<(), String> {
        let cfg = self.config.read().await.clone();
        send_smtp_raw(&cfg, to, subject, body).await
    }

    // ── 统一事件分发 ─────────────────────────────────────────────────

    /// 根据事件类型发送通知（同步等待）。通常用 `notify_spawn` 异步触发。
    pub async fn notify(&self, event: &NotifyEvent) {
        let cfg = self.config.read().await.clone();
        if !cfg.enabled {
            return;
        }

        let (tg_text, email_to, email_subject, email_body) = render_event(event);

        // Telegram
        if let Some(text) = tg_text {
            if cfg.telegram_ready() {
                if let Err(e) = self.send_telegram_with(&cfg, &text).await {
                    warn!("Notify telegram failed: {}", e);
                } else {
                    info!("Notify telegram sent for event");
                }
            }
        }

        // Email
        if let Some(to) = email_to {
            if !to.is_empty() && cfg.smtp_ready() {
                if let Err(e) = send_smtp_raw(&cfg, &to, &email_subject, &email_body).await {
                    warn!("Notify email failed: {}", e);
                } else {
                    info!("Notify email sent to {}", to);
                }
            }
        }
    }

    /// 异步触发通知（spawn，不阻塞调用方）
    pub fn notify_spawn(self: &Arc<Self>, event: NotifyEvent) {
        let svc = self.clone();
        tokio::spawn(async move {
            svc.notify(&event).await;
        });
    }
}

// ── 事件渲染 ─────────────────────────────────────────────────────────

/// 将事件渲染为 (Telegram 文本, 收件邮箱, 邮件主题, 邮件正文)
fn render_event(event: &NotifyEvent) -> (Option<String>, Option<String>, String, String) {
    match event {
        NotifyEvent::PaymentSuccess {
            user_email,
            amount,
            quota,
        } => {
            let tg = format!(
                "<b>✅ 充值成功</b>\n\n用户: {}\n金额: {:.2}\n配额: +{}",
                user_email, amount, quota
            );
            let subject = format!("AIGX 充值成功 - {:.2}", amount);
            let body = format!(
                "您的充值已成功到账。\n\n用户: {}\n金额: {:.2}\n配额: +{}\n\n感谢您的支持。",
                user_email, amount, quota
            );
            (Some(tg), Some(user_email.clone()), subject, body)
        }
        NotifyEvent::QuotaLow {
            user_email,
            remaining,
        } => {
            let tg = format!(
                "<b>⚠️ 额度不足</b>\n\n用户: {}\n剩余配额: {}",
                user_email, remaining
            );
            let subject = "AIGX 额度不足提醒".to_string();
            let body = format!(
                "您的账户额度即将耗尽。\n\n用户: {}\n剩余配额: {}\n\n请及时充值。",
                user_email, remaining
            );
            (Some(tg), Some(user_email.clone()), subject, body)
        }
        NotifyEvent::ChannelFailure {
            channel_name,
            error,
        } => {
            let tg = format!(
                "<b>❌ 渠道故障</b>\n\n渠道: {}\n错误: {}",
                channel_name, error
            );
            (Some(tg), None, String::new(), String::new())
        }
        NotifyEvent::WithdrawRequest { user_email, amount } => {
            let tg = format!(
                "<b>💰 提现请求</b>\n\n用户: {}\n金额: {:.2}",
                user_email, amount
            );
            (Some(tg), None, String::new(), String::new())
        }
    }
}

// ── 原生 SMTP 实现 ───────────────────────────────────────────────────

/// 原生 TCP SMTP 发送（AUTH LOGIN，明文）
///
/// 流程：connect → EHLO → AUTH LOGIN → MAIL FROM → RCPT TO → DATA → QUIT
async fn send_smtp_raw(
    cfg: &NotifyConfig,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    if !cfg.smtp_ready() {
        return Err("SMTP host/port not configured".into());
    }
    if to.is_empty() {
        return Err("Recipient is empty".into());
    }

    let addr = format!("{}:{}", cfg.smtp_host, cfg.smtp_port);
    let mut stream = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("SMTP connect failed: {}", e))?;
    // 设置读写超时（避免挂死）
    let _ = stream.set_nodelay(true);

    // 读取欢迎信息（220）
    let greet = read_smtp_line(&mut stream).await?;
    if !greet.starts_with("220") {
        return Err(format!("SMTP unexpected greeting: {}", greet));
    }

    // EHLO
    write_smtp(&mut stream, "EHLO aigx.local\r\n").await?;
    read_smtp_multiline(&mut stream).await?;

    // AUTH LOGIN
    if !cfg.smtp_username.is_empty() {
        write_smtp(&mut stream, "AUTH LOGIN\r\n").await?;
        let r = read_smtp_line(&mut stream).await?;
        if !r.starts_with("334") {
            return Err(format!("SMTP AUTH LOGIN rejected: {}", r));
        }
        write_smtp(
            &mut stream,
            &format!("{}\r\n", STANDARD.encode(cfg.smtp_username.as_bytes())),
        )
        .await?;
        let r = read_smtp_line(&mut stream).await?;
        if !r.starts_with("334") {
            return Err(format!("SMTP username rejected: {}", r));
        }
        write_smtp(
            &mut stream,
            &format!("{}\r\n", STANDARD.encode(cfg.smtp_password.as_bytes())),
        )
        .await?;
        let r = read_smtp_line(&mut stream).await?;
        if !r.starts_with("235") {
            return Err(format!("SMTP auth failed: {}", r));
        }
    }

    // MAIL FROM
    write_smtp(&mut stream, &format!("MAIL FROM:<{}>\r\n", cfg.smtp_from)).await?;
    let r = read_smtp_line(&mut stream).await?;
    if !r.starts_with("250") {
        return Err(format!("SMTP MAIL FROM rejected: {}", r));
    }

    // RCPT TO
    write_smtp(&mut stream, &format!("RCPT TO:<{}>\r\n", to)).await?;
    let r = read_smtp_line(&mut stream).await?;
    if !r.starts_with("250") {
        return Err(format!("SMTP RCPT TO rejected: {}", r));
    }

    // DATA
    write_smtp(&mut stream, "DATA\r\n").await?;
    let r = read_smtp_line(&mut stream).await?;
    if !r.starts_with("354") {
        return Err(format!("SMTP DATA rejected: {}", r));
    }

    // 邮件内容（点号结束）
    // B12：SMTP dot-stuffing——正文行以 '.' 开头时需再加一个 '.' 前缀，
    // 否则会被服务器误认为 DATA 结束标记导致邮件被截断（RFC 5321 §4.5.2）
    let stuffed_body = body
        .lines()
        .map(|line| {
            if line.starts_with('.') {
                format!(".{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n");
    let msg = format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{}\r\n.\r\n",
        cfg.smtp_from, to, subject, stuffed_body
    );
    write_smtp(&mut stream, &msg).await?;
    let r = read_smtp_line(&mut stream).await?;
    if !r.starts_with("250") {
        return Err(format!("SMTP data send failed: {}", r));
    }

    // QUIT
    write_smtp(&mut stream, "QUIT\r\n").await?;
    Ok(())
}

async fn write_smtp(stream: &mut TcpStream, data: &str) -> Result<(), String> {
    stream
        .write_all(data.as_bytes())
        .await
        .map_err(|e| format!("SMTP write failed: {}", e))
}

async fn read_smtp_line(stream: &mut TcpStream) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream
            .read(&mut byte)
            .await
            .map_err(|e| format!("SMTP read failed: {}", e))?;
        if n == 0 {
            return Err("SMTP connection closed".into());
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n") {
            break;
        }
        if buf.len() > 8192 {
            return Err("SMTP line too long".into());
        }
    }
    String::from_utf8(buf)
        .map(|s| s.trim_end_matches(['\r', '\n']).to_string())
        .map_err(|e| format!("SMTP non-utf8: {}", e))
}

/// 读取多行 SMTP 响应（直到最后一行以 "code " 而非 "code-" 结束）
async fn read_smtp_multiline(stream: &mut TcpStream) -> Result<String, String> {
    let mut last: String;
    loop {
        let line = read_smtp_line(stream).await?;
        let is_last = line.len() >= 4 && line.as_bytes()[3] == b' ';
        last = line;
        if is_last {
            break;
        }
    }
    Ok(last)
}

// ── 测试 ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_payment_success() {
        let ev = NotifyEvent::PaymentSuccess {
            user_email: "u@x.com".into(),
            amount: 9.9,
            quota: 1000,
        };
        let (tg, to, subj, _body) = render_event(&ev);
        assert!(tg.unwrap().contains("充值成功"));
        assert_eq!(to.unwrap(), "u@x.com");
        assert!(subj.contains("9.90"));
    }

    #[test]
    fn render_channel_failure_no_email() {
        let ev = NotifyEvent::ChannelFailure {
            channel_name: "cf".into(),
            error: "boom".into(),
        };
        let (tg, to, _, _) = render_event(&ev);
        assert!(tg.unwrap().contains("渠道故障"));
        assert!(to.is_none());
    }

    #[test]
    fn notify_config_default() {
        let c = NotifyConfig::default();
        assert!(!c.enabled);
        assert!(!c.telegram_ready());
        assert!(!c.smtp_ready());
    }
}
