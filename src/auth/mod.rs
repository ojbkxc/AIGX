//! 认证扩展模块：2FA/TOTP。
//!
//! 登录流程的二次验证（something you have）：
//! - `totp.rs`：自研 RFC 6238 TOTP（含 RFC 3174 SHA-1，不引 crate）
//! - 登录 handler 在 `api/admin/auth.rs`：密码验证成功且用户开启 2FA 时
//!   返回 require_2fa 分支，客户端携 TOTP 码二次提交
pub mod totp;
