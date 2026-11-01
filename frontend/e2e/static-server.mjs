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
  // OAuth 绑定态（个人中心「第三方账号」卡片）：GitHub 已绑，Google/LinuxDO 未绑。
  // unbind 用例会改写这些布尔位；reset-oauth 钩子复位（beforeEach 调用）。
  oauth_github: true,
  oauth_google: false,
  oauth_linuxdo: false,
  password_set: true,
};
const ME_OAUTH_SEED = { oauth_github: true, oauth_google: false, oauth_linuxdo: false, password_set: true };

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
      aliases: ['alias-free', 'alias-off'],
      // 一条启用 + 一条关闭：锁住「关掉的映射仍显示但划线」的渲染
      alias_entries: [
        { alias: 'alias-free', target: 'unpriced-model', enabled: true },
        { alias: 'alias-off', target: 'unpriced-model', enabled: false },
      ],
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
  mappings: { 'alias-free': 'unpriced-model', 'alias-off': 'unpriced-model' },
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

// ── 请求日志（含对话快照，验证「不开调试模式也能看对话」）──
const REQUEST_LOGS = [
  {
    id: 'log-1',
    created_at: 1761900000,
    user_id: 'u-admin',
    user_email: 'admin@aigx.local',
    key_id: 'playground:chat',
    model: 'glm-4.7-flash',
    origin_model: 'glm-4.7-flash',
    channel_id: 'ch-1',
    channel_name: 'lxchat-glm',
    input_tokens: 120,
    output_tokens: 80,
    cost: 5,
    channel_cost: 3,
    latency_ms: 850,
    status_code: 200,
    ip: '203.0.113.10',
    // 关键：只有 conversation、没有 debug —— 证明对话不依赖 log_body 开关
    conversation: {
      messages: [
        { role: 'user', text: '帮我总结这段文本' },
        { role: 'assistant', text: '好的，要点如下……' },
      ],
      system_prompt_chars: 240,
    },
    // 路由回放：管理员可见的调度决策（候选/选中/被过滤及原因）
    candidate_channels: ['ch-1', 'ch-2', 'ch-3'],
    selected_channel: 'ch-1',
    filtered_channels: [
      { channel_id: 'ch-2', reason: 'circuit_open' },
      { channel_id: 'ch-3', reason: 'health_check_failed' },
    ],
  },
  {
    id: 'log-2',
    created_at: 1761899400,
    user_id: 'u-admin',
    model: 'glm-4.7-flash',
    input_tokens: 3000,
    output_tokens: 900,
    cost: 12,
    latency_ms: 2300,
    status_code: 200,
    conversation: {
      messages: [{ role: 'user', text: '最近一条' }],
      system_prompt_chars: 0,
      truncated_history: true,
    },
  },
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

/** 网络代理池内存态（E2E 可驱动 CRUD）。预置两条：一条启用 + 一条禁用。
 *  desktop/mobile 两个 project 串行共用本进程，CRUD 会跨 project 残留 ——
 *  `__e2e/reset-proxy-pool` 供用例 beforeEach 恢复预置态。 */
const PROXY_POOL_SEED = () => [
  { id: 'px_e2e_1', name: '香港节点', enabled: true, protocol: 'socks5', host: '127.0.0.1', port: 1080, username: '', has_password: false, usedBy: [] },
  { id: 'px_e2e_2', name: '日本节点', enabled: false, protocol: 'http', host: '127.0.0.1', port: 8080, username: 'user1', has_password: true, usedBy: [{ id: 'acc-1', name: 'workbuddy-1', enabled: true }] },
];
const proxyPool = PROXY_POOL_SEED();
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
    // ── OAuth 登录回跳 / 绑定 / 解绑（个人中心「第三方账号」+ 登录 ticket）──
    if (p === '/api/auth/oauth/consume' && req.method === 'POST') {
      // 任意 ticket 换一份合法登录数据（真实后端是一次性票据；E2E 不校验唯一性）
      return sendJson(res, {
        success: true,
        data: {
          token: 'e2e-oauth-token', email: ME_ADMIN.email, username: ME_ADMIN.username,
          role: 'admin', expires_at: Math.floor(Date.now() / 1000) + 86400, must_change_password: false,
        },
      });
    }
    if (/^\/api\/auth\/(github|google|linuxdo)\/bind\/start$/.test(p) && req.method === 'POST') {
      return sendJson(res, { success: true, data: { url: `https://example.com/oauth/authorize?state=e2e` } });
    }
    if (p === '/api/auth/oauth/unbind' && req.method === 'POST') {
      const body = JSON.parse(bodyText || '{}');
      const key = `oauth_${body.provider}`;
      if (key in ME_ADMIN) ME_ADMIN[key] = false;
      return sendJson(res, { success: true, data: { provider: body.provider } });
    }
    if (p === '/api/__e2e/reset-oauth' && req.method === 'POST') {
      Object.assign(ME_ADMIN, ME_OAUTH_SEED);
      return sendJson(res, { success: true, data: null });
    }
    if (p === '/api/models/available') return sendJson(res, { success: true, data: ['glm-4'] });
    if (p === '/api/network/status') return sendJson(res, { success: true, data: { enabled: true } });
    if (p === '/api/dashboard/realtime') return sendJson(res, { success: true, data: { window_secs: 300, total_requests: 0, qps: 0, avg_latency_ms: 0, error_rate: 0, errors: 0 } });

    // 统一模型管理
    if (p === '/api/models/manage') return sendJson(res, { success: true, data: MANAGE_VIEW });
    if (p === '/api/models/state') {
      // 启停后回读视图：不真的改状态，只验证请求能打通并返回同形数据
      return sendJson(res, { success: true, data: MANAGE_VIEW });
    }
    if (p === '/api/models/alias' || p === '/api/models/alias/state' || p === '/api/models/alias/remove') {
      // 写别名/切开关/删别名：同样回读视图（不真改），只锁「请求打到正确端点」
      return sendJson(res, { success: true, data: MANAGE_VIEW });
    }
    if (p === '/api/models/test') {
      // 模型测试：回显钉住的来源 + 固定结论，锁住「请求体形状 + 结果渲染」
      const body = JSON.parse(bodyText || '{}');
      return sendJson(res, {
        success: true,
        data: {
          ok: true,
          reply: '你好！我是测试回复。',
          reasoning: '',
          upstream_model: body.model || '',
          duration_ms: 812,
          ttfb_ms: 230,
          prompt_tokens: 2,
          completion_tokens: 9,
          total_tokens: 11,
          stream: body.stream !== false,
          source: {
            kind: body.account_id ? 'account' : 'channel',
            channel_id: body.channel_id || 'ch-3',
            name: body.account_id ? 'workbuddy-1' : 'lxchat-glm',
            account_id: body.account_id || null,
          },
        },
      });
    }

    // E2E 状态复位：恢复代理池预置态（跨 project 残留隔离）
    if (p === '/api/__e2e/reset-proxy-pool' && req.method === 'POST') {
      const fresh = PROXY_POOL_SEED();
      proxyPool.length = 0;
      proxyPool.push(...fresh);
      return sendJson(res, { success: true, items: proxyPool });
    }

    // 网络代理池（进程内数组，E2E 可驱动 CRUD）
    if (p === '/api/proxy-pool' && req.method === 'GET') {
      return sendJson(res, { success: true, items: proxyPool, total: proxyPool.length });
    }
    if (p === '/api/proxy-pool' && req.method === 'POST') {
      const body = JSON.parse(bodyText || '{}');
      const entry = {
        id: `px_e2e_${proxyPool.length + 1}`,
        name: body.name || '',
        enabled: body.enabled ?? true,
        protocol: body.protocol || 'http',
        host: body.host || '',
        port: body.port || 0,
        username: body.username || '',
        has_password: !!(body.password || ''),
      };
      proxyPool.unshift(entry);
      return sendJson(res, { success: true, items: proxyPool, total: proxyPool.length, entry });
    }
    if (p === '/api/proxy-pool/update') {
      const body = JSON.parse(bodyText || '{}');
      const idx = proxyPool.findIndex((e) => e.id === body.id);
      if (idx >= 0) {
        proxyPool[idx] = { ...proxyPool[idx], ...body, has_password: !!(body.password || proxyPool[idx].has_password) };
      }
      return sendJson(res, { success: true, items: proxyPool, total: proxyPool.length, entry: idx >= 0 ? proxyPool[idx] : null });
    }
    if (p === '/api/proxy-pool/enable') {
      const body = JSON.parse(bodyText || '{}');
      const entry = proxyPool.find((e) => e.id === body.id);
      if (entry) entry.enabled = body.enabled;
      return sendJson(res, { success: true, items: proxyPool, total: proxyPool.length, entry });
    }
    if (p === '/api/proxy-pool/remove') {
      const body = JSON.parse(bodyText || '{}');
      const idx = proxyPool.findIndex((e) => e.id === body.id);
      if (idx >= 0) proxyPool.splice(idx, 1);
      return sendJson(res, { success: true, items: proxyPool, total: proxyPool.length });
    }
    if (p === '/api/proxy-pool/test') {
      const body = JSON.parse(bodyText || '{}');
      const entry = proxyPool.find((e) => e.id === body.id);
      return sendJson(res, {
        success: true,
        status: 200,
        ip: '203.0.113.42',
        duration_ms: 120,
        error: null,
        name: entry?.name || '',
        items: proxyPool,
      });
    }
    if (p === '/api/proxy-pool/test-account') {
      return sendJson(res, { success: true, status: 200, ip: '203.0.113.42', duration_ms: 100, error: null, proxy: '' });
    }

    // 账号池（预置一条，供账号弹窗代理选择器用例用）
    if (p === '/api/account-sessions' && req.method === 'GET') {
      return sendJson(res, {
        success: true,
        data: [{
          id: 'acc-e2e-1', channel_id: 'ch-1', channel_name: 'lxchat-glm',
          name: 'e2e-账号', provider: 'openai_compatible', base_url: 'https://upstream.example.com',
          api_key: 'sk-***', proxy: 'pool:px_e2e_1', status: 'enabled', configured_status: 'enabled',
          priority: 0, weight: 1, max_concurrency: 3, models: [], extra_headers: {},
        }],
        total: 1,
      });
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

    // 请求日志（列表；详情弹窗在前端由同一份数据渲染）
    if (p === '/api/logs/requests' && req.method === 'GET') {
      return sendJson(res, {
        success: true,
        data: REQUEST_LOGS,
        total: REQUEST_LOGS.length,
        page: 1,
        size: 20,
      });
    }
    if (p === '/api/logs/requests/clear-preview' && req.method === 'GET') {
      return sendJson(res, {
        success: true,
        data: { days: 0, total_all: REQUEST_LOGS.length, would_remove: REQUEST_LOGS.length, oldest: 1761899400, newest: 1761900000 },
      });
    }
    if (p === '/api/logs/audits') {
      return sendJson(res, { success: true, data: [], total: 0, page: 1, size: 20 });
    }

    // 在途请求
    if (p === '/api/logs/inflight' && req.method === 'GET') {
      return sendJson(res, {
        success: true,
        total: 1,
        data: [{
          id: 'inflight-1', user_id: 'u-admin', key_id: 'playground:chat',
          model: 'glm-4.7-flash', channel_id: 'ch-1', channel_name: 'lxchat-glm',
          started_at: Math.floor(Date.now() / 1000) - 12, elapsed_ms: 12000, stream: true,
        }],
      });
    }
    if (p.startsWith('/api/logs/inflight/') && p.endsWith('/terminate')) {
      return sendJson(res, { success: true, data: null });
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
