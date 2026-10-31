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

// ── 统一模型管理视图（形状对齐 src/model/manage.rs 的 view_json）──
// 给两条渠道来源 + 一条账号池来源，用于验证左栏分组、来源 chips、启停开关、
// 「未定价」统计与搜索筛选。
const MANAGE_VIEW = {
  providers: [
    { id: 'openai_compatible', label: 'OpenAI 兼容', total: 2, enabled: 2, account: false },
    { id: 'workbuddy', label: 'WorkBuddy 国内版', total: 1, enabled: 1, account: true },
  ],
  models: [
    {
      provider: 'openai_compatible',
      provider_label: 'OpenAI 兼容',
      id: 'glm-4.7-flash',
      enabled: true,
      sources: [{ kind: 'channel', channel_id: 'ch-1', name: 'lxchat-glm' }],
      owned_by: 'zhipu',
      context_length: 128000,
      capabilities: ['tool', 'vision'],
      price: { model_name: 'glm-4.7-flash', input_price: 0.001, output_price: 0.002, price_type: 'token' },
      model_ratio: 1,
      aliases: [],
    },
    {
      provider: 'openai_compatible',
      provider_label: 'OpenAI 兼容',
      id: 'unpriced-model',
      enabled: false,
      sources: [{ kind: 'channel', channel_id: 'ch-2', name: 'backup-channel' }],
      owned_by: 'aigx',
      context_length: null,
      capabilities: [],
      price: null,
      model_ratio: 1,
      aliases: ['alias-free'],
    },
    {
      provider: 'workbuddy',
      provider_label: 'WorkBuddy 国内版',
      id: 'wb-coder',
      enabled: true,
      sources: [{ kind: 'account', channel_id: 'ch-3', name: 'workbuddy-1', account_id: 'acc-1' }],
      owned_by: 'workbuddy',
      context_length: 64000,
      capabilities: ['tool'],
      price: { model_name: 'wb-coder', input_price: 0.0005, output_price: 0.001, price_type: 'token' },
      model_ratio: 1.2,
      aliases: [],
    },
  ],
  mappings: { 'alias-free': 'unpriced-model' },
  unpriced: 1,
};

// ── 签到中心快照（形状对齐 src/api/admin/checkin_center.rs）──
const CHECKIN_CENTER = {
  settings: { enabled: true, gap_ms: 500 },
  supported_providers: ['workbuddy', 'workbuddy-intl'],
  providers: [
    {
      provider: 'workbuddy',
      label: 'WorkBuddy 国内版',
      today_done: 1,
      today_total: 2,
      accounts: [
        { id: 'acc-1', name: 'workbuddy-1', provider: 'workbuddy', supports_checkin: true, checked_in_today: true, last_checkin_at: 1761900000, last_outcome: 'success', last_message: '签到成功 +100' },
        { id: 'acc-2', name: 'workbuddy-2', provider: 'workbuddy', supports_checkin: true, checked_in_today: false, last_checkin_at: 0, last_outcome: '', last_message: '' },
      ],
    },
  ],
  schedules: [
    { id: 'sch-1', label: '每日 09:00', enabled: true, time: '09:00', providers: ['workbuddy'], last_fired_date: '', last_result: '' },
  ],
  today_summary: [{ provider: 'workbuddy', done: 1, total: 2 }],
};

// 签到台账：一次成功 + 一次「已签到」，用于验证记录 Tab 与结果着色
const CHECKIN_RECORDS = [
  { ts: 1761900000, date: '2025-10-31', account_id: 'acc-1', account_name: 'workbuddy-1', provider: 'workbuddy', source: 'manual', outcome: 'success', ok: true, message: '签到成功 +100' },
  { ts: 1761890000, date: '2025-10-31', account_id: 'acc-2', account_name: 'workbuddy-2', provider: 'workbuddy', source: 'schedule', outcome: 'already', ok: true, message: '今日已签到' },
];

// ── 报表端点（形状对齐 src/api/admin/dashboard.rs 的 report/*）──
const REPORT_HEATMAP = Array.from({ length: 30 }, (_, i) => ({
  date: `2025-10-${String(i + 1).padStart(2, '0')}`,
  cost: i % 5 === 0 ? 0 : i * 3,
  count: i % 5 === 0 ? 0 : i * 2,
}));
const REPORT_OVERVIEW = {
  total_requests: 120,
  success_requests: 118,
  success_rate: 98.3,
  input_tokens: 40000,
  output_tokens: 20000,
  total_cost: 1234,
  active_days: 21,
  top_models: [{ model: 'glm-4.7-flash', count: 90 }],
};


function sendJson(res, body, status = 200) {
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end(typeof body === 'string' ? body : JSON.stringify(body));
}


/** 聊天会话的内存态（E2E 每次启动清零）。
 *  预置两条带消息的会话：抽屉相关用例需要「多会话 + 可切换」的初始状态。 */
const chatSessions = new Map();
let chatSeq = 0;
{
  const base = Math.floor(Date.now() / 1000);
  for (let i = 0; i < 30; i++) {
    const id = `cs-seed-${i + 1}`;
    chatSessions.set(id, {
      id,
      title: `会话 ${i + 1}`,
      model: 'glm-4',
      created_at: base - i * 60,
      updated_at: base - i * 60,
      messages: [
        { role: 'user', content: `第 ${i + 1} 个会话的第 1 条消息` },
        { role: 'assistant', content: `第 ${i + 1} 个会话的第 2 条消息` },
      ],
    });
  }
  chatSeq = 0;
}


