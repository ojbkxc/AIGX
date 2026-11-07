//! DeepSeek 网页版对话的 PoW 挑战求解（DeepSeekHashV1）。
//!
//! 上游 `POST /api/v0/chat/create_pow_challenge` 会按 target_path 签发挑战
//! （algorithm=DeepSeekHashV1 / challenge（64 hex）/ salt / expire_at /
//! difficulty / signature / target_path）；对话请求必须带
//! `x-ds-pow-response` 头，值为 `base64(JSON{algorithm,challenge,salt,
//! answer,signature,target_path})`，`answer` 是找出的 nonce。
//!
//! DeepSeekHashV1 = SHA3-256 但**跳过 Keccak-f[1600] 的第 0 轮**（只做
//! 1..24 轮），padding 用标准 SHA3（0x06 起始）。算法与协议移植自 ds2api
//! （`pow/deepseek_hash.go` / `pow/deepseek_pow.go`，MIT），测试向量取自
//! 官方 WASM 的直接调用结果。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

/// SHA3-256 的速率（1600 - 256*2 = 136 字节）。
const RATE: usize = 136;

/// Keccak-f 的轮常量（第 0 轮的 0x0000…1 被跳过，所以从索引 1 起）。
const RC: [u64; 24] = [
    0x0000000000000001,
    0x0000000000008082,
    0x800000000000808A,
    0x8000000080008000,
    0x000000000000808B,
    0x0000000080000001,
    0x8000000080008081,
    0x8000000000008009,
    0x000000000000008A,
    0x0000000000000088,
    0x0000000080008009,
    0x000000008000000A,
    0x000000008000808B,
    0x800000000000008B,
    0x8000000000008089,
    0x8000000000008003,
    0x8000000000008002,
    0x8000000000000080,
    0x000000000000800A,
    0x800000008000000A,
    0x8000000080008081,
    0x8000000000008080,
    0x0000000080000001,
    0x8000000080008008,
];

fn rotl64(v: u64, k: u32) -> u64 {
    v.rotate_left(k)
}

/// Keccak-f[1600] 缺第 0 轮的置换（DeepSeekHashV1 的核心差异）。
fn keccak_f23(state: &mut [u64; 25]) {
    let mut a = *state;
    for round in 1..24 {
        // θ
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20];
        }
        let mut d = [0u64; 5];
        for x in 0..5 {
            d[x] = c[(x + 4) % 5] ^ rotl64(c[(x + 1) % 5], 1);
        }
        for y in 0..5 {
            for x in 0..5 {
                a[y * 5 + x] ^= d[x];
            }
        }
        // ρ + π（目标索引与移位量照抄 ds2api `pow/deepseek_hash.go` 参考实现）
        let mut b = [0u64; 25];
        b[0] = a[0];
        b[10] = rotl64(a[1], 1);
        b[20] = rotl64(a[2], 62);
        b[5] = rotl64(a[3], 28);
        b[15] = rotl64(a[4], 27);
        b[16] = rotl64(a[5], 36);
        b[1] = rotl64(a[6], 44);
        b[11] = rotl64(a[7], 6);
        b[21] = rotl64(a[8], 55);
        b[6] = rotl64(a[9], 20);
        b[7] = rotl64(a[10], 3);
        b[17] = rotl64(a[11], 10);
        b[2] = rotl64(a[12], 43);
        b[12] = rotl64(a[13], 25);
        b[22] = rotl64(a[14], 39);
        b[23] = rotl64(a[15], 41);
        b[8] = rotl64(a[16], 45);
        b[18] = rotl64(a[17], 15);
        b[3] = rotl64(a[18], 21);
        b[13] = rotl64(a[19], 8);
        b[14] = rotl64(a[20], 18);
        b[24] = rotl64(a[21], 2);
        b[9] = rotl64(a[22], 61);
        b[19] = rotl64(a[23], 56);
        b[4] = rotl64(a[24], 14);
        // χ（按行取右邻）与 ι
        let mut next = [0u64; 25];
        for y in 0..5 {
            for x in 0..5 {
                next[y * 5 + x] = b[y * 5 + x] ^ (!b[y * 5 + (x + 1) % 5] & b[y * 5 + (x + 2) % 5]);
            }
        }
        next[0] ^= RC[round];
        a = next;
    }
    *state = a;
}

/// DeepSeekHashV1 摘要（32 字节）。
pub fn deepseek_hash_v1(data: &[u8]) -> [u8; 32] {
    let mut state = [0u64; 25];
    let mut offset = 0;
    while offset + RATE <= data.len() {
        for i in 0..RATE / 8 {
            let mut chunk = [0u8; 8];
            chunk.copy_from_slice(&data[offset + i * 8..offset + i * 8 + 8]);
            state[i] ^= u64::from_le_bytes(chunk);
        }
        keccak_f23(&mut state);
        offset += RATE;
    }
    // 终结块：剩余数据 + SHA3 padding（0x06 … 0x80）
    let mut tail = [0u8; RATE];
    let remaining = data.len() - offset;
    tail[..remaining].copy_from_slice(&data[offset..]);
    tail[remaining] = 0x06;
    tail[RATE - 1] |= 0x80;
    for i in 0..RATE / 8 {
        let mut chunk = [0u8; 8];
        chunk.copy_from_slice(&tail[i * 8..i * 8 + 8]);
        state[i] ^= u64::from_le_bytes(chunk);
    }
    keccak_f23(&mut state);
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[i * 8..i * 8 + 8].copy_from_slice(&state[i].to_le_bytes());
    }
    out
}

