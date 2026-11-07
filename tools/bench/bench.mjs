// AIGX 性能基准 harness（零依赖）
// 用法：node bench.mjs --mode nonstream|stream --conc 8 --n 1500 --label v1.0.34
// 输出：JSON 一行（RPS / P50/P90/P99 / err / TTFB[stream]），可叠加执行
import http from 'node:http';
import { execSync } from 'node:child_process';

const arg = (k, d) => {
  const i = process.argv.indexOf('--' + k);
  return i > 0 ? process.argv[i + 1] : d;
};
const MODE = arg('mode', 'nonstream');
const CONC = Number(arg('conc', 8));
const N = Number(arg('n', 1500));
const LABEL = arg('label', 'run');
const BASE = arg('base', 'http://127.0.0.1:18800');
const KEY = arg('key', 'sk-bench-0001');
const MODEL = arg('model', 'mock-model');
const url = new URL(BASE + '/v1/chat/completions');

const body = JSON.stringify({
  model: MODEL,
  messages: [{ role: 'user', content: 'benchmark ping' }],
  ...(MODE === 'stream' ? { stream: true } : {}),
});

const agent = new http.Agent({ keepAlive: true, maxSockets: CONC * 2 });
const lat = [];
const ttfbs = [];
let errs = 0;

function once() {
  return new Promise((resolve) => {
    const t0 = performance.now();
    const req = http.request(
      { hostname: url.hostname, port: url.port, path: url.pathname, method: 'POST', agent, headers: { 'content-type': 'application/json', authorization: `Bearer ${KEY}`, 'content-length': Buffer.byteLength(body) } },
      (res) => {
        let firstByte = null;
        res.on('data', () => { if (firstByte === null) firstByte = performance.now(); });
        res.on('end', () => {
          const t1 = performance.now();
          resolve({ lat: t1 - t0, ttfb: firstByte === null ? t1 - t0 : firstByte - t0, code: res.statusCode });
        });
      }
    );
    req.on('error', () => resolve({ lat: NaN, ttfb: NaN, code: 0 }));
    req.end(body);
  });
}

// ── RSS 采样（压测期间，tasklist 免 PowerShell 转义坑）──
const EXE = 'AIGX-1.0.34-windows-amd64.exe';
let rssPeakMB = 0;
const sampler = setInterval(() => {
  try {
    const out = execSync(`tasklist /FI "IMAGENAME eq ${EXE}" /FO CSV /NH`).toString();
    // 形如 "AIGX-...exe","12608","Console","1","29,812 K"
    const m = out.match(/"([\d,]+) K"/);
    if (m) rssPeakMB = Math.max(rssPeakMB, Number(m[1].replace(/,/g, '')) / 1024);
  } catch {}
}, 1000);

// warmup
for (let i = 0; i < 50; i++) await once();

// 压测
const t0 = performance.now();
let done = 0;
await Promise.all(
  Array.from({ length: CONC }, async () => {
    while (true) {
      const idx = done + 1;
      if (idx > N) break;
      done++;
      const r = await once();
      if (!Number.isFinite(r.lat) || r.code !== 200) errs++;
      else { lat.push(r.lat); if (MODE === 'stream') ttfbs.push(r.ttfb); }
    }
  })
);
const wall = performance.now() - t0;
clearInterval(sampler);

const pct = (arr, p) => arr.length ? arr[Math.min(arr.length - 1, Math.floor((p / 100) * arr.length))] : NaN;
lat.sort((a, b) => a - b);
ttfbs.sort((a, b) => a - b);
console.log(JSON.stringify({
  label: LABEL, mode: MODE, conc: CONC, n: N,
  rps: +(N / (wall / 1000)).toFixed(1),
  p50: +pct(lat, 50).toFixed(1), p90: +pct(lat, 90).toFixed(1), p99: +pct(lat, 99).toFixed(1),
  ...(MODE === 'stream' ? { ttfb_p50: +pct(ttfbs, 50).toFixed(1), ttfb_p99: +pct(ttfbs, 99).toFixed(1) } : {}),
  err: errs, rss_peak_mb: +rssPeakMB.toFixed(1), wall_s: +(wall / 1000).toFixed(1),
}));
