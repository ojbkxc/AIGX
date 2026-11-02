/**
 * ALTCHA 机器人校验（proof-of-work）客户端求解器 —— 纯逻辑，无 DOM。
 *
 * 协议与服务端实现见 `src/altcha.rs`（题面、答案格式、防重放那边有完整说明）：
 * 网关签发 `{algorithm, challenge, maxnumber, salt, signature}`，客户端枚举
 * number 使 `sha256(salt + number) === challenge`，把答案连同题面放进
 * base64(JSON) 提交。
 *
 * ── 为什么不用官方 altcha widget ────────────────────────────
 * 官方实现在**非安全上下文**下开工即抛「Web Crypto is not available.
 * Secure context is required」：浏览器的 `crypto.subtle` 只在 HTTPS /
 * localhost 提供，而局域网部署天然是 `http://<内网IP>`。结果是组件永远停在
 * 「验证中…」、登录按钮形同失效。本文件自己实现同一套协议：有 `crypto.subtle`
 * 就走原生（HTTPS / localhost，快且不占主线程），没有就用内置纯 JS SHA-256
 * 兜底 —— 两条路径的数学完全一致，服务端不区分，校验强度也不打折。
 */

export interface AltchaChallenge {
  algorithm: string;
  challenge: string;
  maxnumber: number;
  salt: string;
  signature: string;
}

/** 有没有原生 Web Crypto：只在 HTTPS / localhost 下有。 */
export function hasNativeCrypto(): boolean {
  return typeof crypto !== 'undefined' && !!crypto.subtle
    && typeof crypto.subtle.digest === 'function';
}

/* ── SHA-256：`crypto.subtle` 不可用时的兜底实现（FIPS 180-4）────── */

const K = new Int32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);
const H0 = new Int32Array([
  0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
]);

function rotr(value: number, bits: number): number {
  return (value >>> bits) | (value << (32 - bits));
}

function sha256(bytes: Uint8Array): Uint8Array {
  const length = bytes.length;
  // 补位：0x80 + 零填充 + 64 位长度（bit），总长对齐到 64 字节
  const padded = new Uint8Array((((length + 9) >> 6) + 1) << 6);
  padded.set(bytes);
  padded[length] = 0x80;
  const view = new DataView(padded.buffer);
  view.setUint32(padded.length - 8, Math.floor(length / 536870912));
  view.setUint32(padded.length - 4, (length << 3) >>> 0);

  const w = new Int32Array(64);
  const h = Int32Array.from(H0);
  for (let offset = 0; offset < padded.length; offset += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getInt32(offset + i * 4);
    for (let j = 16; j < 64; j++) {
      const x = w[j - 15];
      const y = w[j - 2];
      w[j] = (w[j - 16] + (rotr(x, 7) ^ rotr(x, 18) ^ (x >>> 3)) + w[j - 7]
        + (rotr(y, 17) ^ rotr(y, 19) ^ (y >>> 10))) | 0;
    }
    let a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], hh = h[7];
    for (let k = 0; k < 64; k++) {
      const t1 = (hh + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + K[k] + w[k]) | 0;
      const t2 = ((rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c))) | 0;
      hh = g; g = f; f = e; e = (d + t1) | 0; d = c; c = b; b = a; a = (t1 + t2) | 0;
    }
    h[0] = (h[0] + a) | 0; h[1] = (h[1] + b) | 0; h[2] = (h[2] + c) | 0; h[3] = (h[3] + d) | 0;
    h[4] = (h[4] + e) | 0; h[5] = (h[5] + f) | 0; h[6] = (h[6] + g) | 0; h[7] = (h[7] + hh) | 0;
  }
  const out = new Uint8Array(32);
  const outView = new DataView(out.buffer);
  for (let m = 0; m < 8; m++) outView.setInt32(m * 4, h[m]);
  return out;
}

function toHex(bytes: Uint8Array): string {
  let text = '';
  for (let i = 0; i < bytes.length; i++) {
    text += (bytes[i] < 16 ? '0' : '') + bytes[i].toString(16);
  }
  return text;
}

/** `sha256(文本)` → 小写 hex（原生可用走原生，否则纯 JS 兜底）。 */
export async function sha256Hex(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text);
  if (hasNativeCrypto()) {
    const buf = await crypto.subtle.digest('SHA-256', bytes);
    return toHex(new Uint8Array(buf));
  }
  return toHex(sha256(bytes));
}

/**
 * 枚举 number 使 `sha256(salt + number) === challenge`。
 *
 * 每 2000 次让出一次主线程（`yieldEvery`）：纯 JS 路径的 await 只进微任务队列，
 * 不让出的话转圈动画会被冻住（原生路径每次 digest 本身就是异步的，天然让出）。
 * 题目上限 5–10 万次、平均命中在一半，这个量级下一两百毫秒就跑完。
 *
 * `isStale` 每轮检查一次：reset / 重新领题会让旧的一轮作废（返回 -1 由调用方丢弃）。
 */
export async function solveChallenge(
  challenge: AltchaChallenge,
  opts: { isStale?: () => boolean; yieldEvery?: number } = {},
): Promise<number> {
  const target = String(challenge.challenge || '').toLowerCase();
  const salt = String(challenge.salt || '');
  const max = Number(challenge.maxnumber) || 0;
  const yieldEvery = opts.yieldEvery ?? 2000;
  if (!target || !salt) throw new Error('题面不完整');
  for (let n = 0; n <= max; n++) {
    if (opts.isStale?.()) return -1;
    if ((await sha256Hex(salt + n)) === target) return n;
    if (n % yieldEvery === yieldEvery - 1) {
      await new Promise((resolve) => setTimeout(resolve, 0));
    }
  }
  throw new Error(`未找到答案（maxnumber=${max}）`);
}

/**
 * 把题面 + 答案拼成提交用的 payload（base64 的 JSON）。
 * 字段名与 `src/altcha.rs` 的 `verify` 逐字对应，别改。
 */
export function buildPayload(challenge: AltchaChallenge, number: number): string {
  const json = JSON.stringify({
    algorithm: challenge.algorithm,
    challenge: challenge.challenge,
    number,
    salt: challenge.salt,
    signature: challenge.signature,
  });
  // btoa 给的是标准 base64，服务端 std 与 url-safe 都认
  return btoa(json);
}