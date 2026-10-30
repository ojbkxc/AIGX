//! Qoder COSY 请求签名（自实现，不引第三方 AES / 大整数依赖）。
//!
//! 协议抄自 Agent2API `providers/qoder/cosy.rs`：
//! 1. 身份 JSON（`uid` / `security_oauth_token` / `name` / `aid` / `email`）
//!    用一次性 AES-128-CBC 密钥加密成 `info`；
//! 2. 该 AES 密钥用 Qoder 内置 RSA-1024 公钥（PKCS#1 v1.5）加密成 `key`；
//! 3. `{version,requestId,info,cosyVersion,ideVersion}` base64 → `payload`；
//! 4. `MD5(payload \n key \n 时间戳 \n 请求体 \n 签名路径)` → 签名；
//! 5. `Authorization: Bearer COSY.<payload>.<签名>` + 一组 `Cosy-*` 头。

use base64::Engine as _;

const COSY_VERSION: &str = "1.1.38";
const CLIENT_TYPE: &str = "5";
const DATA_POLICY: &str = "disagree";
const LOGIN_VERSION: &str = "v2";
const MACHINE_TYPE: &str = "5";
const CLIENT_IP: &str = "127.0.0.1";

/// Qoder 客户端内置 RSA 公钥模数（1024 位，hex；指数固定 65537）。
const RSA_MODULUS_HEX: &str = "c0f22307e5cd362e296bb04470f6de8fbf935ce24e8fcf511a0e2701329769c4a76e499bb938036a52af1eaf818cf79a2600620e3ce87e371d2ca6d85803606a1b3fa5e874643c9ed2db7e85673ef7227fca56e2e7c08f0927609bb896a9f24be1782099a66016a5bfdc3f1ff756bfc9e88d7b5dc5be30bf45a0223a00ebcecf";

/// 自定义字母表（与标准 base64 字母表逐字符对应）。
const CUSTOM_ALPHABET: &[u8] = b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";
const STD_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub struct CosyIdentity<'a> {
    pub user_id: &'a str,
    pub auth_token: &'a str,
    pub name: &'a str,
    pub email: &'a str,
    pub machine_id: &'a str,
}

