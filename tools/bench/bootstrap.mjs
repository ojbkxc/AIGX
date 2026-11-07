// AIGX bench 引导：解 ALTCHA → 登录 → 改密 → 建渠道(→GUA mock) → 建 API key
// 产出 C:/tmp/aigx-bench/bench.env（KEY=... 形态，供压测脚本 source）
import crypto from 'node:crypto';

const BASE = 'http://127.0.0.1:18800';
const GUA = process.env.UPSTREAM_HOST || '';
const MOCK = `http://[${GUA}]:18900`;
const PASSWORD = 'Bench#20261108';

const j = async (method, path, body, token) => {
  const res = await fetch(BASE + path, {
    method,
    headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let data; try { data = JSON.parse(text); } catch { data = text; }
  if (!res.ok) throw new Error(`${method} ${path} -> ${res.status} ${text.slice(0, 300)}`);
  return data;
};

// 1. 解 ALTCHA PoW（每次登录现解——challenge 一次性消费）
const solveCaptcha = async () => {
  const ch = await j('GET', '/api/auth/captcha');
  let number = 0;
  for (; number <= (ch.maxnumber ?? ch?.data?.maxnumber ?? 1_000_000); number++) {
    const salt = ch.salt ?? ch?.data?.salt;
    const target = ch.challenge ?? ch?.data?.challenge;
    const h = crypto.createHash('sha256').update(salt + number).digest('hex');
    if (h === target) {
      const payload = Buffer.from(JSON.stringify({
        algorithm: 'SHA-256', challenge: target, salt, signature: ch.signature ?? ch?.data?.signature, number,
      })).toString('base64');
      return payload;
    }
  }
  throw new Error('ALTCHA solve failed');
};
console.log('ALTCHA solved');

// 2. 登录（幂等：密码可能已改过）
const tryLogin = async (password) => {
  try {
    return await j('POST', '/api/auth/login', { email: 'admin@gmail.com', password, captcha: await solveCaptcha() });
  } catch (e) {
    if (String(e).includes('账号或密码错误')) return null;
    throw e;
  }
};
const fresh = process.env.FRESH !== '0';
let loginRes = null;
if (fresh) loginRes = await tryLogin('123456');
if (!loginRes) loginRes = await tryLogin(PASSWORD);
if (!loginRes) throw new Error('both passwords rejected');
const token = loginRes?.data?.token ?? loginRes?.token;
if (!token) throw new Error('no token: ' + JSON.stringify(loginRes).slice(0, 300));
console.log('login ok');

// 3. 改密（幂等：已改过则忽略「旧密码不正确」）
try {
  await j('POST', '/api/auth/change-password', { old_password: '123456', new_password: PASSWORD }, token);
  console.log('password changed ->', PASSWORD);
} catch (e) {
  if (!String(e).includes('旧密码不正确')) throw e;
  console.log('password already changed');
}
const re = await j('POST', '/api/auth/login', { email: 'admin@gmail.com', password: PASSWORD, captcha: await solveCaptcha() });
const token2 = re?.data?.token ?? re?.token;

// 4. 建渠道（openai_compatible → mock GUA；先清旧 bench 渠道保证幂等）
const list = await j('GET', '/api/channels', null, token2);
const items = list?.data?.items ?? list?.data ?? list?.items ?? [];
for (const c of [].concat(items)) {
  if (c?.name === 'bench-mock' && c?.id) {
    await j('DELETE', `/api/channels/${c.id}`, null, token2).catch(() => {});
    console.log('removed stale channel', c.id);
  }
}
const chRes = await j('POST', '/api/channels', {
  name: 'bench-mock',
  channel_type: 'openai_compatible',
  base_url: MOCK + '/v1',
  api_key: 'sk-bench-mock',
  priority: 100,
  weight: 100,
  status: 'enabled',
  models: ['mock-model'],
}, token2);
console.log('channel:', JSON.stringify(chRes).slice(0, 200));

// 5. 建 API key（固定值，可复现；已存在则沿用）
try {
  const keyRes = await j('POST', '/api/tokens', { name: 'bench-key', custom_key: 'sk-bench-0001' }, token2);
  console.log('token:', JSON.stringify(keyRes).slice(0, 160));
} catch (e) {
  if (!String(e).includes('已存在')) throw e;
  console.log('token already exists, reuse sk-bench-0001');
}

// 6. 冒烟：数据面一发
const smoke = await fetch(BASE + '/v1/chat/completions', {
  method: 'POST',
  headers: { 'content-type': 'application/json', authorization: 'Bearer sk-bench-0001' },
  body: JSON.stringify({ model: 'mock-model', messages: [{ role: 'user', content: 'hi' }] }),
});
console.log('smoke:', smoke.status, (await smoke.text()).slice(0, 160));
