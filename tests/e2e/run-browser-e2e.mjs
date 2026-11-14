#!/usr/bin/env node
// 浏览器 E2E CI runner：起「真后端 + loopback mock 上游」再驱动 run-e2e.mjs。
//
// 姿势对齐 tests/e2e/499_disconnect_billing.rs 的 spawn_server 惯例：
// - tempdir HOME + 预写 ~/.aigx/config.toml（data_dir + allow_internal_upstream=true
//   逃生阀，否则 SSRF 防护拒绝 loopback 渠道）
// - spawn 编译产物 aigx，清掉代理类 env，stdout/stderr 落 server.log
// - /api/monitor/healthz 就绪探针（60s 超时）
// - 首启一次性管理员密码从 server.log 解析（673a9a67 起随机化且仅打印一次，
//   `Aigx-` 前缀 + 24 位 base64url）→ 登录 → 改密 → 用测试密码重登
// - 注册 loopback mock 渠道（models 含 deepseek-chat，run-e2e.mjs 模型搜索断言依赖）
// - env 注入 AIGX_E2E_BASE / AIGX_E2E_USER / AIGX_E2E_PASS / CHROME_PATH
//
// 失败时把 server.log 尾部打印到 stdout，并复制到 AIGX_E2E_ARTIFACT_DIR（如设置）。
import { spawn, spawnSync } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '..', '..');
const AIGX_BIN = process.argv[2] || path.join(REPO, 'target', 'debug', 'aigx');
const ARTIFACT_DIR = process.env.AIGX_E2E_ARTIFACT_DIR || '';

// 测试密码每次运行随机生成（不回写仓库；run-e2e.mjs 通过 env 拿到）
const E2E_PASS = `E2e-${crypto.randomBytes(12).toString('base64url')}`;

const log = (msg) => console.log(`[runner] ${msg}`);
const die = (msg) => {
  console.error(`[runner] FATAL: ${msg}`);
  process.exit(1);
};

function freePort() {
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.listen(0, '127.0.0.1', () => {
      const { port } = srv.address();
      srv.close(() => resolve(port));
    });
    srv.on('error', reject);
  });
}

function readFirstLine(child) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('mock upstream 启动超时')), 15_000);
    child.stdout.once('data', (d) => {
      clearTimeout(timer);
      const line = String(d).split('\n')[0];
      try {
        resolve(JSON.parse(line));
      } catch {
        reject(new Error(`mock upstream 首行非 JSON: ${line.slice(0, 120)}`));
      }
    });
    child.once('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`mock upstream 提前退出 code=${code}`));
    });
  });
}

/** ALTCHA PoW（sha256(salt+number) == challenge），照 tools/bench/bootstrap.mjs 姿势 */
function solveAltcha(ch) {
  const salt = ch.salt ?? ch?.data?.salt;
  const target = ch.challenge ?? ch?.data?.challenge;
  const max = ch.maxnumber ?? ch?.data?.maxnumber ?? 1_000_000;
  for (let n = 0; n <= max; n++) {
    if (crypto.createHash('sha256').update(salt + n).digest('hex') === target) {
      return Buffer.from(
        JSON.stringify({
          algorithm: 'SHA-256',
          challenge: target,
          salt,
          signature: ch.signature ?? ch?.data?.signature,
          number: n,
        }),
      ).toString('base64');
    }
  }
  throw new Error('ALTCHA solve failed');
}

