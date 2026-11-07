// 建 N 个同构渠道（选路规模测试用）；FRESH=1 时先清全部 bench-mock*
import crypto from 'node:crypto';
const BASE = 'http://127.0.0.1:18800';
const GUA = process.env.UPSTREAM_HOST || '';
const N = Number(process.env.CHANNELS || 31);
const j = async (m, p, b, t) => { const r = await fetch(BASE + p, { method: m, headers: { 'content-type': 'application/json', ...(t ? { authorization: 'Bearer ' + t } : {}) }, body: b ? JSON.stringify(b) : undefined }); const x = await r.text(); if (!r.ok) throw new Error(m + ' ' + p + ' ' + r.status + ' ' + x.slice(0, 150)); return JSON.parse(x); };
const solve = async () => { const c = await j('GET', '/api/auth/captcha'); for (let n = 0; n < 2e6; n++) { if (crypto.createHash('sha256').update(c.salt + n).digest('hex') === c.challenge) return Buffer.from(JSON.stringify({ algorithm: 'SHA-256', challenge: c.challenge, salt: c.salt, signature: c.signature, number: n })).toString('base64'); } throw new Error('altcha fail'); };
(async () => {
  const l = await j('POST', '/api/auth/login', { email: 'admin@gmail.com', password: process.env.PASSWORD || 'Bench#20261108', captcha: await solve() });
  const t = l?.data?.token ?? l?.token;
  if (process.env.FRESH === '1') {
    const list = await j('GET', '/api/channels', null, t);
    const items = [].concat(list?.data?.items ?? list?.data ?? list?.items ?? []);
    for (const c of items) if (String(c?.name || '').startsWith('bench-mock')) await j('DELETE', `/api/channels/${c.id}`, null, t).catch(() => {});
  }
  for (let i = 1; i <= N; i++) {
    try { await j('POST', '/api/channels', { name: 'bench-mock' + (i === 1 ? '' : '-' + i), channel_type: 'openai_compatible', base_url: `http://[${GUA}]:18900/v1`, api_key: 'sk-bench-mock', priority: 100, weight: 100, status: 'enabled', models: ['mock-model'] }, t); }
    catch (e) { if (!String(e).includes('已存在')) throw e; }
  }
  console.log('channels ready:', N);
})();