/// DeepSeek 网页版 PoW 挑战（`data.biz_data.challenge`）。
#[derive(Debug, Clone)]
pub struct PowChallenge {
    pub algorithm: String,
    pub challenge: String,
    pub salt: String,
    pub expire_at: i64,
    pub difficulty: i64,
    pub signature: String,
    pub target_path: String,
}

impl PowChallenge {
    /// 从 `data.biz_data.challenge` JSON 解析；形态不符返回 None。
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        Some(Self {
            algorithm: str_of(value, &["algorithm"])?,
            challenge: str_of(value, &["challenge"])?,
            salt: str_of(value, &["salt"])?,
            expire_at: value.get("expire_at").and_then(serde_json::Value::as_i64)?,
            difficulty: value
                .get("difficulty")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(144_000),
            signature: str_of(value, &["signature"])?,
            target_path: str_of(value, &["target_path"])?,
        })
    }

    /// `x-ds-pow-response` 头的值：`base64(JSON{algorithm,challenge,salt,
    /// answer,signature,target_path})`（不含 difficulty / expire_at）。
    pub fn header_with_answer(&self, answer: i64) -> Option<String> {
        use base64::Engine;
        let payload = serde_json::json!({
            "algorithm": self.algorithm,
            "challenge": self.challenge,
            "salt": self.salt,
            "answer": answer,
            "signature": self.signature,
            "target_path": self.target_path,
        });
        let bytes = serde_json::to_vec(&payload).ok()?;
        Some(base64::engine::general_purpose::STANDARD.encode(bytes))
    }
}

fn str_of(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(serde_json::Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

/// 求解 PoW：找 nonce ∈ [0, difficulty) 使
/// `DeepSeekHashV1("<salt>_<expire_at>_<nonce>")` 的前 32 字节 == challenge。
/// 无解（或算法不认识）返回 None。
pub fn solve(challenge: &PowChallenge) -> Option<i64> {
    if challenge.algorithm != "DeepSeekHashV1" {
        return None;
    }
    if challenge.challenge.len() != 64 {
        return None;
    }
    let mut target = [0u8; 32];
    hex::decode_to_slice(&challenge.challenge, &mut target).ok()?;
    let prefix = format!("{}_{}_", challenge.salt, challenge.expire_at);
    let difficulty = if challenge.difficulty <= 0 {
        144_000
    } else {
        challenge.difficulty
    };
    for nonce in 0..difficulty {
        let input = format!("{prefix}{nonce}");
        if deepseek_hash_v1(input.as_bytes()) == target {
            return Some(nonce);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    /// 测试向量来自直接调用 DeepSeek 官方 WASM（ds2api pow 测试逐字移植）。
    #[test]
    fn hash_matches_official_wasm_vectors() {
        let cases: &[(&str, &str)] = &[
            (
                "",
                "e594808bc5b7151ac160c6d39a02e0a8e261ed588578403099e3561dc40c26b3",
            ),
            (
                "testsalt_1700000000_42",
                "d4a2ea58c89e40887c933484868380c6f803eaa8dc53a3b9df8e431b921a4f09",
            ),
            (
                "testsalt_1700000000_100000",
                "abea2f35796b65486e9be1b36f7878c66cab021e96faa473fdf4decd31f9ba30",
            ),
            (
                "abc123salt_1700000000_12345",
                "74b3b7452745b70e85eb32ee7f0a9ec0381d42dd5137b695da915e104fc390e1",
            ),
        ];
        for (input, want) in cases {
            let got = hex::encode(deepseek_hash_v1(input.as_bytes()));
            assert_eq!(&got, want, "hash({input})");
        }
    }

    #[test]
    fn solve_finds_the_nonce() {
        for (salt, expire, answer, difficulty) in [
            ("testsalt", 1_700_000_000i64, 42i64, 1_000i64),
            ("testsalt", 1_700_000_000, 500, 2_000),
            ("abc123salt", 1_700_000_000, 12_345, 20_000),
        ] {
            let digest = hex::encode(deepseek_hash_v1(
                format!("{salt}_{expire}_{answer}").as_bytes(),
            ));
            let challenge = PowChallenge {
                algorithm: "DeepSeekHashV1".to_string(),
                challenge: digest,
                salt: salt.to_string(),
                expire_at: expire,
                difficulty,
                signature: "sig".to_string(),
                target_path: "/api/v0/chat/completion".to_string(),
            };
            assert_eq!(solve(&challenge), Some(answer));
        }
    }

    #[test]
    fn header_payload_shape() {
        let digest = hex::encode(deepseek_hash_v1(b"salt_1712345678_777"));
        let challenge = PowChallenge {
            algorithm: "DeepSeekHashV1".to_string(),
            challenge: digest,
            salt: "salt".to_string(),
            expire_at: 1_712_345_678,
            difficulty: 2_000,
            signature: "sig".to_string(),
            target_path: "/api/v0/chat/completion".to_string(),
        };
        let answer = solve(&challenge).expect("2000 内应有解");
        let header = challenge.header_with_answer(answer).expect("header");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&header)
            .expect("base64");
        let value: serde_json::Value = serde_json::from_slice(&raw).expect("json");
        assert_eq!(value["algorithm"], "DeepSeekHashV1");
        assert_eq!(value["answer"], answer);
        assert_eq!(value["target_path"], "/api/v0/chat/completion");
        assert!(value.get("difficulty").is_none(), "头载荷不含 difficulty");
    }

    #[test]
    fn unsupported_algorithm_is_rejected() {
        let challenge = PowChallenge {
            algorithm: "SomethingElse".to_string(),
            challenge: "00".repeat(32),
            salt: "s".to_string(),
            expire_at: 1,
            difficulty: 10,
            signature: "sig".to_string(),
            target_path: "/api/v0/chat/completion".to_string(),
        };
        assert_eq!(solve(&challenge), None);
    }
}
