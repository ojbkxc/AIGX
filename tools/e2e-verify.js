// AIGX e2e 验证脚本：ALTCHA 登录 → 首登改密 → admin 端点探测
const crypto = require('crypto');

const BASE = 'http://127.0.0.1:8080';
const NEW_PW = 'Aigx#2026verify';

function solveCaptcha(cap) {
  for (let n = 0; n <= cap.maxnumber; n++) {
    if (crypto.createHash('sha256').update(cap.salt + n).digest('hex') === cap.challenge) return n;
  }
  return null;
}

async function getCaptcha() {
  const r = await fetch(`${BASE}/api/auth/captcha`);
  const cap = await r.json();
  const number = solveCaptcha(cap);
  const captcha = Buffer.from(JSON.stringify({
    algorithm: 'SHA-256', challenge: cap.challenge, salt: cap.salt,
    signature: cap.signature, number,
  })).toString('base64');
  return { captcha, cap };
}

async function login(password) {
  const { captcha } = await getCaptcha();
  const r = await fetch(`${BASE}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ email: 'admin@gmail.com', password, captcha }),
  });
  return { status: r.status, body: await r.json() };
}

async function main() {
  // 登录：密码可能是上次运行改过的 NEW_PW，也可能是初始 123456
  let loginRes = await login(NEW_PW);
  if (!loginRes.body?.data?.token) loginRes = await login('123456');
  console.log('login:', loginRes.status, 'success:', loginRes.body.success);
  const token = loginRes.body?.data?.token;
  if (!token) { console.error('no token', JSON.stringify(loginRes.body)); process.exit(1); }
  let auth = { Authorization: `Bearer ${token}` };

  // 首登（用初始密码）返回 PASSWORD_CHANGE_REQUIRED 时，改密后重登
  if (loginRes.body.error === 'Password change required') {
    const cp = await fetch(`${BASE}/api/auth/change-password`, {
      method: 'POST',
      headers: { ...auth, 'Content-Type': 'application/json' },
      body: JSON.stringify({ old_password: '123456', new_password: NEW_PW }),
    });
    console.log('change-password:', cp.status);
    const lr = await login(NEW_PW);
    console.log('re-login:', lr.status, 'success:', lr.body.success);
    if (lr.body?.data?.token) auth = { Authorization: `Bearer ${lr.body.data.token}` };
  }

  // 探测端点
  const probes = [
    ['/api/monitor/health', 'monitor health（真实 uptime/内存）'],
    ['/api/monitor/system', 'monitor system'],
    ['/api/diagnostics/summary', 'diagnostics summary'],
    ['/api/diagnostics/breakers', 'diagnostics breakers'],
    ['/api/agent/config', 'agent config（camelCase 统一）'],
    ['/api/agent/settings', 'agent settings'],
    ['/api/alerts/active', 'alerts active'],
    ['/api/scheduled-tasks', 'scheduled tasks（含新字段）'],
  ];
  for (const [path, label] of probes) {
    const r = await fetch(`${BASE}${path}`, { headers: auth });
    const text = await r.text();
    let body = text;
    try { body = JSON.parse(text); } catch {}
    const preview = typeof body === 'string' ? body.slice(0, 300) : JSON.stringify(body).slice(0, 500);
    console.log(`\n== ${label} == ${path} -> ${r.status}`);
    console.log(preview);
  }
}

main().catch((e) => { console.error(e); process.exit(1); });