const server = http.createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const p = url.pathname;

  // 先把 body 收齐再分发：会话相关端点需要读 JSON（PUT messages / POST rename）。
  // E2E 的请求体都很小，一次性缓冲即可。
  let bodyText = '';
  req.on('data', (c) => { bodyText += c; });
  req.on('end', () => { handle(req, res, p, bodyText); });
});

function handle(req, res, p, bodyText) {

  if (p.startsWith('/api/') || p.startsWith('/v1/')) {
    // 少数需要具体形状的端点
    if (p === '/api/users/me') return sendJson(res, { success: true, data: ME_ADMIN });
    if (p === '/api/auth/logout') return sendJson(res, { success: true, data: null });
    if (p === '/api/models/available') return sendJson(res, { success: true, data: ['glm-4'] });
    if (p === '/api/network/status') return sendJson(res, { success: true, data: { enabled: true } });
    if (p === '/api/dashboard/realtime') return sendJson(res, { success: true, data: { window_secs: 300, total_requests: 0, qps: 0, avg_latency_ms: 0, error_rate: 0, errors: 0 } });

    // 统一模型管理
    if (p === '/api/models/manage') return sendJson(res, { success: true, data: MANAGE_VIEW });
    if (p === '/api/models/state') {
      // 启停后回读视图：不真的改状态，只验证请求能打通并返回同形数据
      return sendJson(res, { success: true, data: MANAGE_VIEW });
    }

    // 签到中心
    if (p === '/api/checkin-center') return sendJson(res, { success: true, data: CHECKIN_CENTER });
    if (p === '/api/checkin-center/records') {
      return sendJson(res, { success: true, data: { records: CHECKIN_RECORDS, total: CHECKIN_RECORDS.length } });
    }
    if (p === '/api/checkin-center/run') {
      return sendJson(res, {
        success: true,
        data: { total: 1, success: 1, already: 0, failed: 0, results: [{ account_id: 'acc-2', account_name: 'workbuddy-2', provider: 'workbuddy', success: true, outcome: 'success', message: '签到成功' }] },
      });
    }

    // ── 聊天会话（Chat 页已从 localStorage 迁到后端持久化）──
    // 用进程内 Map 维护状态，让「切换会话 / 重命名 / 删除」这类交互可被
    // E2E 真实驱动，而不是每次返回空数组。
    if (p === '/api/chat/sessions' && req.method === 'GET') {
      return sendJson(res, {
        success: true,
        data: [...chatSessions.values()].map(({ messages: _m, ...s }) => s),
      });
    }
    if (p === '/api/chat/sessions' && req.method === 'POST') {
      const id = `cs-${++chatSeq}`;
      const now = Math.floor(Date.now() / 1000);
      const session = { id, title: '新会话', model: '', created_at: now, updated_at: now, messages: [] };
      chatSessions.set(id, session);
      return sendJson(res, { success: true, data: { id, title: session.title, model: session.model } });
    }
    {
      const m = p.match(/^\/api\/chat\/sessions\/([^/]+)(\/rename|\/messages)?$/);
      if (m) {
        const sid = decodeURIComponent(m[1]);
        const sub = m[2];
        const s = chatSessions.get(sid);
        if (sub === '/messages' && req.method === 'PUT') {
          const body = JSON.parse(bodyText || '{}');
          if (s) {
            s.messages = body.messages ?? [];
            s.updated_at = Math.floor(Date.now() / 1000);
          }
          return sendJson(res, { success: true, data: { count: s?.messages.length ?? 0 } });
        }
        if (sub === '/rename' && req.method === 'POST') {
          const body = JSON.parse(bodyText || '{}');
          if (s) s.title = body.title ?? s.title;
          return sendJson(res, { success: true, data: null });
        }
        if (req.method === 'DELETE') {
          chatSessions.delete(sid);
          return sendJson(res, { success: true, data: null });
        }
        if (req.method === 'GET') {
          if (!s) return sendJson(res, { success: false, error: 'not found' }, 404);
          return sendJson(res, {
            success: true,
            data: {
              session: { id: s.id, title: s.title, model: s.model },
              messages: s.messages,
            },
          });
        }
      }
    }

    // 报表
    if (p === '/api/dashboard/report/heatmap') return sendJson(res, { success: true, data: REPORT_HEATMAP });
    if (p === '/api/dashboard/report/overview') return sendJson(res, { success: true, data: REPORT_OVERVIEW });
    if (p === '/api/dashboard/report/top_sources') {
      return sendJson(res, { success: true, data: [{ name: 'glm-4.7-flash', count: 90, cost: 800 }, { name: 'wb-coder', count: 30, cost: 434 }] });
    }
    if (p === '/api/dashboard/report/token_trend') {
      return sendJson(res, { success: true, data: [{ date: '2025-10-30', input_tokens: 1000, output_tokens: 500 }, { date: '2025-10-31', input_tokens: 2000, output_tokens: 900 }] });
    }
    if (p === '/api/dashboard/report/models') {
      return sendJson(res, { success: true, data: [{ model: 'glm-4.7-flash', count: 90 }] });
    }
    if (p === '/api/dashboard/report/cache_rates') {
      return sendJson(res, { success: true, data: [{ window: 'last24h', hit_tokens: 100, input_tokens: 500, rate: 20 }] });
    }
    if (p === '/api/dashboard/report/daily_trend') {
      return sendJson(res, { success: true, data: [{ date: '2025-10-31', count: 20, cost: 123 }] });
    }

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
}

server.listen(PORT, '127.0.0.1', () => {
  console.log(`[e2e] static server on http://127.0.0.1:${PORT} (root: ${ROOT})`);
});
