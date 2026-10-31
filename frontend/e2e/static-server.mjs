// E2E 用的静态服务器：托管 ../static（vite 构建产物）+ 最小 API 桩。
//
// 为什么不用 `vite preview`：E2E 要验证的是**入库的构建产物**（线上跑的就是
// static/），而不是 dev server 的即时编译。同时这里要拦截 /api/*，让测试不
// 依赖真实后端与数据库，可离线、可并发、无副作用。
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..', '..', 'static');
const PORT = Number(process.env.E2E_PORT || 4173);

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.webp': 'image/webp',
  '.ico': 'image/x-icon',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
};

/** 管理员会话桩：role=admin，凭此通过前端 AdminRoute 守卫 */
const ME_ADMIN = {
  id: 'u-admin',
  email: 'admin@aigx.local',
  username: 'admin',
  role: 'admin',
  status: 'active',
  quota: null,
  used_quota: 0,
  aff_code: 'demo',
};

/** 列表类接口统一返回空数组，避免各页面因结构不符而报错 */
const EMPTY_LIST = JSON.stringify({ success: true, data: [], total: 0, page: 1, size: 20 });

function sendJson(res, body, status = 200) {
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end(typeof body === 'string' ? body : JSON.stringify(body));
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const p = url.pathname;

  if (p.startsWith('/api/') || p.startsWith('/v1/')) {
    // 少数需要具体形状的端点
    if (p === '/api/users/me') return sendJson(res, { success: true, data: ME_ADMIN });
    if (p === '/api/auth/logout') return sendJson(res, { success: true, data: null });
    if (p === '/api/models/available') return sendJson(res, { success: true, data: ['glm-4'] });
    if (p === '/api/network/status') return sendJson(res, { success: true, data: {} });
    if (p === '/api/dashboard/realtime') return sendJson(res, { success: true, data: {} });
    return sendJson(res, EMPTY_LIST);
  }

  // SPA 前端路由：找不到实体文件时回退 index.html
  let file = path.join(ROOT, p === '/' ? 'index.html' : decodeURIComponent(p));
  if (!file.startsWith(ROOT) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
    file = path.join(ROOT, 'index.html');
  }
  if (!fs.existsSync(file)) {
    res.writeHead(500, { 'Content-Type': 'text/plain; charset=utf-8' });
    return res.end('static/ missing — run `npm run build` first');
  }
  res.writeHead(200, {
    'Content-Type': MIME[path.extname(file).toLowerCase()] || 'application/octet-stream',
    // index.html 不缓存，避免测试读到上一轮构建的引用
    'Cache-Control': path.basename(file) === 'index.html' ? 'no-store' : 'public, max-age=3600',
  });
  fs.createReadStream(file).pipe(res);
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`[e2e] static server on http://127.0.0.1:${PORT} (root: ${ROOT})`);
});