/// 构造一次请求的全部 COSY 头。
pub fn build_auth_headers(
    body: Option<&[u8]>,
    request_url: &str,
    identity: &CosyIdentity<'_>,
) -> Result<Vec<(String, String)>, String> {
    if identity.user_id.is_empty() || identity.auth_token.is_empty() {
        return Err("Qoder 账号缺少用户标识或访问令牌".to_string());
    }
    let aes_key = random_hex(16);
    let user_info = format!(
        "{{\"uid\":{},\"security_oauth_token\":{},\"name\":{},\"aid\":\"\",\"email\":{}}}",
        json_text(identity.user_id),
        json_text(identity.auth_token),
        json_text(identity.name),
        json_text(identity.email),
    );
    let info = aes128_cbc_base64(&aes_key, user_info.as_bytes())?;
    let cosy_key = rsa_encrypt_base64(&aes_key)?;

    let request_id = uuid::Uuid::new_v4().to_string();
    let timestamp = (chrono::Utc::now().timestamp_millis() / 1000).to_string();

    let payload_src = format!(
        "{{\"version\":\"v1\",\"requestId\":\"{request_id}\",\"info\":\"{info}\",\
         \"cosyVersion\":\"{COSY_VERSION}\",\"ideVersion\":\"\"}}"
    );
    let payload = base64::engine::general_purpose::STANDARD.encode(payload_src.as_bytes());

    let sig_path = signature_path(request_url)?;
    let body_bytes = body.unwrap_or(&[]);

    let mut signing = Vec::new();
    signing.extend_from_slice(payload.as_bytes());
    signing.push(b'\n');
    signing.extend_from_slice(cosy_key.as_bytes());
    signing.push(b'\n');
    signing.extend_from_slice(timestamp.as_bytes());
    signing.push(b'\n');
    signing.extend_from_slice(body_bytes);
    signing.push(b'\n');
    signing.extend_from_slice(sig_path.as_bytes());
    let signature = format!("{:x}", md5::compute(&signing));

    let body_hash = format!("{:x}", md5::compute(body_bytes));

    Ok(vec![
        (
            "Authorization".to_string(),
            format!("Bearer COSY.{payload}.{signature}"),
        ),
        ("Cosy-Key".to_string(), cosy_key),
        ("Cosy-User".to_string(), identity.user_id.to_string()),
        ("Cosy-Date".to_string(), timestamp),
        ("Cosy-Version".to_string(), COSY_VERSION.to_string()),
        (
            "Cosy-Machineid".to_string(),
            identity.machine_id.to_string(),
        ),
        (
            "Cosy-Machinetoken".to_string(),
            identity.machine_id.to_string(),
        ),
        ("Cosy-Machinetype".to_string(), MACHINE_TYPE.to_string()),
        ("Cosy-Machineos".to_string(), machine_os()),
        ("Cosy-Clienttype".to_string(), CLIENT_TYPE.to_string()),
        ("Cosy-Clientip".to_string(), CLIENT_IP.to_string()),
        ("Cosy-Bodyhash".to_string(), body_hash),
        ("Cosy-Bodylength".to_string(), body_bytes.len().to_string()),
        ("Cosy-Sigpath".to_string(), sig_path),
        ("Cosy-Data-Policy".to_string(), DATA_POLICY.to_string()),
        ("Cosy-Organization-Id".to_string(), String::new()),
        ("Cosy-Organization-Tags".to_string(), String::new()),
        ("Login-Version".to_string(), LOGIN_VERSION.to_string()),
        ("X-Request-Id".to_string(), request_id),
    ])
}

/// 请求体编码（上游 `Encode=1` 时不是裸 JSON）。
pub fn encode_body(payload: &[u8]) -> Vec<u8> {
    let standard = base64::engine::general_purpose::STANDARD.encode(payload);
    let bytes = standard.as_bytes();
    let length = bytes.len();
    let third = length / 3;

    let mut table = [0u8; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        *slot = index as u8;
    }
    for (index, ch) in STD_ALPHABET.iter().enumerate() {
        table[*ch as usize] = CUSTOM_ALPHABET[index];
    }
    table[b'=' as usize] = b'$';

    let mut out = Vec::with_capacity(length);
    for index in (length - third)..length {
        out.push(table[bytes[index] as usize]);
    }
    for index in third..(length - third) {
        out.push(table[bytes[index] as usize]);
    }
    for index in 0..third {
        out.push(table[bytes[index] as usize]);
    }
    out
}

/// 签名路径：去掉 `/algo` 前缀、不含查询串。
pub fn signature_path(request_url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(request_url).map_err(|_| "Qoder 请求地址无效".to_string())?;
    let path = parsed.path();
    Ok(path
        .strip_prefix("/algo")
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string()))
}

fn json_text(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn machine_os() -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let platform = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    format!("{arch}_{platform}")
}

fn random_hex(len: usize) -> String {
    use rand::RngCore;
    let mut bytes = vec![0u8; len.div_ceil(2)];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes).chars().take(len).collect()
}

// ── AES-128-CBC（自实现）────────────────────────────────────────────

/// GF(2^8) 乘 2（AES 的 `xtime`）。
fn xtime(value: u8) -> u8 {
    (value << 1) ^ if value & 0x80 != 0 { 0x1b } else { 0 }
}

fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut out = 0u8;
    while b != 0 {
        if b & 1 != 0 {
            out ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    out
}

/// 求 GF(2^8) 乘法逆元（a^254，0 映射 0）。
fn gf_inv(a: u8) -> u8 {
    let mut result = 1u8;
    let mut base = a;
    let mut exp = 254u8;
    while exp > 0 {
        if exp & 1 == 1 {
            result = gf_mul(result, base);
        }
        base = gf_mul(base, base);
        exp >>= 1;
    }
    if a == 0 {
        0
    } else {
        result
    }
}

fn rotl(value: u8, bits: u32) -> u8 {
    value.rotate_left(bits)
}

/// 按算法生成 AES S-box（不依赖手抄常量表）。
fn sbox() -> [u8; 256] {
    let mut table = [0u8; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        let inverse = gf_inv(index as u8);
        *slot = inverse
            ^ rotl(inverse, 1)
            ^ rotl(inverse, 2)
            ^ rotl(inverse, 3)
            ^ rotl(inverse, 4)
            ^ 0x63;
    }
    table
}

/// AES-128 密钥扩展（11 轮 × 16 字节）。
fn expand_key(key: &[u8; 16]) -> [[u8; 16]; 11] {
    const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];
    let boxed = sbox();
    let mut words = [[0u8; 4]; 44];
    for index in 0..4 {
        words[index] = [
            key[4 * index],
            key[4 * index + 1],
            key[4 * index + 2],
            key[4 * index + 3],
        ];
    }
    for index in 4..44 {
        let mut temp = words[index - 1];
        if index % 4 == 0 {
            temp = [
                boxed[temp[1] as usize] ^ RCON[index / 4 - 1],
                boxed[temp[2] as usize],
                boxed[temp[3] as usize],
                boxed[temp[0] as usize],
            ];
        }
        for byte in 0..4 {
            words[index][byte] = words[index - 4][byte] ^ temp[byte];
        }
    }
    let mut keys = [[0u8; 16]; 11];
    for round in 0..11 {
        for word in 0..4 {
            let source = words[round * 4 + word];
            for byte in 0..4 {
                keys[round][word * 4 + byte] = source[byte];
            }
        }
    }
    keys
}

fn add_round_key(state: &mut [u8; 16], key: &[u8; 16]) {
    for index in 0..16 {
        state[index] ^= key[index];
    }
}

fn sub_bytes(state: &mut [u8; 16], table: &[u8; 256]) {
    for byte in state.iter_mut() {
        *byte = table[*byte as usize];
    }
}

/// 列是 state 的连续 4 字节（AES 的列主序）。
fn shift_rows(state: &mut [u8; 16]) {
    let old = *state;
    for row in 0..4 {
        for column in 0..4 {
            state[row + 4 * column] = old[row + 4 * ((column + row) % 4)];
        }
    }
}

fn mix_columns(state: &mut [u8; 16]) {
    for column in 0..4 {
        let base = 4 * column;
        let a = [
            state[base],
            state[base + 1],
            state[base + 2],
            state[base + 3],
        ];
        state[base] = xtime(a[0]) ^ (xtime(a[1]) ^ a[1]) ^ a[2] ^ a[3];
        state[base + 1] = a[0] ^ xtime(a[1]) ^ (xtime(a[2]) ^ a[2]) ^ a[3];
        state[base + 2] = a[0] ^ a[1] ^ xtime(a[2]) ^ (xtime(a[3]) ^ a[3]);
        state[base + 3] = (xtime(a[0]) ^ a[0]) ^ a[1] ^ a[2] ^ xtime(a[3]);
    }
}

fn aes_encrypt_block(block: &mut [u8; 16], keys: &[[u8; 16]; 11], table: &[u8; 256]) {
    add_round_key(block, &keys[0]);
    for key in keys.iter().take(10).skip(1) {
        sub_bytes(block, table);
        shift_rows(block);
        mix_columns(block);
        add_round_key(block, key);
    }
    sub_bytes(block, table);
    shift_rows(block);
    add_round_key(block, &keys[10]);
}