async function api(base, method, reqPath, body, token) {
  const res = await fetch(base + reqPath, {
    method,
    headers: {
      'content-type': 'application/json',
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let data;
  try {
    data = JSON.parse(text);
  } catch {
    data = text;
  }
  return { status: res.status, ok: res.ok, body: data, text };
}

/** 从 server.log 提取首启一次性密码（与 499 的 initial_password_from_log 同口径） */
function initialPasswordFromLog(logFile) {
  const text = fs.readFileSync(logFile, 'utf-8');
  const m = text.match(/Aigx-[A-Za-z0-9_-]{24}/);
  return m ? m[0] : null;
}

function tail(file, n = 40) {
  try {
    const lines = fs.readFileSync(file, 'utf-8').split('\n');
    return lines.slice(-n).join('\n');
  } catch {
    return `(无法读取 ${file})`;
  }
}

function killTree(child) {
  if (!child || child.exitCode !== null || child.killed) return;
  if (process.platform === 'win32') {
    spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F']);
  } else {
    try {
      process.kill(-child.pid, 'SIGKILL');
    } catch {
      child.kill('SIGKILL');
    }
  }
}

const children = [];
let workdir = '';

try {
  if (!fs.existsSync(AIGX_BIN)) {
    die(`aigx 二进制不存在: ${AIGX_BIN}（先 cargo build）`);
  }

  // ── mock 上游 ──
  const mockPort = await freePort();
  const mock = spawn(process.execPath, [path.join(HERE, 'mock-upstream.mjs')], {
    cwd: HERE,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  children.push(mock);
  mock.stderr.on('data', (d) => console.error('[mock]', String(d)));
  const mockInfo = await readFirstLine(mock);
  if (mockInfo.port !== mockPort) {
    // mock 自报端口以它实际 bind 的为准（防竞态）
    log(`mock 端口以子进程自报为准: ${mockInfo.port}（预选 ${mockPort}）`);
  }
  log(`mock upstream 就绪: http://127.0.0.1:${mockInfo.port}`);

  // ── 起后端（499 spawn_server 姿势）──
  const port = await freePort();
  workdir = fs.mkdtempSync(path.join(os.tmpdir(), 'aigx-browser-e2e-'));
  const dataDir = path.join(workdir, 'data');
  const aigxDir = path.join(workdir, '.aigx');
  fs.mkdirSync(dataDir, { recursive: true });
  fs.mkdirSync(aigxDir, { recursive: true });
  const cfgDataDir = dataDir.replace(/\\/g, '/');
  fs.writeFileSync(
    path.join(aigxDir, 'config.toml'),
    `[server]\nhost = "127.0.0.1"\nport = ${port}\ndata_dir = "${cfgDataDir}"\n[usage]\nallow_internal_upstream = true\n`,
  );
  const serverLog = path.join(workdir, 'server.log');
  const logStream = fs.openSync(serverLog, 'a');
  // 中间盒/代理 env 一律清掉（网关出站直连 loopback mock；499 同款防御）
  const aigxEnv = { ...process.env };
  for (const k of ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy']) {
    delete aigxEnv[k];
  }
  Object.assign(aigxEnv, {
    HOME: workdir,
    USERPROFILE: workdir,
    AIGX_SERVER__DATA_DIR: dataDir,
    AIGX_SERVER__PORT: String(port),
    AIGX_SERVER__HOST: '127.0.0.1',
  });
  const aigx = spawn(AIGX_BIN, [], {
    // cwd 必须 = REPO：find_static_dir()（src/web/mod.rs）的候选全是
    // cwd 相对或 exe 目录相对（./static、./frontend/dist…），tempdir
    // cwd 下找不到任何 static ⇒ SPA 页面全 404 ⇒ 登录页无表单。
    // REPO/static 即本 job 里 npm run build 刚产出的产物（与被测口径一致）。
    cwd: REPO,
    env: aigxEnv,
    stdio: ['ignore', logStream, logStream],
    detached: process.platform !== 'win32',
  });
  children.push(aigx);
  const base = `http://127.0.0.1:${port}`;
  log(`aigx spawned (pid=${aigx.pid}) base=${base}`);

  // 就绪探针：healthz（比只等端口更严）
  const deadline = Date.now() + 60_000;
  let ready = false;
  while (Date.now() < deadline) {
    if (aigx.exitCode !== null) break;
    try {
      const r = await fetch(`${base}/api/monitor/healthz`);
      if (r.ok) {
        ready = true;
        break;
      }
    } catch {
      // 未就绪
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  if (!ready) {
    console.error(tail(serverLog));
    die(`网关 60s 未就绪（base=${base}），server.log 尾部见上`);
  }
  log(`网关就绪: ${base}`);

  // ── 登录：首启一次性密码 → 改密 → 测试密码重登（499 admin_token 姿势）──
  const initial = initialPasswordFromLog(serverLog);
  if (!initial) {
    console.error(tail(serverLog));
    die('server.log 中未找到 Aigx- 一次性初始密码（673a9a67 随机化后必须从日志取）');
  }
  log(`提取到首启初始密码: ${initial.slice(0, 5)}...（已脱敏）`);
  // ALTCHA challenge 一次性消费（tools/bench/bootstrap.mjs 同口径注释）：
  // 每次登录都要重新 GET /api/auth/captcha + 现解 PoW，不能跨登录复用。
  const login = async (password) => {
    const captcha = solveAltcha((await api(base, 'GET', '/api/auth/captcha')).body);
    return api(base, 'POST', '/api/auth/login', { email: 'admin@gmail.com', password, captcha });
  };
  let loginRes = await login(initial);
  if (!loginRes.body?.data?.token && !loginRes.body?.token) {
    console.error(tail(serverLog));
    die(`初始密码登录被拒: ${loginRes.text.slice(0, 300)}`);
  }
  const cp = await api(base, 'POST', '/api/auth/change-password', {
    old_password: initial,
    new_password: E2E_PASS,
  }, loginRes.body?.data?.token ?? loginRes.body?.token);
  if (!cp.ok && !cp.text.includes('旧密码不正确')) {
    die(`change-password 失败: ${cp.text.slice(0, 300)}`);
  }
  const relogin = await login(E2E_PASS);
  const adminToken = relogin.body?.data?.token ?? relogin.body?.token;
  if (!adminToken) {
    die(`改密后重登失败: ${relogin.text.slice(0, 300)}`);
  }
  log('管理员改密完成，测试密码就绪');

  // ── 注册 loopback mock 渠道（run-e2e.mjs：渠道行>0、搜 deepseek、对话收发）──
  const chRes = await api(base, 'POST', '/api/channels', {
    name: 'browser-e2e-mock',
    channel_type: 'openai_compatible',
    base_url: `http://127.0.0.1:${mockInfo.port}/v1`,
    api_key: 'sk-browser-e2e-mock',
    priority: 100,
    weight: 100,
    status: 'enabled',
    models: ['deepseek-chat'],
  }, adminToken);
  if (!chRes.ok) {
    die(`渠道注册失败: ${chRes.text.slice(0, 300)}`);
  }
  log('mock 渠道已注册 (deepseek-chat)');

  // ── 解析 playwright chromium 路径（存在才设 CHROME_PATH；本地无缓存则走系统 Chrome）──
  const chromeRes = spawnSync(
    process.execPath,
    ['-p', "require('playwright-core').chromium.executablePath()"],
    { cwd: path.join(REPO, 'frontend'), encoding: 'utf-8' },
  );
  const chromePath = chromeRes.stdout?.trim();
  const env = {
    ...process.env,
    AIGX_E2E_BASE: base,
    AIGX_E2E_USER: 'admin',
    AIGX_E2E_PASS: E2E_PASS,
  };
  if (chromePath && fs.existsSync(chromePath)) {
    env.CHROME_PATH = chromePath;
    log(`CHROME_PATH=${chromePath}`);
  } else {
    log('未发现 playwright 缓存浏览器，run-e2e.mjs 将回退系统 Chrome（CHROME_PATH 可外部指定）');
  }

  // ── 驱动 run-e2e.mjs ──
  const r = spawnSync(process.execPath, [path.join(HERE, 'run-e2e.mjs')], {
    env,
    stdio: 'inherit',
  });
  if (r.error) die(`run-e2e.mjs 启动失败: ${r.error.message}`);

  if (r.status !== 0) {
    console.error(`[runner] run-e2e.mjs 退出码 ${r.status}，server.log 尾部:`);
    console.error(tail(serverLog));
    process.exitCode = r.status || 1;
  } else {
    log('浏览器 E2E 全部通过');
  }
} catch (e) {
  console.error(`[runner] 异常: ${e.stack || e.message}`);
  process.exitCode = 1;
} finally {
  for (const c of children.reverse()) killTree(c);
  if (workdir && process.exitCode === undefined) {
    fs.rmSync(workdir, { recursive: true, force: true });
  } else if (workdir && ARTIFACT_DIR) {
    try {
      fs.mkdirSync(ARTIFACT_DIR, { recursive: true });
      for (const f of fs.readdirSync(workdir)) {
        fs.copyFileSync(path.join(workdir, f), path.join(ARTIFACT_DIR, f));
      }
      log(`失败工件已复制到 ${ARTIFACT_DIR}`);
    } catch {
      console.error(`[runner] 工件复制失败，原始目录: ${workdir}`);
    }
  }
}
