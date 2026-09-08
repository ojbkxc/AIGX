//! TOTP 两步验证（RFC 6238）——自研实现，不引第三方 crate。
//!
//! 为什么手写 SHA-1：项目现有 crypto 依赖（sha2 0.10 只含 SHA-2 家族、
//! hmac 0.12 泛型于 digest trait）均不含 SHA-1；引入 `sha1` crate 会
//! 违反百年工程红线「不引新依赖」。RFC 3174 SHA-1 约 60 行纯函数
//! 无 unsafe，用官方测试向量锁死正确性——自研是红线下的唯一合规方案。
//!
//! 安全性说明：SHA-1 的碰撞攻击不影响 TOTP 安全性（TOTP 依赖的是
//! HMAC 的 PRF 性质，SHA-1 在该用途下目前无实际攻击），RFC 6238
//! 标准本身也以 HMAC-SHA-1 为默认算法。

use std::time::{SystemTime, UNIX_EPOCH};

// ── SHA-1（RFC 3174）────────────────────────────────────────────────

/// SHA-1 哈希（RFC 3174 标准 80 轮实现）。
///
/// 仅用于 TOTP 的 HMAC 构造，不做任何安全敏感的碰撞场景用途。
/// 测试向量：RFC 3174 第 7.3 节 + NIST 标准向量（见本文件 tests）。
pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

    // 填充：0x80 + 0x00* + 8 字节大端 bit 长度
    let ml = (msg.len() as u64) * 8;
    let mut data = msg.to_vec();
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&ml.to_be_bytes());

    // 分块处理（每块 64 字节 = 16 个大端 u32）
    for block in data.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..80 {
            // SHA-1 消息扩展：w[i] = 循环左移 1 位的 (w[i-3]^w[i-8]^w[i-14]^w[i-16])
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

// ── HMAC-SHA1（RFC 2104）────────────────────────────────────────────

/// HMAC-SHA1：key 长于块（64 字节）时先哈希；短于则零填充。
pub fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let key = if key.len() > 64 {
        let k = sha1(key);
        let mut padded = k.to_vec();
        padded.resize(64, 0);
        padded
    } else {
        let mut padded = key.to_vec();
        padded.resize(64, 0);
        padded
    };

    let mut inner = Vec::with_capacity(64 + msg.len());
    for b in key.iter() {
        inner.push(b ^ 0x36);
    }
    inner.extend_from_slice(msg);
    let inner_hash = sha1(&inner);

    let mut outer = Vec::with_capacity(64 + 20);
    for b in key.iter() {
        outer.push(b ^ 0x5C);
    }
    outer.extend_from_slice(&inner_hash);
    sha1(&outer)
}

// ── TOTP（RFC 6238）─────────────────────────────────────────────────

/// 时间步长（秒）——RFC 6238 推荐 30s。
pub const TOTP_PERIOD: u64 = 30;

/// 在指定计数器（时间步）生成 TOTP 码。
///
/// dynamic truncation（RFC 4226 第 5.3 节）：取 HMAC 末 4 位低 7 bit
/// 作偏移，取 4 字节大端并 mod 10^digits。
pub fn totp_at(secret: &[u8], counter: u64, digits: usize) -> String {
    let mac = hmac_sha1(secret, &counter.to_be_bytes());
    let offset = (mac[19] & 0x0F) as usize;
    let code = ((mac[offset] as u32 & 0x7F) << 24)
        | ((mac[offset + 1] as u32) << 16)
        | ((mac[offset + 2] as u32) << 8)
        | (mac[offset + 3] as u32);
    let modulus = 10u32.pow(digits as u32);
    format!("{:0width$}", code % modulus, width = digits)
}

/// 当前 Unix 时间戳（秒）。
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 验证 TOTP 码——允许 ±window 个时间步的时钟偏移（常量时间比较）。
///
/// 返回 true 当且仅当 code 与 [now-window, now+window] 内任一步匹配。
pub fn verify(secret: &[u8], code: &str, window: u64) -> bool {
    let now_step = unix_now() / TOTP_PERIOD;
    // 常量时间比较：逐字节异或累计，避免短路泄长度/前缀信息
    let eq_const = |a: &str, b: &str| -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        let mut diff = (a.len() ^ b.len()) as u8;
        for i in 0..a.len().max(b.len()) {
            let x = a.get(i).copied().unwrap_or(0);
            let y = b.get(i).copied().unwrap_or(0);
            diff |= x ^ y;
        }
        diff == 0
    };
    for delta in 0..=window {
        for step in [now_step + delta, now_step - delta.min(now_step)] {
            if eq_const(&totp_at(secret, step, 6), code) {
                return true;
            }
        }
    }
    false
}