fn aes128_cbc_base64(key16: &str, plaintext: &[u8]) -> Result<String, String> {
    if key16.len() != 16 {
        return Err("Qoder 签名密钥长度无效".to_string());
    }
    let mut key = [0u8; 16];
    key.copy_from_slice(key16.as_bytes());
    let keys = expand_key(&key);
    let table = sbox();

    let mut data = plaintext.to_vec();
    let pad = 16 - (data.len() % 16);
    data.extend(std::iter::repeat_n(pad as u8, pad));

    let mut previous = key;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut block = [0u8; 16];
        for index in 0..16 {
            block[index] = chunk[index] ^ previous[index];
        }
        aes_encrypt_block(&mut block, &keys, &table);
        out.extend_from_slice(&block);
        previous = block;
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(&out))
}

// ── RSA-1024 PKCS#1 v1.5 加密（自实现模幂）───────────────────────────

/// 大整数：小端 u32 limb。
fn big_from_hex(text: &str) -> Vec<u32> {
    let mut limbs = vec![0u32];
    for ch in text.chars() {
        let digit = ch.to_digit(16).unwrap_or(0) as u64;
        let mut carry = digit;
        for limb in limbs.iter_mut() {
            let value = (*limb as u64) * 16 + carry;
            *limb = (value & 0xffff_ffff) as u32;
            carry = value >> 32;
        }
        if carry != 0 {
            limbs.push(carry as u32);
        }
    }
    limbs
}

fn big_mul(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = vec![0u32; a.len() + b.len() + 1];
    for (i, &left) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &right) in b.iter().enumerate() {
            let value = out[i + j] as u64 + (left as u64) * (right as u64) + carry;
            out[i + j] = (value & 0xffff_ffff) as u32;
            carry = value >> 32;
        }
        let mut index = i + b.len();
        while carry != 0 {
            let value = out[index] as u64 + carry;
            out[index] = (value & 0xffff_ffff) as u32;
            carry = value >> 32;
            index += 1;
        }
    }
    out
}

fn big_bits(a: &[u32]) -> usize {
    for index in (0..a.len()).rev() {
        if a[index] != 0 {
            return index * 32 + (32 - a[index].leading_zeros() as usize);
        }
    }
    0
}

fn big_sub(a: &mut [u32], b: &[u32]) {
    let mut borrow = 0i64;
    for index in 0..a.len() {
        let right = if index < b.len() { b[index] as i64 } else { 0 };
        let mut value = a[index] as i64 - right - borrow;
        if value < 0 {
            value += 1i64 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        a[index] = value as u32;
    }
}

fn big_cmp(a: &[u32], b: &[u32]) -> std::cmp::Ordering {
    for index in (0..a.len().max(b.len())).rev() {
        let left = a.get(index).copied().unwrap_or(0);
        let right = b.get(index).copied().unwrap_or(0);
        if left != right {
            return left.cmp(&right);
        }
    }
    std::cmp::Ordering::Equal
}

/// `a mod m`：逐位移位取模（正确性优先，1024 位规模足够快）。
fn big_mod(a: &[u32], m: &[u32]) -> Vec<u32> {
    let bits = big_bits(a);
    let mut remainder = vec![0u32; m.len() + 1];
    for bit in (0..bits).rev() {
        // remainder <<= 1
        let mut carry = 0u32;
        for limb in remainder.iter_mut() {
            let next = *limb >> 31;
            *limb = (*limb << 1) | carry;
            carry = next;
        }
        // 取 a 的第 bit 位
        let word = bit / 32;
        let offset = bit % 32;
        let value = if word < a.len() {
            (a[word] >> offset) & 1
        } else {
            0
        };
        remainder[0] |= value;

        let mut candidate = remainder.clone();
        if big_cmp(&candidate, m) != std::cmp::Ordering::Less {
            big_sub(&mut candidate, m);
            remainder = candidate;
        }
    }
    remainder.truncate(m.len());
    remainder
}

fn big_modpow(base: &[u32], exponent: u32, modulus: &[u32]) -> Vec<u32> {
    let mut result = vec![0u32; modulus.len()];
    result[0] = 1;
    let mut value = big_mod(base, modulus);
    let mut exp = exponent;
    while exp > 0 {
        if exp & 1 == 1 {
            result = big_mod(&big_mul(&result, &value), modulus);
        }
        value = big_mod(&big_mul(&value, &value), modulus);
        exp >>= 1;
    }
    result
}

fn rsa_encrypt_base64(plaintext: &str) -> Result<String, String> {
    use rand::RngCore;
    let modulus = big_from_hex(RSA_MODULUS_HEX);
    let key_len = big_bits(&modulus).div_ceil(8);
    let message = plaintext.as_bytes();
    if message.len() + 11 > key_len {
        return Err("Qoder 签名内容过长".to_string());
    }
    let ps_len = key_len - 3 - message.len();

    let mut ps = vec![0u8; ps_len];
    rand::thread_rng().fill_bytes(&mut ps);
    for byte in ps.iter_mut() {
        if *byte == 0 {
            *byte = 1;
        }
    }

    let mut encoded = Vec::with_capacity(key_len);
    encoded.push(0x00);
    encoded.push(0x02);
    encoded.extend_from_slice(&ps);
    encoded.push(0x00);
    encoded.extend_from_slice(message);

    // 大端字节 → 小端 limb
    let mut limbs = vec![0u32; encoded.len().div_ceil(4)];
    for (index, chunk) in encoded.chunks(4).enumerate() {
        let mut word = 0u32;
        for (offset, byte) in chunk.iter().enumerate() {
            word |= (*byte as u32) << (8 * (3 - offset));
        }
        limbs[index] = word;
    }
    // 转成真正的小端顺序（上面的 chunks 是从高位开始的）
    limbs.reverse();

    let cipher = big_modpow(&limbs, 65537, &modulus);
    let mut out = vec![0u8; key_len];
    for index in 0..key_len {
        let word = cipher.get(index / 4).copied().unwrap_or(0);
        let byte = (word >> (8 * (index % 4))) as u8;
        out[key_len - 1 - index] = byte;
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sbox_matches_known_values() {
        let table = sbox();
        assert_eq!(table[0x00], 0x63);
        assert_eq!(table[0x01], 0x7c);
        assert_eq!(table[0x53], 0xed);
        assert_eq!(table[0xff], 0x16);
    }

    #[test]
    fn aes_encrypts_known_block() {
        // FIPS-197 附录 B 的第一组中间值（key=000102…0f，明文=00112233…ff）
        let key_hex = "000102030405060708090a0b0c0d0e0f";
        let mut key = [0u8; 16];
        for index in 0..16 {
            key[index] = u8::from_str_radix(&key_hex[index * 2..index * 2 + 2], 16).unwrap();
        }
        let keys = expand_key(&key);
        let table = sbox();
        let mut block = [0u8; 16];
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (index * 0x11) as u8;
        }
        aes_encrypt_block(&mut block, &keys, &table);
        assert_eq!(
            block,
            [
                0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
                0xc5, 0x5a
            ]
        );
    }

    #[test]
    fn encode_body_reorders_segments() {
        let encoded = encode_body(b"{\"a\":1}");
        assert!(!encoded.contains(&b'='));
        assert!(encoded.iter().all(|byte| *byte < 128));
    }

    #[test]
    fn signature_path_drops_algo_prefix() {
        assert_eq!(
            signature_path("https://gateway.qoder.com.cn/algo/api/v2/model/list?Encode=1").unwrap(),
            "/api/v2/model/list"
        );
    }

    #[test]
    fn rsa_output_is_key_length() {
        let encrypted = rsa_encrypt_base64("0123456789abcdef").unwrap();
        let raw = base64::engine::general_purpose::STANDARD
            .decode(encrypted)
            .unwrap();
        // 密文定长（模数 1024 位 → 128 字节）；首字节随随机 PS 变化，不固定
        assert_eq!(raw.len(), 128);
    }
}