// ── Base32（RFC 4648，无填充）——Google Authenticator 的 secret 编码 ──

const B32_ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Base32 编码（无填充，otpauth secret 惯例）。
pub fn base32_encode(data: &[u8]) -> String {
    // 位流补零规则（RFC 4648）：bit 流直接补 0 到 5 的倍数，每组取 5 bit。
    // n 字符 = ceil(len*8 / 5)；n%8 ∈ {0,2,4,5,7} 对应无填充变体的合法尾部。
    let n = (data.len() * 8).div_ceil(5);
    let mut out = String::with_capacity(n);
    let mut bit_pos = 0usize;
    for _ in 0..n {
        // 取从 bit_pos 起 5 bit（不足补 0）
        let mut val = 0u8;
        for b in 0..5 {
            let idx = bit_pos + b;
            let bit = if idx < data.len() * 8 {
                (data[idx / 8] >> (7 - idx % 8)) & 1
            } else {
                0
            };
            val = (val << 1) | bit;
        }
        out.push(B32_ALPHABET[val as usize] as char);
        bit_pos += 5;
    }
    out
}

/// Base32 解码（忽略大小写，容忍无填充输入）。
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=').to_ascii_uppercase();
    let mut bits: u64 = 0;
    let mut nbits = 0u32;
    let mut out = Vec::new();
    for ch in s.bytes() {
        let v = B32_ALPHABET.iter().position(|&c| c == ch)? as u64;
        bits = (bits << 5) | v;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_nist_vectors() {
        // NIST FIPS 180-1 / RFC 3174 §7.3 官方向量
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        // 空串
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        // >64 字节（跨块）
        assert_eq!(
            hex(&sha1(&vec![0x61u8; 1000])),
            "291e9a6c66994949b57ba5e650361e98fc36b1ba"
        );
    }

    #[test]
    fn hmac_sha1_rfc2202_vectors() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        // RFC 2202 test case 1：key=20 字节 0x0b
        assert_eq!(
            hex(&hmac_sha1(&[0x0b; 20], b"Hi There")),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
        // RFC 2202 test case 2：key="Jefe"
        assert_eq!(
            hex(&hmac_sha1(b"Jefe", b"what do ya want for nothing?")),
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
        );
        // RFC 2202 test case 6：key 超 64 字节（先哈希 key 的路径）
        assert_eq!(
            hex(&hmac_sha1(
                &[0xaa; 80],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }

    #[test]
    fn totp_rfc6238_appendix_b() {
        // RFC 6238 附录 B 官方向量：secret = "12345678901234567890"（ASCII）
        let secret = b"12345678901234567890";
        // (T, TOTP SHA1 8 位)
        let cases = [
            (59u64, "94287082"),
            (1111111109, "07081804"),
            (1111111111, "14050471"),
            (1234567890, "89005924"),
            (2000000000, "69279037"),
            (20000000000, "65353130"),
        ];
        for (t, expected) in cases {
            let counter = t / 30;
            assert_eq!(totp_at(secret, counter, 8), expected, "T={t}");
        }
    }

    #[test]
    fn totp_verify_current_window() {
        // 用当前时间步生成再验证，必须命中（window=0 即精确步）
        let secret = b"any-secret-for-test";
        let now_step = unix_now() / TOTP_PERIOD;
        let code = totp_at(secret, now_step, 6);
        assert!(verify(secret, &code, 0));
        // 错误码必须失败
        let bad = format!(
            "{:06}",
            code.parse::<u32>().unwrap().wrapping_add(1) % 1_000_000
        );
        assert!(!verify(secret, &bad, 0));
        // 相邻步在 window=1 下应通过（模拟时钟偏移 30s）
        let next_code = totp_at(secret, now_step + 1, 6);
        assert!(verify(secret, &next_code, 1));
    }

    #[test]
    fn base32_roundtrip() {
        for data in [
            b"".as_slice(),
            b"f".as_slice(),
            b"fo".as_slice(),
            b"foo".as_slice(),
            b"foob".as_slice(),
            b"fooba".as_slice(),
            b"foobar".as_slice(),
        ] {
            let enc = base32_encode(data);
            assert_eq!(base32_decode(&enc).unwrap(), data, "roundtrip {enc}");
        }
        // RFC 4648 test vectors（无填充变体）
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_encode(b"foob"), "MZXW6YQ");
        assert_eq!(base32_encode(b"fooba"), "MZXW6YTB");
        // 小写容忍
        assert_eq!(base32_decode("mzxw6ytboi").unwrap(), b"foobar");
    }
}
