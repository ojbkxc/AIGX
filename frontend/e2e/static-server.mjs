// E2E 用的静态服务器：托管 ../static（vite 构建产物）+ 最小 API 桩。
//
// 为什么不用 `vite preview`：E2E 要验证的是**入库的构建产物**（线上跑的就是
// static/），而不是 dev server 的即时编译。同时这里要拦截 /api/*，让测试不
// 依赖真实后端与数据库，可离线、可并发、无副作用。
//
// ── 桩分支可达性判据（#46）——改本文件前必读 ──
// R1 直打桩：每个写端点/新臂须有一条不经 page.route 的 page.request 直打用例钉它（被测 spec 自己 page.route 了同名端点 ⇒ 该桩臂未被覆盖、断的是副本）。
// R2 变异取证：把桩/实现还原成 buggy 版 ⇒ 对应用例必须变红，没变红即装饰。
// R3 不许桩自证：断契约形状/状态机语义，不断桩自己造的数字。
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

// ── 聊天设置（GET/PUT /api/chat/settings，#10/#11）──
// GET 响应 camelCase；PUT body snake_case（后端 ChatConfig serde），桩内互映射。
const CHAT_SETTINGS = { autoTitle: false, titleModel: '', titleChannel: '', titlePromptChars: 500 };

// ── 报表时区设置（GET/PUT /api/report/settings + POST /api/logs/agg/tz-migrate，#20 批3）──
// 三口径状态机，逐字段对齐真实产出方（对账基线 2026-11-09，tip `ee4ca78f` 实读）：
//   · `report_settings_json`（src/api/admin/settings.rs:626-637）——GET/PUT 响应的六个键；
//   · `handle_update_report_settings`（settings.rs:661-696）——PUT 保存即写进程快照＝热生效，
//     且**不碰** storedOffset（存量重算不在本端点职责内，需管理员显式调迁移端点）；
//   · `handle_agg_tz_migrate`（src/api/admin/logs.rs:695-750）+ `LogStore::tz_migrate`
//     （src/log/mod.rs:1870-1900）——幂等短路（标记已等于目标偏移 ⇒ 不删不回填）、
//     顺序坑（offset_to ≠ 当前快照 ⇒ 报错且零写入）、四条坑提示逐字回给调用方。
// 初始态镜像「升级后的老库尚未操作」：快照=缺省 8、标记=null（老库从没记过口径）⇒
// needsMigration 保守为 true。迁移窗口的 earliest 用三条存量聚合日（固定日期，恒早于
// 任何「昨天」）⇒ 首次迁移 window_applied=true、rows_deleted=3；第二次调用走幂等短路。
// 顺序坑（offset_to ≠ 当前快照）按**裁定后的契约**返 **400**（client 前置违例，与同类 range
// 检查 400、以及坑 1 notice 文案「否则本端点直接 400」一致；#10 裁定，勿回退成 500）。
// ⚠️ 非整数 body：axum `JsonDataError` ⇒ **422 + text/plain**（axum-0.7.9 rejection.rs:11-19，
//    `(status, String).into_response()`），不是 400 JSON——前端数字输入框产出带小数时真实后端
//    就是这个反应，桩照此建模（message 的 position 后缀省略，测试只钉前缀与状态码）。
const REPORT_TZ_SEED = { cfgOffset: 8, effOffset: 8, storedOffset: null };
let reportTz = { ...REPORT_TZ_SEED };
/** 存量聚合行样本日（迁移窗口的最早日判据；固定日期 ⇒ 断言确定） */
const REPORT_AGG_DAYS = ['2025-10-01', '2025-10-15', '2025-10-31'];

/** 报表日键（镜像 `daily_agg::date_key_at`：now 平移到目标偏移再取 UTC 日期） */
const reportDayKey = (offset, backDays = 0) => {
  const shifted = new Date(Date.now() + offset * 3600_000 - backDays * 86400_000);
  return shifted.toISOString().slice(0, 10);
};

/** 报表设置响应的规范形状（镜像 settings.rs:626-637；effectiveOffset 读进程快照） */
const reportSettingsJson = (cfgOffset, storedOffset) => ({
  timezoneOffset: cfgOffset,
  effectiveOffset: reportTz.effOffset,
  storedOffset,
  needsMigration: storedOffset !== reportTz.effOffset,
  minOffset: -12,
  maxOffset: 14,
});

/** serde_json 的 Unexpected 类型词与值展示（照 serde 的 unexpected.rs 逐型） */
const serdeTypeDesc = (v) => {
  if (typeof v === 'number') return `floating point \`${v}\``;
  if (typeof v === 'string') return `string "${v}"`;
  if (typeof v === 'boolean') return `boolean \`${v}\``;
  if (Array.isArray(v)) return 'sequence';
  return 'map';
};

/** 迁移响应的四条坑（逐字照抄 logs.rs:665-685 的 `tz_migrate_notices`） */
const tzMigrateNotices = (offsetTo, effectiveFrom, windowTo, today) => {
  const off = `${offsetTo >= 0 ? '+' : ''}${offsetTo}`;
  return [
    `顺序坑：目标偏移 UTC${off} 必须先在「设置 → 报表」保存并生效，否则本端点直接 400（重算用的口径必须与运行期写入口径同源，否则重启后配置回退、数据和运行期又错开）。`,
    `分裂窗口：升级到本次调用之间，报表是新旧两套口径混排——新到日志已按 UTC${off} 归日，存量聚合行仍停在旧口径。本次重算区间 [${effectiveFrom}, ${windowTo}] 闭合该窗口。`,
    `切换当日偏低、次日自愈：回填与迁移都不覆盖今天（${today} 归运行期记账），所以切换当日的报表只包含切换之后到达的请求。`,
    `先删后算：区间 [${effectiveFrom}, ${windowTo}] 内的旧口径聚合行是**先删除再回填**的。回填自带 shrink 守卫（既有行计数更大就保留不动），不先删就会把旧口径的大值原样留下、等于没迁；若 data.backfill.skipped_shrink 非 0，说明区间外还有被守卫拦住的行，需人工核对明细是否已被 retention 裁过。`,
  ];
};

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
      source: 'builtin',
      enabled: true,
      // SPEC-1.0.46 §2.1：default_enabled = 默认绑定（id→id）那颗开关；
      // enabled = default_enabled OR 任一条别名开着（后端 build_view 派生）。
      default_enabled: true,
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
      source: 'manual',
      // 默认绑定开着，且 alias-free 也开着 ⇒ 即便关掉默认绑定，行仍不压淡（A2A 口径）
      enabled: true,
      default_enabled: true,
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
      source: 'remote',
      enabled: true,
      default_enabled: true,
      sources: [{ kind: 'account', channel_id: 'ch-3', name: 'workbuddy-1', account_id: 'acc-1' }],
      owned_by: 'workbuddy',
      context_length: 64000,
      capabilities: ['tool'],
      price: { model_name: 'wb-coder', input_price: 0.0005, output_price: 0.001, price_type: 'token' },
      model_ratio: 1.2,
      aliases: [],
    },
  ],
  mappings: {
    'alias-free': 'unpriced-model',
    'alias-off': 'unpriced-model',
    // 悬空映射：目标模型不在 models 清单里（上游下线），悬空区应列出并可删
    'alias-dangling': 'gone-model',
  },
  unpriced: 1,
};

// ── 元信息覆盖（含 reasoning_levels 声明：思考等级下拉三级回退的回归源）──
const MODEL_META = {
  'glm-4.7-flash': {
    owned_by: 'zhipu',
    context_length: 128000,
    capabilities: ['chat', 'tool', 'vision'],
    // 该模型自己声明的档位（候选下拉优先用它，而不是兜底表 off~max）
    reasoning_levels: ['low', 'medium', 'high'],
  },
};

// ── 账号供应商目录（含 category 三值，形状对齐 src/account_session/registry.rs 的
// providers 端点；给添加账号五档弹窗回归用：agent/web/api 三形态各若干家）──
const ACCOUNT_PROVIDERS = [
  // agent 形态（登录态/cookie/JWT）
  { kind: 'workbuddy', label: 'WorkBuddy 国内版', protocol: 'session', default_base_url: 'https://www.example-wb.com', auth: 'cookie', credential_hint: '粘贴 Cookie', chat_path: '/api/chat', openai_compatible: false, native_supported: true, category: 'agent' },
  { kind: 'qoder', label: 'Qoder', protocol: 'session', default_base_url: 'https://www.qoder.com', auth: 'bearer', credential_hint: '粘贴 JWT', openai_compatible: false, native_supported: true, category: 'agent' },
  // web 形态（网页版，走 deepseek-web/chatglm 这类网页桥）
  { kind: 'deepseek-web', label: 'DeepSeek 网页版', protocol: 'web', default_base_url: 'https://chat.deepseek.com', auth: 'cookie', credential_hint: '粘贴登录 Cookie', openai_compatible: false, native_supported: true, category: 'web' },
  { kind: 'chatglm', label: '智谱清言', protocol: 'web', default_base_url: 'https://chatglm.cn', auth: 'cookie', credential_hint: '粘贴登录 Cookie', openai_compatible: false, native_supported: true, category: 'web' },
  // api 形态（API Key 型）
  { kind: 'openai_compatible', label: 'OpenAI 兼容', protocol: 'openai', default_base_url: 'https://api.openai.com/v1', auth: null, openai_compatible: true, native_supported: true, category: 'api' },
  { kind: 'anthropic_compatible', label: 'Anthropic 兼容', protocol: 'anthropic', default_base_url: 'https://api.anthropic.com', auth: null, openai_compatible: false, native_supported: true, category: 'api' },
];

// ── 账号池列表（分组折叠/有效期列/按到期排序回归用：两家 provider 各两条，
// 一条带凭证到期（未来 5 天）、一条带套餐到期（未来 2 小时）、一条已过期、一条无到期）──
const ACCOUNT_SESSIONS = [
  {
    id: 'acc-e2e-1', channel_id: 'ch-9', channel_name: 'lxchat-glm',
    name: 'e2e-账号', provider: 'openai_compatible', base_url: 'https://upstream.example.com',
    api_key: 'sk-***', proxy: 'pool:px_e2e_1', status: 'enabled', configured_status: 'enabled',
    priority: 0, weight: 1, max_concurrency: 3, models: [], extra_headers: {},
    // 5 天后到期（10 位秒 → 前端 expiryMillis 归一；取 5 天避开「N 天后」的取整边界）
    expires_at: Math.floor(Date.now() / 1000) + 5 * 86400,
  },
  {
    id: 'acc-e2e-2', channel_id: 'ch-8', channel_name: 'workbuddy-pool',
    name: 'wb-一号', provider: 'workbuddy', base_url: 'https://www.example-wb.com',
    api_key: 'ey***', proxy: '', status: 'enabled', configured_status: 'enabled',
    priority: 10, weight: 1, max_concurrency: 3, models: [], extra_headers: {},
    // 无凭证到期，但余额快照的套餐 2 小时后到期（13 位毫秒）+ 一份待生效套餐
    expires_at: null,
    last_balance: {
      available: 42, unit: '积分',
      wallets: [{ type: 'credits', displayName: '积分池', balance: 42, total: 100 }],
      subscription: { planName: '专业版', status: 'active', expireAt: Date.now() + 2 * 3600_000, remainQuota: 42, totalQuota: 100 },
      plans: [
        { name: '活动加赠', state: 'active' },
        { name: '周末礼包', state: 'pending', pendingUntil: Math.floor(Date.now() / 1000) + 86400 },
      ],
    },
  },
  {
    id: 'acc-e2e-3', channel_id: 'ch-8', channel_name: 'workbuddy-pool',
    name: 'wb-二号', provider: 'workbuddy', base_url: 'https://www.example-wb.com',
    api_key: 'ey***', proxy: '', status: 'disabled', configured_status: 'disabled',
    priority: 5, weight: 1, max_concurrency: 3, models: [], extra_headers: {},
    // 已过期（昨天）
    expires_at: Math.floor(Date.now() / 1000) - 86400,
  },
  {
    id: 'acc-e2e-4', channel_id: 'ch-9', channel_name: 'lxchat-glm',
    name: 'e2e-无到期', provider: 'openai_compatible', base_url: 'https://upstream.example.com',
    api_key: 'sk-***', proxy: '', status: 'enabled', configured_status: 'enabled',
    priority: 0, weight: 1, max_concurrency: 0, models: [], extra_headers: {},
  },
];

// ── 粘滞会话管理面桩（#38；形状对齐 admin/routing.rs sticky 端点）──
// id 内部编码 session+US 控制符+model（前端原样回传不解析）；进程内数组
// 可被清除写操作驱动；expires_at/updated_at 每次 GET 现算（unix 秒）。
let stickyMode = 'auto';
let stickyEntries = [
  { id: 'sess-e2e-1\u001fgpt-4o', channel_id: 'ch-1', channel_name: 'lxchat-glm' },
  { id: 'sess-e2e-2\u001fclaude-3', channel_id: 'ch-2', channel_name: 'workbuddy-pool' },
  { id: 'sess-e2e-3\u001fglm-4', channel_id: 'ch-1', channel_name: 'lxchat-glm' },
];

// ── 插件管理（/api/plugins 桩：一个已装包 + 目录商店两条，形状对齐 PluginsPayload；
// 「运行中」宿主 + 一家已加载/一家未加载，验证启停开关/提供商行/卸载确认/诊断区）──
const PLUGINS_PAYLOAD = {
  packs: [
    {
      id: 'aigx-provider-pack', name: '提供商增强包', description: '为网关扩展多家 AI 提供商的示例插件包。',
      version: '1.2.0', requiredVersion: null, updateRequired: false, compatibilityError: null,
      enabled: true, signed: true, installedAt: Math.floor(Date.now() / 1000) - 86400 * 9,
      entryPresent: true, protocol: 3, homepage: null, icon: null,
      providers: [
        { id: 'monkeyCode', label: 'MonkeyCode', enabled: true, loaded: true },
        { id: 'orca-term', label: 'OrcaTerm', enabled: false, loaded: false },
      ],
      host: { running: true, providers: ['monkeyCode'], crashes: 0, disabled: false, lastError: null },
      providerCategory: 'agent',
    },
  ],
  host: { running: true, providers: ['monkeyCode'], crashes: 0, disabled: false, lastError: null },
  adminPanels: [],
  accountFeatures: {},
  target: 'x86_64-unknown-linux-gnu',
  protocol: 3,
  pluginsDir: '/data/aigx/plugins',
  catalog: {
    available: true, current: 'v1', reason: null, error: false, checked: true, checking: false,
    checkedAt: Math.floor(Date.now() / 1000) - 600,
    plugins: [
      {
        id: 'aigx-provider-pack', name: '提供商增强包', description: '为网关扩展多家 AI 提供商。',
        official: true, version: '1.2.0', page: 'https://example.com/changelog', notes: null,
        prerelease: false, platforms: [{ target: 'x86_64-unknown-linux-gnu', size: 2097152, url: 'https://example.com/pack.zip', current: true }],
      },
      {
        id: 'aigx-web-pack', name: '网页提供商包', description: '网页形态提供商扩展（智谱清言等）。',
        official: true, version: '0.9.0', page: null, prerelease: true,
        platforms: [{ target: 'x86_64-unknown-linux-gnu', size: 1048576, url: 'https://example.com/web-pack.zip', current: true }],
        // 清单声明的 provider：缺失插件横幅据此把隐藏账号映射到可安装包
        providers: [{ id: 'web-glm', label: '智谱清言' }],
      },
    ],
  },
  updates: [
    { id: 'aigx-provider-pack', available: false, checked: true, current: '1.2.0', latest: '1.2.0', reason: null, error: false, asset: null },
  ],
  operations: [],
  // 本地安装上限（FileInstallDialog 用）
  maxPackBytes: 24 * 1024 * 1024,
  // provider → 账号数（批量条目账号计数）
  accountCounts: { monkeyCode: 1, 'orca-term': 1, 'web-glm': 2 },
  // 隐藏账号摘要：web-glm 的 2 个账号因插件未安装而隐藏（缺失插件横幅数据源）
  hiddenAccounts: {
    total: 2,
    byProvider: { 'web-glm': 2 },
    providers: [
      { id: 'web-glm', label: '智谱清言', count: 2, reason: 'not_installed', packId: null },
    ],
  },
};

/** 插件桩复位种子：桌面/移动两个 project 串行共用进程，写操作会跨 project 残留。
 *  注意必须克隆「初始字面量」而不是运行时的 PLUGINS_PAYLOAD（后者已被写操作改过），
 *  否则复位 = 把已改坏的状态再克隆一遍（自引用复位 bug）。 */
const PLUGINS_SEED = JSON.parse(JSON.stringify(PLUGINS_PAYLOAD));

// ── 签到中心快照（形状对齐 src/api/admin/checkin_center.rs）──
const CHECKIN_CENTER = {
  // A2A flat 契约：daily / extras / auto / keepalive / history（camelCase）。
  // 旧桩用的是 providers/today_summary/schedules，v1.0.71 签到改版后页面读不到
  // 任何数据（统计全 0、分组空），这批 e2e 因此整片红。
  daily: {
    providers: [
      {
        id: 'workbuddy',
        label: 'WorkBuddy 国内版',
        doneCount: 1,
        totalCount: 2,
        accounts: [
          { id: 'acc-1', name: 'workbuddy-1', available: true, checkinAt: 1761900000000, checkedInToday: true, edition: 'cn', streakDays: 3, lastOutcome: 'success', lastMessage: '签到成功 +100' },
          { id: 'acc-2', name: 'workbuddy-2', available: true, checkinAt: null, checkedInToday: false, edition: 'cn', streakDays: 0, lastOutcome: '', lastMessage: '' },
        ],
      },
    ],
    outOfScope: [],
    todayDone: 1,
    todayEligible: 2,
  },
  extras: { onboarding: [], plans: [], campaigns: [], growth: [] },
  auto: {
    enabled: true,
    running: false,
    providerOptions: [{ id: 'workbuddy', label: 'WorkBuddy 国内版' }],
    schedules: [
      {
        id: 'sch-1',
        label: '每日 09:00',
        displayLabel: '每日 09:00',
        enabled: true,
        time: '09:00',
        providers: ['workbuddy'],
        // 页面直接读 providerRows.length / providerTimes，缺数组会整页渲染异常
        providerTimes: {},
        providerRows: [
          { id: 'workbuddy', label: 'WorkBuddy 国内版', time: '09:00', firedToday: true, accountCount: 2 },
        ],
        retryTime: '',
        providerRetryTimes: {},
        retryGapMinutes: 0,
        retryLimit: 0,
        lastFiredDate: null,
        lastFiredToday: true,
        lastResult: null,
        retryExhausted: [],
      },
    ],
    time: '09:00',
    providers: ['workbuddy'],
    lastFiredDate: null,
    lastFiredToday: false,
    lastResult: null,
    nextRunAt: null,
  },
  keepalive: { models: [], default_models: [], defaultModels: [] },
  history: [],
  adminPanels: [],
  checkinFeatures: {},
  /* ── AIGX 扩展 ── */
  settings: { enabled: true, gap_ms: 500 },
  supportedProviders: ['workbuddy', 'workbuddy-intl'],
  unsupportedProviders: [],
  recordsTotal: 2,
};

// 签到台账：一次成功 + 一次「已签到」，用于验证记录 Tab 与结果着色。
// 时间必须是「今天」：页面默认时间筛选是「今天」，写死旧日期会让表格恒为空。
const __ckNow = Math.floor(Date.now() / 1000);
const __ckToday = new Date(__ckNow * 1000).toISOString().slice(0, 10);
const CHECKIN_RECORDS = [
  { ts: __ckNow - 60, date: __ckToday, account_id: 'acc-1', account_name: 'workbuddy-1', provider: 'workbuddy', source: 'manual', outcome: 'success', ok: true, message: '签到成功 +100' },
  { ts: __ckNow - 120, date: __ckToday, account_id: 'acc-2', account_name: 'workbuddy-2', provider: 'workbuddy', source: 'schedule', outcome: 'already', ok: true, message: '今日已签到' },
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
    // 重试列（后端 `RequestLog.attempts`，缺省 1；>1 才上 warning 色）。
    // 三条给 3 / 2 / 1：>1 的徽标与「一次就通」的次要色两条臂都能被断言到，
    // 而全 1 的桩会让「>1 上色」这条判据永远渲染不出来（假绿）。
    attempts: 3,
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
    attempts: 2,
    conversation: {
      messages: [{ role: 'user', text: '最近一条' }],
      system_prompt_chars: 0,
      truncated_history: true,
    },
  },
  {
    // 开了 log_body 的条目：详情弹窗应多出「原始报文」Tab。
    // 前两条故意不带 debug，用来锁「没有报文就不给空 Tab」的行为。
    id: 'log-3',
    created_at: 1761895800,
    user_id: 'u-admin',
    user_email: 'admin@aigx.local',
    model: 'glm-4.7-flash',
    channel_id: 'ch-1',
    channel_name: 'lxchat-glm',
    input_tokens: 10,
    output_tokens: 20,
    cost: 1,
    latency_ms: 400,
    status_code: 500,
    // 一次就通的那条：与上面两条 >1 的对照，锁「1 次不上警告色」的另一臂
    attempts: 1,
    error_msg: 'upstream 429',
    debug: {
      upstream_url: 'https://upstream.example.com/v1/chat/completions',
      request_body: '{"model":"glm-4.7-flash","messages":[{"role":"user","content":"hi"}]}',
      response_body: '{"error":{"message":"rate limited"}}',
    },
  },
];

// ── 渠道列表（形状对齐 src/api/admin/channel.rs 的 ChannelItem）──
// 此前 /api/channels 落到 EMPTY_LIST，渠道页在 E2E 里恒为「暂无渠道」空态，
// 表格与列设置面板一行都没被覆盖。这里给三条，覆盖启用/禁用、有密钥/无密钥、
// 有余额/无余额、有映射/全模型四种形态。
//
// #32 PR3 三键（`balance_query_interval_secs` / `balance_last_query_at` /
// `balance_last_error`）按 `mask_channel` 的口径**恒显式给出**（关就写 null，
// 不是省略键）——省略键与显式 null 在后端是同一个状态，桩里若省掉，前端「把
// undefined 与 null 分两种画法」的偏差就永远测不出来。四条样本铺开四种形态：
//   ch-1 = 定频开启（每 6 小时，有上次读数、无失败）
//   ch-2 = 显式 null（关闭）
//   ch-3 = 定频开启但上次巡检失败（带脱敏后的失败文案）
//   ch-4 = 显式 `0`（`Some(0)`）：PATCH 侧已归一成 None，但 `is_due`
//          （src/channel/balance_refresh.rs:34 的 `.filter(|v| *v > 0)` + 同文件
//          L170「显式 0 = 关闭」那条单测）仍然防它，说明这条形状**真能到前端**
//          （手写 config.toml / 旧版持久化数据）。画成「每 0 秒查一次」就是假信息。
const CHANNELS = [
  {
    id: 'ch-1',
    seq: 1,
    name: 'lxchat-glm',
    channel_type: 'openai_compatible',
    base_url: 'https://upstream.example.com/v1',
    api_key: 'sk-e2e-1',
    priority: 10,
    weight: 3,
    status: 'enabled',
    models: ['glm-4.7-flash', 'glm-4-plus'],
    model_mapping: { 'glm-4.7-flash': 'glm-4.7-flash-upstream' },
    balance: 12.5,
    credit: 3.2,
    balance_unit: 'USD',
    balance_scheme: 'openai_billing',
    // #32：每 6 小时巡检；上次读数固定给 2 天前（相对时间只断言前缀，不断言「2 天前」字样）
    balance_query_interval_secs: 21600,
    balance_last_query_at: 1761900000 - 2 * 86400,
    balance_last_error: null,
    response_time: 320,
    test_time: 1761900000,
    quirks: null,
    last_error: null,
  },
  {
    id: 'ch-2',
    seq: 2,
    name: 'cf-free-gateway',
    channel_type: 'cloudflare',
    base_url: 'https://cf-ai-gw.pages.dev',
    api_key: null,
    priority: 5,
    weight: 1,
    status: 'enabled',
    models: [],
    // #32：关闭态。键必须在、值为 null（与「这条数据比字段更早」同形）
    balance_query_interval_secs: null,
    balance_last_query_at: null,
    balance_last_error: null,
    response_time: 880,
    test_time: 1761896400,
    quirks: { headers: { 'X-E2E': '1' } },
    last_error: null,
  },
  {
    id: 'ch-3',
    seq: 3,
    name: 'anthropic-relay',
    channel_type: 'anthropic',
    base_url: 'https://relay.example.com',
    api_key: 'sk-ant-e2e',
    priority: 1,
    weight: 1,
    status: 'disabled',
    models: ['claude-sonnet-4-20250514'],
    balance: -1.5,
    balance_unit: 'USD',
    // #32：每 30 分钟巡检失败过一次。文案是后端 `mask_error_text` 处理后的形状
    // （`Bearer <token>` 已打码、上游正文片段原样保留），桩照形状给，不照理想给。
    balance_query_interval_secs: 86400,
    balance_last_query_at: 1761900000,
    balance_last_error: '上游返回 HTTP 401：{"error":"invalid Bearer *** key"}',
    response_time: 2400,
    test_time: 0,
    last_error: 'upstream 429',
  },
  {
    // #32 PR3：显式 `0` 那条。巡检侧 `is_due` 已把它当关闭（见上方注释），
    // 界面必须同样收成「未启用」，不能显示「每 0 秒」/「下次查询 = 刚刚」。
    // 上次读数留个非空值：只有这样才能测出「间隔已关但时间戳还在」时
    // 前端是不是还照着时间戳画一条「上次查询」——那种画法是自相矛盾的。
    id: 'ch-4',
    seq: 4,
    name: 'legacy-zero-interval',
    channel_type: 'openai_compatible',
    base_url: 'https://legacy.example.com/v1',
    api_key: 'sk-e2e-4',
    priority: 0,
    weight: 1,
    status: 'enabled',
    models: ['gpt-4o-mini'],
    balance: 0.4,
    balance_unit: 'USD',
    balance_scheme: 'openai_billing',
    balance_query_interval_secs: 0,
    balance_last_query_at: 1761900000,
    balance_last_error: null,
    response_time: 500,
    test_time: 1761900000,
    quirks: null,
    last_error: null,
  },
  {
    // account 型渠道（账号池容器）：添加账号弹窗「已建 API」档的池卡数据源
    id: 'ch-9',
    seq: 9,
    name: 'lxchat-glm',
    channel_type: 'account',
    base_url: '',
    api_key: null,
    priority: 0,
    weight: 1,
    status: 'enabled',
    models: [],
    response_time: 0,
    test_time: 0,
    last_error: null,
  },
  {
    // account 型渠道：workbuddy 账号池（登录态家的池；「已建 API」档按
    // provider category=api 过滤，这个池属 agent 家，不出现在该档）
    id: 'ch-8',
    seq: 8,
    name: 'workbuddy-pool',
    channel_type: 'account',
    base_url: 'https://www.example-wb.com',
    api_key: null,
    priority: 0,
    weight: 1,
    status: 'enabled',
    models: [],
    response_time: 0,
    test_time: 0,
    last_error: null,
  },
];

// ── 报表端点（形状对齐 src/api/admin/dashboard.rs 的 report/*）──
// 连续 365 天（UTC 日期字符串），贴合前端 days=365 的热力窗口
const REPORT_HEATMAP = Array.from({ length: 365 }, (_, i) => {
  const d = new Date(Date.UTC(2025, 10, 1) - (364 - i) * 86400_000);
  const date = d.toISOString().slice(0, 10);
  return {
    date,
    cost: i % 5 === 0 ? 0 : i * 3,
    count: i % 5 === 0 ? 0 : i * 2,
    tokens: i % 5 === 0 ? 0 : i * 1200,
  };
});
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

// ── 报表榜单桩的形状纪律（本轮补）────────────────────────────
// 榜单行数必须能**区分** `rank-controls` 的三档（前 5 / 前 10 / 全部），否则
// 「前 N 名」选择器在 E2E 里点了也没变化 —— 那是装饰而不是覆盖：
//   · top_sources 给 10 行（前 5 = 5 行、全部 = 10 行，两档可区分）；
//   · report/models 给 12 行（前 5 / 前 10 / 全部 = 5 / 10 / 12 三档互不相同）。
// 榜尾占比的分母必须是 overview.total_requests（120），不能是榜内合计：
//   top_sources 前 5 行请求数合计 74 → 74/120 = 61.7%（正确）vs 74/83 = 89.2%
//   （拿榜内合计当分母的错误实现）。桩**故意**让两者不同，断言才能锁住口径。
const REPORT_TOP_SOURCES = [
  { name: 'glm-4.7-flash', count: 30, cost: 800, tokens: 5000, success_rate: 98.3, avg_latency_ms: 820 },
  { name: 'wb-coder', count: 18, cost: 434, tokens: 3000, success_rate: 97.1, avg_latency_ms: 1100 },
  { name: 'claude-sonnet-4', count: 12, cost: 260, tokens: 1000, success_rate: 99.2, avg_latency_ms: 1600 },
  { name: 'gpt-4o-mini', count: 8, cost: 120, tokens: 500, success_rate: 96.4, avg_latency_ms: 640 },
  { name: 'deepseek-v3', count: 6, cost: 90, tokens: 300, success_rate: 99.9, avg_latency_ms: 520 },
  { name: 'kimi-k2', count: 4, cost: 60, tokens: 200, success_rate: 95.0, avg_latency_ms: 1400 },
  { name: 'qwen-max', count: 2, cost: 30, tokens: 100, success_rate: 99.0, avg_latency_ms: 900 },
  { name: 'gemini-flash', count: 1, cost: 15, tokens: 50, success_rate: 92.0, avg_latency_ms: 700 },
  { name: 'mistral-large', count: 1, cost: 10, tokens: 30, success_rate: 90.0, avg_latency_ms: 1500 },
  { name: 'local-ollama', count: 1, cost: 0, tokens: 20, success_rate: 100, avg_latency_ms: 4000 },
];

// 客户端榜：请求数与 Token 的**排序不同**（A 请求最多但 Token 最少），
// 「按请求数 / 按 Token」切换才真的能验出排序行为 —— 两者同序的桩会让
// 切换按钮点了也没变化。
const REPORT_TOP_CLIENTS = [
  { name: 'cursor', count: 50, cost: 300, tokens: 500, success_rate: 99.1, avg_latency_ms: 700 },
  { name: 'cline', count: 20, cost: 210, tokens: 9000, success_rate: 98.0, avg_latency_ms: 900 },
  { name: 'aider', count: 10, cost: 90, tokens: 3000, success_rate: 97.0, avg_latency_ms: 1200 },
  { name: 'codex-cli', count: 5, cost: 20, tokens: 100, success_rate: 95.5, avg_latency_ms: 1500 },
];

// 模型榜：12 行、token 合计 10000（占比 % 即 token/100），三档数量 + 饼图阈值
// 折叠都能从这一份数据里读出来：
//   · 前 5 / 前 10 / 全部 = 5 / 10 / 12 行；
//   · 饼图默认阈值 1% → 7 项（<100 token）并入「其他」→ 6 个扇区；
//   · 阈值 5% → 8 项并入「其他」→ 5 个扇区；关掉阈值 → 只剩 maxSlices(9) 兜底 → 9 个扇区。
const REPORT_MODELS = [
  { model: 'glm-4.7-flash', count: 60, input_tokens: 3500, output_tokens: 1500, cost: 500 },
  { model: 'wb-coder', count: 40, input_tokens: 2000, output_tokens: 1000, cost: 300 },
  { model: 'claude-sonnet-4', count: 20, input_tokens: 600, output_tokens: 300, cost: 120 },
  { model: 'gpt-4o-mini', count: 12, input_tokens: 400, output_tokens: 200, cost: 80 },
  { model: 'deepseek-v3', count: 8, input_tokens: 250, output_tokens: 100, cost: 40 },
  { model: 'kimi-k2', count: 5, input_tokens: 40, output_tokens: 20, cost: 12 },
  { model: 'qwen-max', count: 4, input_tokens: 30, output_tokens: 10, cost: 9 },
  { model: 'gemini-flash', count: 3, input_tokens: 15, output_tokens: 5, cost: 6 },
  { model: 'mistral-large', count: 2, input_tokens: 10, output_tokens: 5, cost: 4 },
  { model: 'local-ollama', count: 2, input_tokens: 5, output_tokens: 3, cost: 2 },
  { model: 'tinyllama', count: 1, input_tokens: 3, output_tokens: 2, cost: 1 },
  { model: 'phi-3', count: 1, input_tokens: 1, output_tokens: 1, cost: 1 },
];


function sendJson(res, body, status = 200) {
  res.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8' });
  res.end(typeof body === 'string' ? body : JSON.stringify(body));
}


// ── #22：流式 usage 的「两跳」桩（镜像后端 #21-A 的注入语义）─────────────────
//
// 为什么要两跳：此前 chat-sse-tool-calls.spec 的内联桩**无条件**吐 usage 终帧，
// 「界面出现 usage 数字」只证明桩自己的形状（#31 同型假绿）。真实链路里 usage
// 终帧是上游**只有在收到 `stream_options.include_usage` 时**才发的，而该字段由
// 后端注入出站请求体（legacy.rs `build_openai_chat_test_payload`）。e2e 里
// `ChannelChatTestRequest` 字段全显式无 flatten，前端也不发该字段，所以条件不能
// 取自「前端请求体」——只能把因果链本身照搬进来：
//
//   前端 ─▶ /api/channels/chat_test（后端双：重建出站 payload；stream 时注入
//           stream_options；上游 400 则去字段单次重试；SSE 原样透传）
//        ─▶ /mock-upstream/chat/completions（上游双：真 OpenAI 行为，
//           **只有**收到该字段才在 [DONE] 前吐 usage 终帧）
//
// 于是「后端双漏注入 / 上游漏条件」⇒ 界面无数字 ⇒ 断言红，桩不再自证。
// 用例通过 message 文本选上游档位：含 E2E_LEGACY_MARK 的模拟「不识别
// stream_options 的老 vLLM/自建网关」——收到该字段直接 400，被去字段重试后
// 正常流式但**不带** usage 终帧（正好钉住反例：数字不是前端自造的）；
// 含 E2E_ACCOUNT_MARK 的模拟「账号池归一化上游压根不回报 usage」——由后端双
// 按 #21-B 合成带 `estimated:true` 的 usage 尾帧（#24 角标用例的口径来源）。

/** 上游双吐的正文分片（拼起来是「两跳链路验证」） */
const E2E_UPSTREAM_WORDS = ['两跳', '链路', '验证'];
/** 账号池档的正文分片（拼起来是「账号池归一」）——与真值档可分辨，选错档不会撞绿 */
const E2E_ACCOUNT_WORDS = ['账号', '池', '归一'];
/** 老上游触发标记：出现在 messages 里即模拟不识别 stream_options 的网关 */
const E2E_LEGACY_MARK = '[e2e-upstream:legacy]';
/** 账号池触发标记：出现在 messages 里即模拟「上游压根不回报 usage」的归一化链路 */
const E2E_ACCOUNT_MARK = '[e2e-upstream:account]';
/** 慢速档标记：帧间隔从 60ms 拉到 500ms，给「中途点停止」留出能断言的窗口（#25） */
const E2E_SLOW_MARK = '[e2e-upstream:slow]';
/** 上游双的 usage 数字（固定值便于断言；total = prompt + completion） */
const E2E_UPSTREAM_USAGE = { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18 };
/** 后端双合成的估算数字（#21-B `usage_tail_frame()` 镜像），刻意与上游真值不同 */
const E2E_GATEWAY_USAGE = { prompt_tokens: 26, completion_tokens: 9, total_tokens: 35 };

const sseHeaders = {
  'Content-Type': 'text/event-stream; charset=utf-8',
  'Cache-Control': 'no-store',
  Connection: 'keep-alive',
};

/** 把一条 OpenAI chunk 对象序列化成 SSE 帧 */
const openaiChunkFrame = (model, extra) =>
  `data: ${JSON.stringify({
    id: 'chatcmpl-e2e', object: 'chat.completion.chunk', created: 1762000000, model, ...extra,
  })}\n\n`;

/** #21-B：后端合成的估算 usage 尾帧。`estimated` 挂在帧顶层（与 usage 同级） */
const gatewayUsageTailFrame = () =>
  `data: ${JSON.stringify({ choices: [], usage: E2E_GATEWAY_USAGE, estimated: true })}\n\n`;

/**
 * 上游双：OpenAI `/chat/completions` 的流式行为。
 *
 * 关键条件（去桩自证的本体）：`usage` 终帧**只在**请求带
 * `stream_options.include_usage === true` 时产出——与真实 OpenAI 一致。
 * 老上游档（messages 含 E2E_LEGACY_MARK）收到 stream_options 直接 400。
 * 慢速档（E2E_SLOW_MARK）把帧间隔拉到 500ms：给「中途点停止」用例留出
 * 可断言的窗口（默认 60ms 时 usage 尾帧约 300ms 就到，点停止必然抢在它后面，
 * 反向断言会因「帧未落地即成立」假绿）。
 */
function handleMockUpstream(res, bodyText) {
  let body = {};
  try {
    body = JSON.parse(bodyText || '{}');
  } catch {
    return sendJson(res, { error: { message: 'invalid json body' } }, 400);
  }
  const model = typeof body.model === 'string' ? body.model : 'e2e-model';
  const stream = body.stream === true;
  const wantsUsage = body.stream_options?.include_usage === true;
  const msgBlob = JSON.stringify(body.messages ?? '');
  const legacy = msgBlob.includes(E2E_LEGACY_MARK);
  // 账号池档：模拟非 OpenAI 原生的归一化上游——即便收到 stream_options 也**不回报** usage
  const account = msgBlob.includes(E2E_ACCOUNT_MARK);
  // 慢速档（#25）：帧间隔 500ms，让「点停止」能抢在 usage 尾帧之前
  const slow = msgBlob.includes(E2E_SLOW_MARK);
  const words = account ? E2E_ACCOUNT_WORDS : E2E_UPSTREAM_WORDS;

  if (legacy && body.stream_options !== undefined) {
    // 老 vLLM/自建网关：整字段不认识 → 400（后端双据此去字段重试一次）
    return sendJson(
      res,
      { error: { message: 'unknown field: stream_options', type: 'invalid_request_error' } },
      400,
    );
  }

  if (!stream) {
    // 非流式：OpenAI 恒带 usage（与 stream_options 无关），本单不依赖该分支
    return sendJson(res, {
      id: 'chatcmpl-e2e', object: 'chat.completion', model,
      choices: [{ index: 0, message: { role: 'assistant', content: words.join('') }, finish_reason: 'stop' }],
      usage: E2E_UPSTREAM_USAGE,
    });
  }

  const frames = [
    openaiChunkFrame(model, { choices: [{ index: 0, delta: { role: 'assistant' } }] }),
    ...words.map((w) => openaiChunkFrame(model, { choices: [{ index: 0, delta: { content: w } }] })),
    openaiChunkFrame(model, { choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] }),
  ];
  if (wantsUsage && !account) {
    frames.push(openaiChunkFrame(model, { choices: [], usage: E2E_UPSTREAM_USAGE }));
  }
  frames.push('data: [DONE]\n\n');

  res.writeHead(200, sseHeaders);
  let i = 0;
  const tick = () => {
    // 客户端 abort（点「停止生成」）后 res 已销毁：停掉泵，别继续写
    if (res.destroyed || res.writableEnded) return;
    if (i >= frames.length) return res.end();
    res.write(frames[i++]);
    setTimeout(tick, slow ? 500 : 60);
  };
  tick();
}

/**
 * 后端双：复刻 legacy.rs `handle_channel_chat_test` 的 OpenAI 支关键行为——
 * 从前端请求体**重建**出站 payload（#21-A：`stream` 时注入
 * `stream_options.include_usage`）、上游 400 时去字段单次重试、上游 SSE
 * 字节流原样透传给前端。
 *
 * 注意：这里的「注入」是 e2e 对 #21-A 的镜像。真正的注入由后端做（有 Rust
 * 单测钉住 payload builder），本桩的作用是：让前端 e2e 的 usage 断言依赖
 * 这条因果链，而不是依赖测试作者手写的帧。
 */
async function handleChatTestDouble(res, bodyText) {
  let body = {};
  try {
    body = JSON.parse(bodyText || '{}');
  } catch {
    return sendJson(res, { success: false, error: 'invalid json body' }, 400);
  }
  // 与后端 `#[serde(default = "default_true")] stream` 对齐：缺省即流式
  const stream = body.stream !== false;
  const history = Array.isArray(body.history) ? body.history : [];
  const messages = [...history, { role: 'user', content: body.message ?? '' }];
  const payload = {
    model: typeof body.model === 'string' && body.model ? body.model : 'glm-4',
    messages,
    stream,
  };
  if (stream) payload.stream_options = { include_usage: true };

  const callUpstream = (p) => fetch(`http://127.0.0.1:${PORT}/mock-upstream/chat/completions`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(p),
  });

  let up = await callUpstream(payload);
  if (up.status === 400 && stream && payload.stream_options) {
    // #21-A 的 400 兜底：个别老上游不识别该字段 → 去字段重试一次（一次往返）
    const stripped = { ...payload };
    delete stripped.stream_options;
    const retry = await callUpstream(stripped).catch(() => null);
    if (retry) {
      up = retry;
    }
  }
  if (!up.ok) {
    const text = await up.text().catch(() => '');
    return sendJson(res, { success: false, error: text || `上游失败 ${up.status}` }, 502);
  }
  res.writeHead(200, {
    'Content-Type': up.headers.get('content-type') || 'application/json; charset=utf-8',
    'Cache-Control': 'no-store',
  });
  // 逐块转发：保持「渐进」语义（用例断言首帧可见而末帧未出，靠这个）
  //
  // 账号池档（#21-B 镜像）：上游压根不回报 usage，由「后端双」在 `[DONE]`
  // **之前**合成 usage 尾帧，估算口径标记挂在**帧顶层**（与 usage 同级，
  // 不是 usage 内字段）。合成顺序照搬线上；注意前端对 `[DONE]` **之后**的帧
  // 也会解析（parseSseFrame 见 DONE 只置 ended 并 continue，不退出循环），
  // 所以「尾帧必须在 DONE 前」这条约束由 #21-B 的 Rust 单测钉住，本桩只保证
  // 帧形状与到达顺序同源。角标断言的因果落在「后端双合成 + 顶层键透传」。
  const account = JSON.stringify(messages).includes(E2E_ACCOUNT_MARK);
  const DONE_LINE = 'data: [DONE]';
  const dec = new TextDecoder();
  let pending = '';
  let synthesized = false;
  try {
    for await (const chunk of up.body) {
      if (res.destroyed) break;
      if (!account || !stream) {
        res.write(chunk);
        continue;
      }
      pending += typeof chunk === 'string' ? chunk : dec.decode(chunk, { stream: true });
      const at = pending.indexOf(DONE_LINE);
      if (at < 0) {
        // 滚动窗口：留住可能被切断的标记尾部，其余先转发（不牺牲渐进性）
        const keep = DONE_LINE.length - 1;
        if (pending.length > keep) {
          res.write(pending.slice(0, pending.length - keep));
          pending = pending.slice(pending.length - keep);
        }
        continue;
      }
      res.write(pending.slice(0, at) + gatewayUsageTailFrame() + pending.slice(at));
      pending = '';
      synthesized = true;
      break;
    }
    // 未到 `[DONE]` 就结束（客户端 abort 等）：只补发已缓冲字节，**不**合成尾帧。
    // 口径说明（be-api 2026-11-09 确认）：线上真因是断线时**响应体流本身被 drop**，
    // 网关就算合成也发不出去 ⇒ 前端永远观测不到尾帧，这条不变量只能由后端读码/单测钉。
    // ⚠️ 但「前端无数字」≠「不计费」：记账侧走 Drop 守卫兜底仍会落日志（报表 prompt
    // tokens 口径另议，见 47 号报告 §8）。本桩只镜像**界面可见**那一半。
    if (!synthesized && pending && !res.destroyed) res.write(pending);
    if (synthesized) await up.body.cancel().catch(() => undefined);
  } catch {
    // 客户端 abort：上游流被切断属正常
  }
  res.end();
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

/** API 令牌（GET /api/tokens）内存态。
 *
 *  形状按后端 `TokenListResp` 口径：**顶层**附 `providers`（provider 候选表）
 *  与 `modelsByProvider`（每家可用模型）；行内 `allowed_providers` 是白名单
 *  （空/null = 不限，与数据面 `channel_matches_allowed_providers` 同口径）。
 *
 *  三条种子各自承担一条判据：
 *   · tk-seed-1 白名单两家 → 表内「提供商」列两个 chip；
 *   · tk-seed-2 白名单 null → 该列显示「全部」（不是空白）；
 *   · tk-seed-3 白名单三家 → 触发「前 2 个 + +1」的折叠臂。
 *  provider 候选与 modelsByProvider 的模型**故意**包含 /api/models/available
 *  里没有的名字：否则「勾了提供商→模型候选收窄」与「全量候选」长得一样，
 *  断言分不出联动到底有没有生效。
 *
 *  ── 轮换 / 重置已用 / 每页条数（keys-ops.spec.ts）需要的三条扩展 ──
 *   · 每条令牌额外带 `_plain`（**只在桩内部**，列表响应里会被剥掉）：后端列表是
 *     脱敏的，明文只经 `GET /api/tokens/:id/key` 单条取回，`POST .../rotate` 一次性
 *     返回新明文。桩照此建模，而不是把明文塞进列表；
 *   · `TOKEN_FILLERS`（57 条）凑到 60 条 —— 只有超过「每页 50」才分得出默认档
 *     50 与 20（3 条时两种实现都只有 1 页，断言恒真）；
 *   · filler 全部 `status:'active'`、无 `quota_limit`：不参与白名单列的 +N 折叠，
 *     也不会把「重置已用」菜单项错误地渲染出来（该菜单项只在有 quota_limit 时出现）。 */
const TOKEN_FILLERS = Array.from({ length: 57 }, (_, i) => {
  const n = i + 1;
  return {
    id: `tk-fill-${n}`, name: `填充令牌 ${n}`, user_email: 'admin@aigx.local', group: 'default',
    status: 'active', key: `sk-e2e-****-f${String(n).padStart(3, '0')}`, allowed_providers: null,
    allowed_models: '', ip_limit: [], quota_limit: null, used_quota: n * 10,
    expires_at: null, created_at: 1761800000 - n * 60,
  };
});
const TOKENS_SEED = () => [
  {
    id: 'tk-seed-1', name: 'CrewAI 生产', user_email: 'admin@aigx.local', group: 'default',
    status: 'active', key: 'sk-e2e-****-aaaa', allowed_providers: ['openai', 'anthropic'],
    allowed_models: '', ip_limit: [], quota_limit: 500000, used_quota: 12000,
    expires_at: null, created_at: 1761900000, _plain: 'sk-e2e-plain-aaaa-0001',
  },
  {
    id: 'tk-seed-2', name: 'LiteLLM 网关', user_email: 'ops@aigx.local', group: 'default',
    status: 'active', key: 'sk-e2e-****-bbbb', allowed_providers: null,
    allowed_models: '', ip_limit: [], quota_limit: null, used_quota: 0,
    expires_at: null, created_at: 1761890000, _plain: 'sk-e2e-plain-bbbb-0002',
  },
  {
    id: 'tk-seed-3', name: '三方混跑', user_email: 'dev@aigx.local', group: 'default',
    status: 'disabled', key: 'sk-e2e-****-cccc',
    allowed_providers: ['openai', 'anthropic', 'google'],
    allowed_models: '', ip_limit: [], quota_limit: null, used_quota: 400,
    expires_at: null, created_at: 1761880000, _plain: 'sk-e2e-plain-cccc-0003',
  },
  ...TOKEN_FILLERS.map((tk) => ({ ...tk, _plain: `sk-e2e-plain-${tk.id}` })),
];
/** 列表响应里必须剥掉的内部字段（明文只走单条端点） */
const maskTokenRow = ({ _plain, ...row }) => row;
const TOKEN_PROVIDERS = ['anthropic', 'google', 'openai'];
const TOKEN_MODELS_BY_PROVIDER = {
  openai: ['gpt-4o', 'gpt-4o-mini'],
  anthropic: ['claude-sonnet-4'],
  google: ['gemini-2.0-flash'],
};
const tokensSeed = TOKENS_SEED();
/** 写侧留痕：POST / PUT 的最近一次请求体（spec 用它断「清空空数组真的发出去了」） */
let lastTokenWrite = null;

/** AI 运维会话（GET /api/agent/sessions）：进程内 Map，跨用例共享（与 chatSessions 同口径）。
 *  预置一条：抽屉里「有会话」这条臂需要它（全空时抽屉只显示空态）。 */
const agentSessions = new Map();
let agentSeq = 0;
{
  const base = Math.floor(Date.now() / 1000);
  agentSessions.set('ag-seed-1', {
    id: 'ag-seed-1', title: '检查渠道健康', role: 'operator', created_at: base - 120, messages: [],
  });
  agentSeq = 1;
}


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

  // #22：上游双——后端双的出站目标，故意不放在 /api 下（前端永不调用它）
  if (p === '/mock-upstream/chat/completions' && req.method === 'POST') {
    return handleMockUpstream(res, bodyText);
  }

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

    // 网关监听设置（API 密钥页「接入文档」Tab 管理员视角读）：
    // 通配地址 → 前端回退站点 origin（与生产通配口径一致）
    if (p === '/api/gateway/settings') {
      return sendJson(res, {
        success: true,
        data: { host: '0.0.0.0', port: 9527, corsOrigins: [], trustProxyHeaders: false },
      });
    }

    // 聊天设置（#10 契约：GET camel 出 / PUT snake 入；PUT 即热生效）
    if (p === '/api/chat/settings' && req.method === 'GET') {
      return sendJson(res, { success: true, data: CHAT_SETTINGS });
    }
    if (p === '/api/chat/settings' && req.method === 'PUT') {
      const body = JSON.parse(bodyText || '{}');
      if (typeof body.auto_title === 'boolean') CHAT_SETTINGS.autoTitle = body.auto_title;
      if (typeof body.title_model === 'string') CHAT_SETTINGS.titleModel = body.title_model;
      if (typeof body.title_channel === 'string') CHAT_SETTINGS.titleChannel = body.title_channel;
      if (Number.isFinite(body.title_prompt_chars)) CHAT_SETTINGS.titlePromptChars = body.title_prompt_chars;
      // 复刻后端唯一 400 组合：auto_title=true 且 title_prompt_chars=0
      if (CHAT_SETTINGS.autoTitle && CHAT_SETTINGS.titlePromptChars === 0) {
        return sendJson(res, { success: false, error: 'auto_title 开启时 title_prompt_chars 不能为 0' }, 400);
      }
      return sendJson(res, { success: true, data: CHAT_SETTINGS });
    }

    // ── 报表时区设置（#20 批3）──
    // GET/PUT /api/report/settings：形状逐字段对齐 settings.rs 的 report_settings_json
    // （:626-637）与 handle_update_report_settings（:661-696）；
    // POST /api/logs/agg/tz-migrate：形状对齐 logs.rs:695-750 + log/mod.rs:1870-1900。
    // 两处状态机语义：①PUT 保存即写快照（热生效），storedOffset 不动；
    // ②tz-migrate 幂等短路（标记已等于目标偏移 ⇒ 不删不回填，notices/tzEffectiveFrom 与首次一致）。
    // 非整数 body：axum JsonDataError ⇒ 422 + text/plain（不是 400 JSON，见上方状态机注释）。
    if (p === '/api/report/settings' && req.method === 'GET') {
      return sendJson(res, {
        success: true,
        data: reportSettingsJson(reportTz.cfgOffset, reportTz.storedOffset),
      });
    }
    if (p === '/api/report/settings' && req.method === 'PUT') {
      const body = JSON.parse(bodyText || '{}');
      const raw = body.timezone_offset ?? body.timezoneOffset;
      // null / 缺键 = 保持原值（与 Rust `Option<i32>` 的 serde 口径一致）
      if (raw !== null && raw !== undefined) {
        if (!Number.isInteger(raw)) {
          res.writeHead(422, { 'Content-Type': 'text/plain; charset=utf-8' });
          return res.end(
            `Failed to deserialize the JSON body into the target type: invalid type: ${serdeTypeDesc(raw)}, expected i32`,
          );
        }
        if (raw < -12 || raw > 14) {
          return sendJson(
            res,
            { success: false, error: '报表时区偏移必须在 -12~14 之间（整数小时，UTC 偏移）' },
            400,
          );
        }
        // 保存即热生效：配置文件值与进程快照同时写（storedOffset 不动）
        reportTz.cfgOffset = raw;
        reportTz.effOffset = raw;
      }
      return sendJson(res, {
        success: true,
        data: reportSettingsJson(reportTz.cfgOffset, reportTz.storedOffset),
      });
    }
    if (p === '/api/logs/agg/tz-migrate' && req.method === 'POST') {
      const body = JSON.parse(bodyText || '{}');
      const rawFrom = body.offset_from ?? body.offsetFrom;
      const rawTo = body.offset_to ?? body.offsetTo;
      // 缺省口径与后端一致：offset_to = 当前生效快照；offset_from = 0（生产现状 UTC+0）
      const offsetFrom = rawFrom === null || rawFrom === undefined ? 0 : rawFrom;
      const offsetTo = rawTo === null || rawTo === undefined ? reportTz.effOffset : rawTo;
      for (const [v, label] of [
        [offsetFrom, '旧口径 offset_from'],
        [offsetTo, '新口径 offset_to'],
      ]) {
        if (!Number.isInteger(v)) {
          res.writeHead(422, { 'Content-Type': 'text/plain; charset=utf-8' });
          return res.end(
            `Failed to deserialize the JSON body into the target type: invalid type: ${serdeTypeDesc(v)}, expected i32`,
          );
        }
        if (v < -12 || v > 14) {
          return sendJson(
            res,
            { success: false, error: `${label}=${v} 越界：必须在 -12~14 之间（整数小时，UTC 偏移）` },
            400,
          );
        }
      }
      // 第 0 步（顺序坑）：目标偏移必须等于当前生效口径 ⇒ client 前置违例返 400
      //（#10 裁定：与同类 range 检查 400、notice「否则本端点直接 400」一致；勿回退成 500）
      if (offsetTo !== reportTz.effOffset) {
        return sendJson(
          res,
          {
            success: false,
            error: `Agg tz migrate failed: 目标偏移 ${offsetTo} 与当前生效口径 ${reportTz.effOffset} 不一致：请先在报表设置里保存偏移，再调迁移端点`,
          },
          400,
        );
      }
      const today = reportDayKey(reportTz.effOffset);
      const windowTo = reportDayKey(reportTz.effOffset, 1);
      const earliest = REPORT_AGG_DAYS.length > 0 ? REPORT_AGG_DAYS[0] : today;
      const report = {
        effective_from: earliest,
        window_to: windowTo,
        window_applied: false,
        rows_deleted: 0,
        backfill: { scanned: 0, days_written: 0, user_rows_written: 0, skipped_shrink: 0 },
      };
      // 幂等短路：标记已等于目标偏移（老库 null 时以调用方声明的旧口径兜底）⇒ 不删不回填
      const stored = reportTz.storedOffset ?? offsetFrom;
      if (stored === offsetTo) {
        return sendJson(res, {
          success: true,
          data: report,
          tzEffectiveFrom: report.effective_from,
          today,
          notices: tzMigrateNotices(offsetTo, report.effective_from, windowTo, today),
        });
      }
      // 删窗口 [最早聚合日, 昨天]（先删后算）+ 回填 + 落标记
      if (earliest <= windowTo) {
        report.window_applied = true;
        report.rows_deleted = REPORT_AGG_DAYS.length;
      }
      report.backfill = {
        scanned: 128,
        days_written: REPORT_AGG_DAYS.length,
        user_rows_written: 6,
        skipped_shrink: 0,
      };
      reportTz.storedOffset = offsetTo;
      return sendJson(res, {
        success: true,
        data: report,
        tzEffectiveFrom: report.effective_from,
        today,
        notices: tzMigrateNotices(offsetTo, report.effective_from, windowTo, today),
      });
    }

    // E2E 状态复位：恢复报表时区桩预置态（写用例跨 project / 本地 reuseExistingServer 残留隔离）
    if (p === '/api/__e2e/reset-report-tz' && req.method === 'POST') {
      reportTz = { ...REPORT_TZ_SEED };
      return sendJson(res, { success: true, data: null });
    }

    // #22：chat_test 后端双（两跳链路入口，此前该路径落到 EMPTY_LIST 兜底）
    if (p === '/api/channels/chat_test' && req.method === 'POST') {
      return handleChatTestDouble(res, bodyText);
    }

    // 渠道健康 Tab（原系统诊断页并入）：三件套形状对齐 src/api/admin/diagnostics.rs
    if (p === '/api/diagnostics/summary') {
      return sendJson(res, {
        success: true,
        data: {
          generated_at: 1761900000,
          channels: { total: 3, enabled: 2, circuit_open: 1, circuit_halfopen: 0, cooldown: 0 },
          today_usage: { requests: 42, success: 40, success_rate: 95.2, input_tokens: 3000, output_tokens: 900 },
          errors_24h: { total_failures: 2, top: [{ status_code: 502, count: 2, error_msg: 'upstream connect failed' }] },
          runtime: { version: '1.0.34-e2e', pid: 4321, uptime_secs: 3720, uptime_available: true },
        },
      });
    }
    if (p === '/api/diagnostics/breakers') {
      return sendJson(res, {
        success: true,
        data: {
          tracked: 1, open: 1, halfopen: 0,
          breakers: [{
            channel_id: 'ch-e2e-1', state: 'open', failure_count: 3, failure_type: 'upstream_5xx',
            cooldown_remaining_secs: 45, rate_limit_remaining_secs: 0, probe_in_flight: false, in_store_cooldown: false,
          }],
        },
      });
    }
    if (p === '/api/diagnostics/channels') {
      return sendJson(res, {
        success: true,
        data: {
          probed: 2, alive: 1, deadline_secs: 10,
          channels: [
            { channel_id: 'ch-e2e-1', name: 'e2e-openai', enabled: true, result: 'alive', latency_ms: 120, message: '' },
            { channel_id: 'ch-e2e-2', name: 'e2e-anthropic', enabled: true, result: 'dead', latency_ms: 0, message: 'connect refused' },
          ],
        },
      });
    }

    // 渠道列表（渠道管理页）：返回三条样本，覆盖启用/禁用、有/无密钥、有/无余额、有/无模型映射
    if (p === '/api/channels' && req.method === 'GET') {
      return sendJson(res, { success: true, data: CHANNELS, total: CHANNELS.length, page: 1, size: CHANNELS.length });
    }

    // #32 PR3 渠道写端点（PUT / PATCH / POST），照后端真实语义写。
    // 形状来源（**已于 `1a51d6f0` 落在 main**，2026-11-09 现跑 `git show origin/main:…` 对账）：
    //   · `ChannelRequest`（POST/PUT body，src/api/admin/channels.rs）**不含**
    //     `balance_query_interval_secs` ⇒ PUT 带上也会被 serde 丢弃，PATCH 是这条配置的唯一写路径；
    //   · PUT 走 `preserve_non_form_fields`（channels.rs:633，调用点 :673）把三键从已存渠道
    //     搬回来 ⇒ 保存一次表单**不该**把巡检关掉；
    //   · PATCH 的三态（`apply_interval_patch`，legacy.rs:1296）：键不存在 = 保持原值；
    //     显式 null 或 0 = 关闭；**非负整数**封顶 30 天；其余形状（字符串/负数/小数/对象）
    //     = **忽略不改动**（后端沿用 `api_key` 臂的既有口径，不新增 400）。运行态两键
    //     不接受 PATCH 写入（写了是造假）。
    // 本桩**只回显不改写** `CHANNELS`：进程被全部 spec 共用（desktop/mobile 两轮串跑，
    // 本地还 reuseExistingServer），写坏一条会把后续用例的显示态带偏
    // （同一个坑见 proxies.spec.ts 的复位接口注释）。需要「保存后回读真的变了」的用例
    // 在自己内部建 per-test 副本 —— 见 channels-balance-refresh.spec.ts 的 useChannelStore。
    const channelById = (id) => CHANNELS.find((c) => c.id === id);
    if (p === '/api/channels' && req.method === 'POST') {
      // 新建：后端 `to_channel` 恒按「不参与巡检」起步（opt-in 默认），桩照此返回。
      // id 固定串即可：前端只看 `data.id`（决定是否补那一刀 PATCH），不消费列表里的新行。
      // 桩里没有这条持久行 ⇒ 随后 PATCH ch-new 会 404：要测「新建 + 设间隔」请在 spec
      // 内建副本（useChannelStore 同款），别把桩改成能持久化（跨 spec 污染）。
      return sendJson(res, {
        success: true,
        data: {
          id: 'ch-new',
          balance_query_interval_secs: null,
          balance_last_query_at: null,
          balance_last_error: null,
        },
      });
    }
    // ⚠️ id 用 exec 取捕获组，别 `p.split('/')[2]`：`/api/channels/ch-1` 切出来
    // [0]=''、[1]='api'、[2]='channels'、[3]='ch-1' —— 取 [2] 永远查不到渠道、恒 404。
    // 写路径用例都跑在 spec 内的 per-test 副本上（见下方注释），所以这条分支
    // 没有用例经过，写错也不会红 —— 由 channels-balance-refresh.spec.ts 的
    // 「直打桩的 PATCH」那条用例钉住可达性 + 只回显不改写。
    const chHit = /^\/api\/channels\/([^/]+)$/.exec(p);
    if (chHit && (req.method === 'PUT' || req.method === 'PATCH')) {
      const id = decodeURIComponent(chHit[1]);
      const body = JSON.parse(bodyText || '{}');
      const stored = channelById(id);
      if (!stored) return sendJson(res, { success: false, error: { message: 'Channel not found' } }, 404);
      if (req.method === 'PUT') {
        // 三键原样保留（PUT body 带不带都一样：`ChannelRequest` 没这个字段）
        return sendJson(res, {
          success: true,
          data: {
            ...stored, ...body,
            balance_query_interval_secs: stored.balance_query_interval_secs ?? null,
            balance_last_query_at: stored.balance_last_query_at ?? null,
            balance_last_error: stored.balance_last_error ?? null,
          },
        });
      }
      // PATCH：巡检间隔按后端三态归一（`apply_interval_patch`）；运行态两键**不接受写入**
      // （body 里出现也只回显已存值，写了是造假）。其余字段（status / priority / weight
      // 这类内联编辑）照 body 回显，让「PATCH 一个字段」的调用方拿到自洽的响应。
      const keptInterval = stored.balance_query_interval_secs ?? null;
      const nextInterval = Object.prototype.hasOwnProperty.call(
        body,
        'balance_query_interval_secs',
      )
        ? (() => {
            const raw = body.balance_query_interval_secs;
            if (raw === null) return null;
            // 只认非负整数：`as_u64` 取不到值（字符串 / 负数 / 小数 / 对象）时后端**不改**，
            // 收成关闭就是把「一次没生效的 PATCH」显示成「关掉了」——另一种假信息
            if (!Number.isInteger(raw) || raw < 0) return keptInterval;
            return raw === 0 ? null : Math.min(raw, 30 * 86400);
          })()
        : keptInterval;
      return sendJson(res, {
        success: true,
        data: {
          ...stored,
          ...body,
          balance_query_interval_secs: nextInterval,
          balance_last_query_at: stored.balance_last_query_at ?? null,
          balance_last_error: stored.balance_last_error ?? null,
        },
      });
    }

    // 统一模型管理
    if (p === '/api/models/manage') return sendJson(res, { success: true, data: MANAGE_VIEW });
    // 元信息覆盖表（含 reasoning_levels 声明，供思考等级下拉候选回退）
    if (p === '/api/models/meta') return sendJson(res, { success: true, data: MODEL_META });
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

    // E2E 状态复位：恢复插件桩预置态（启停/卸载/安装写操作会改内存态）
    if (p === '/api/__e2e/reset-plugins' && req.method === 'POST') {
      PLUGINS_PAYLOAD.packs = JSON.parse(JSON.stringify(PLUGINS_SEED.packs));
      PLUGINS_PAYLOAD.host = JSON.parse(JSON.stringify(PLUGINS_SEED.host));
      PLUGINS_PAYLOAD.catalog = JSON.parse(JSON.stringify(PLUGINS_SEED.catalog));
      PLUGINS_PAYLOAD.updates = JSON.parse(JSON.stringify(PLUGINS_SEED.updates));
      PLUGINS_PAYLOAD.operations = JSON.parse(JSON.stringify(PLUGINS_SEED.operations));
      PLUGINS_PAYLOAD.accountCounts = JSON.parse(JSON.stringify(PLUGINS_SEED.accountCounts));
      PLUGINS_PAYLOAD.hiddenAccounts = JSON.parse(JSON.stringify(PLUGINS_SEED.hiddenAccounts));
      return sendJson(res, { success: true, data: null });
    }

    // 粘滞会话管理面（#38；形状对齐 admin/routing.rs handle_get_route_sticky /
    // handle_clear_route_sticky）。query 对 id/渠道名/channel_id 小写包含过滤。
    if (p === '/api/routing/sticky' && req.method === 'GET') {
      const q = (new URL(req.url, 'http://x').searchParams.get('query') || '').trim().toLowerCase();
      const now = Math.floor(Date.now() / 1000);
      const filtered = stickyEntries.filter((e) =>
        q === '' ||
        e.id.toLowerCase().includes(q) ||
        (e.channel_name || '').toLowerCase().includes(q) ||
        (e.channel_id || '').toLowerCase().includes(q));
      return sendJson(res, {
        success: true,
        data: {
          entries: filtered.map((e) => ({ ...e, expires_at: now + 1500, updated_at: now - 300 })),
          total: filtered.length,
          truncated: false,
          now,
          sticky: stickyMode,
        },
      });
    }
    if (p === '/api/routing/sticky/clear' && req.method === 'POST') {
      const body = JSON.parse(bodyText || '{}');
      let cleared = 0;
      if (typeof body.id === 'string' && body.id.trim()) {
        const before = stickyEntries.length;
        stickyEntries = stickyEntries.filter((e) => e.id !== body.id);
        cleared = before - stickyEntries.length;
      } else if (body.all === true) {
        cleared = stickyEntries.length;
        stickyEntries = [];
      }
      return sendJson(res, { success: true, data: { cleared } });
    }

    // 错误处理策略（GET/PUT /api/routing/errors）—— 路由页「错误处理」ErrorsCard 读写。
    // 形状对齐 ErrorPolicyView（RoutingErrorsPanel.tsx:20-29）。缺桩会兜底 EMPTY_LIST
    // （data=[]），ErrorsCard adopt 取 [].settings=undefined → draft undefined →
    // 渲染访问 draft[f.key] 崩，拖垮整张卡（含 RetryTimeoutCard）→ e2e 字段全找不到。
    if (p === '/api/routing/errors' && (req.method === 'GET' || req.method === 'PUT')) {
      // rules 必须非空：simulateError（RoutingErrorsFeatures.tsx:283）在 rules 空时
      // tier3=settings.rules[0]=undefined → rule=undefined → 读 rule.action 崩。
      // 真实后端恒返回完整默认规则集，桩照此造全套 9 kind。
      const errRule = (kind, statuses, code, action, cooldown, scope = 'auto', fallbackSeconds = 60) => ({
        id: kind, name: kind, kind, statuses, code, provider: '*', model: '',
        enabled: true, action, cooldown, fallbackSeconds, exponential: false,
        fallbackCapSeconds: 600, scope,
      });
      const errorRules = [
        errRule('rate_limit', [429], 'rate_limit_error', 'switch_account', 'upstream'),
        errRule('quota', [429], 'usage_limit_reached', 'switch_account', 'upstream', 'account'),
        errRule('auth', [401], 'token_expired', 'refresh_then_switch', 'fixed', 'account', 0),
        errRule('balance', [402], 'insufficient_balance', 'switch_account', 'fixed', 'account', 0),
        errRule('transient', [500, 502, 503, 504], 'service_unavailable', 'retry_then_switch', 'protection'),
        errRule('unsupported', [501], 'unsupported', 'switch_provider', 'fixed', 'provider', 0),
        errRule('bad_request', [400, 413], '', 'return_error', 'none'),
        errRule('content', [400], 'content_policy_violation', 'return_error', 'none'),
        errRule('unknown', [418], 'unknown_upstream_error', 'switch_account', 'fixed'),
      ];
      const errorSettings = {
        resends: 1, switches: 3, intervalSeconds: 0, waitBudgetSeconds: 30,
        cooldownFloorSeconds: 5, backoff: true, backoffSeconds: 30, backoffCapSeconds: 300,
        sink: true, sinkSeconds: 60, breaker: true, breakerFailures: 3, breakerSeconds: 120,
        breakerSuccesses: 2, rules: errorRules, overrides: [],
      };
      return sendJson(res, {
        success: true,
        data: {
          settings: errorSettings, defaults: { ...errorSettings }, revision: '1',
          configured: true, legacy: false,
          providers: [{ id: '*', label: '全部提供商' }], providerRules: [],
        },
      });
    }
    // 重试 / 超时（GET/PUT /api/limits）—— 路由页「错误处理」RetryTimeoutCard 读写。
    if (p === '/api/limits' && (req.method === 'GET' || req.method === 'PUT')) {
      return sendJson(res, {
        success: true,
        data: {
          max_retries: 2, retry_self_count: 1, retry_interval_ms: 0, retry_on_status: [402],
          api_timeout_secs: 300, connect_timeout_secs: 10, headers_timeout_secs: 60,
          stream_idle_secs: 30, body_timeout_secs: 300,
        },
      });
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

    // 账号池（分组折叠/有效期列回归用：两家 provider 四条，含到期/已过期/待生效）
    if (p === '/api/account-sessions' && req.method === 'GET') {
      return sendJson(res, { success: true, data: ACCOUNT_SESSIONS, total: ACCOUNT_SESSIONS.length });
    }

    // 供应商目录（含 category 三值：agent/web/api —— 添加账号五档弹窗回归用）
    if (p === '/api/account-sessions/providers') {
      return sendJson(res, { success: true, data: ACCOUNT_PROVIDERS, total: ACCOUNT_PROVIDERS.length });
    }

    // 插件管理（契约对齐 Agent2API api/plugins.rs；写端点回读同一份桩）
    if (p === '/api/plugins' && req.method === 'GET') {
      return sendJson(res, { success: true, data: PLUGINS_PAYLOAD });
    }
    if (/^\/api\/plugins\/(enable|uninstall|provider)$/.test(p) && req.method === 'POST') {
      // 最小可动写桩：真改内存态，让启停/卸载用例能断言列表回读后的变化
      const body = JSON.parse(bodyText || '{}');
      if (p === '/api/plugins/enable') {
        const pack = PLUGINS_PAYLOAD.packs.find((pk) => pk.id === body.id);
        if (pack) pack.enabled = body.enabled !== false;
      } else if (p === '/api/plugins/provider') {
        const pack = PLUGINS_PAYLOAD.packs.find((pk) => pk.id === body.id);
        const provider = pack?.providers.find((pv) => pv.id === body.provider);
        if (provider) provider.enabled = body.enabled !== false;
      } else if (p === '/api/plugins/uninstall') {
        const idx = PLUGINS_PAYLOAD.packs.findIndex((pk) => pk.id === body.id);
        if (idx >= 0) PLUGINS_PAYLOAD.packs.splice(idx, 1);
      }
      return sendJson(res, { success: true, data: { packs: PLUGINS_PAYLOAD.packs, host: PLUGINS_PAYLOAD.host } });
    }
    // 在线安装（下载）：把商店条目装进 packs 并从目录里摘掉（模拟真实安装链路）
    if (p === '/api/plugins/install' && req.method === 'POST') {
      const body = JSON.parse(bodyText || '{}');
      const entry = PLUGINS_PAYLOAD.catalog.plugins.find((it) =>
        it.platforms.some((pf) => pf.url === body.url));
      if (!entry) return sendJson(res, { success: false, error: '未知安装地址' }, 400);
      const installedPack = {
        id: entry.id, name: entry.name, description: entry.description,
        version: entry.version || '0.0.0', requiredVersion: null, updateRequired: false,
        compatibilityError: null, enabled: true, signed: false,
        installedAt: Math.floor(Date.now() / 1000), entryPresent: true, protocol: 3,
        homepage: entry.homepage ?? null, icon: entry.icon ?? null,
        providers: [], host: { running: false, providers: [], crashes: 0, disabled: false, lastError: null },
      };
      PLUGINS_PAYLOAD.packs.push(installedPack);
      PLUGINS_PAYLOAD.catalog.plugins = PLUGINS_PAYLOAD.catalog.plugins.filter((it) => it.id !== entry.id);
      return sendJson(res, { success: true, data: { pack: installedPack } });
    }
    // 从发布目录后台安装/更新（前端单包下载走这里）：桩同步完成安装并直接回
    // 一个已完成 Operation（页面随后回读列表即可看到新包与 toast）。
    if (p === '/api/plugins/install-from-release' && req.method === 'POST') {
      const body = JSON.parse(bodyText || '{}');
      const entry = PLUGINS_PAYLOAD.catalog.plugins.find((it) => it.id === body.id);
      if (!entry) return sendJson(res, { success: false, error: '发布目录里没有这个插件包' }, 404);
      const installedPack = {
        id: entry.id, name: entry.name, description: entry.description,
        version: entry.version || '0.0.0', requiredVersion: null, updateRequired: false,
        compatibilityError: null, enabled: true, signed: false,
        installedAt: Math.floor(Date.now() / 1000), entryPresent: true, protocol: 3,
        homepage: entry.homepage ?? null, icon: entry.icon ?? null,
        providers: [], host: { running: false, providers: [], crashes: 0, disabled: false, lastError: null },
      };
      PLUGINS_PAYLOAD.packs.push(installedPack);
      PLUGINS_PAYLOAD.catalog.plugins = PLUGINS_PAYLOAD.catalog.plugins.filter((it) => it.id !== entry.id);
      return sendJson(res, {
        success: true,
        data: {
          operation: {
            id: entry.id, action: body.action || 'install', stage: 'done', active: false, done: true,
            received: 0, total: 0, percent: 100, version: installedPack.version, updatedAt: Date.now(),
          },
        },
      });
    }
    if (p === '/api/plugins/update' || p === '/api/plugins/release') {
      return sendJson(res, { success: true, data: { error: false, reason: null } });
    }
    // 本 spec 不覆盖脚本插件：显式 404 让页面「Magpie 插件」源如实显示不可用
    // （否则落到 EMPTY_LIST 兜底会被误判为「脚本后端已就绪」）。
    if (p === '/api/plugins/scripts' || p === '/api/plugins/scripts/catalog') {
      return sendJson(res, { success: false, error: 'not found' }, 404);
    }

    // 签到中心
    // 签到中心快照是 **flat 响应**（A2A 契约，无 {success,data} 信封）；
    // records / run 仍走信封。桩按各自形状返回，否则页面拿不到 daily。
    if (p === '/api/checkin-center') return sendJson(res, CHECKIN_CENTER);
    if (p === '/api/checkin-center/records') {
      return sendJson(res, { success: true, data: { records: CHECKIN_RECORDS, total: CHECKIN_RECORDS.length } });
    }
    // 台账端点已迁到 A2A flat 契约：前端 GET /api/checkin-records 直读 records/total
    //（无 {success,data} 信封）。旧桩仍挂在 /api/checkin-center/records，页面因此取不到。
    if (p === '/api/checkin-records') {
      return sendJson(res, { records: CHECKIN_RECORDS, total: CHECKIN_RECORDS.length });
    }
    if (p === '/api/checkin-center/run') {
      return sendJson(res, {
        success: true,
        data: { total: 1, success: 1, already: 0, failed: 0, results: [{ account_id: 'acc-2', account_name: 'workbuddy-2', provider: 'workbuddy', success: true, outcome: 'success', message: '签到成功' }] },
      });
    }

    // ── AI 运维 Agent（/agent）──
    // 此前**没有任何 /api/agent/* 桩**：页面靠 EMPTY_LIST 兜底，`POST /agent/sessions`
    // 拿到的是数组（没有 id）→ 前端报「创建会话失败」，整条对话链在 E2E 里跑不起来。
    // 形状对齐 `src/agent/api.rs` 与 `api/endpoints/agent_chat.ts` 的 AgentConfigView。
    if (p === '/api/agent/config') {
      return sendJson(res, {
        success: true,
        data: {
          enabled: true,
          model: 'glm-4.7-flash',
          channel: '',
          effectiveModel: null,
          reasoningEffort: 'medium',
          systemPrompt: '',
          maxTurns: 8,
          approvalTimeoutSecs: 120,
          maxHistory: 40,
          disabledTools: [],
          // 工具中文名/风险的唯一来源（后端 registry）；缺它审批卡就只能显示裸名
          tools: [
            { name: 'list_channels', label: '查看供应商', risk: 'readonly' },
            { name: 'update_channel', label: '修改供应商', risk: 'high' },
          ],
        },
      });
    }
    if (p === '/api/agent/sessions' && req.method === 'GET') {
      return sendJson(res, {
        success: true,
        data: [...agentSessions.values()].map(({ messages: _m, ...s }) => s),
      });
    }
    if (p === '/api/agent/sessions' && req.method === 'POST') {
      const body = JSON.parse(bodyText || '{}');
      const id = `ag-${++agentSeq}`;
      const session = {
        id,
        title: body.title || '新会话',
        role: body.role === 'observer' ? 'observer' : 'operator',
        created_at: Math.floor(Date.now() / 1000),
        messages: [],
      };
      agentSessions.set(id, session);
      return sendJson(res, { success: true, data: { id, title: session.title, role: session.role } });
    }
    {
      const m = p.match(/^\/api\/agent\/sessions\/([^/]+)$/);
      if (m) {
        const sid = decodeURIComponent(m[1]);
        const s = agentSessions.get(sid);
        if (req.method === 'DELETE') {
          agentSessions.delete(sid);
          return sendJson(res, { success: true, data: null });
        }
        if (req.method === 'GET') {
          if (!s) return sendJson(res, { success: false, error: 'not found' }, 404);
          return sendJson(res, { success: true, data: { session: { id: s.id, title: s.title, role: s.role }, messages: s.messages } });
        }
      }
    }
    // 审批回复（POST /agent/approvals/:id）：只回成功，审批状态机由页面维护
    if (/^\/api\/agent\/approvals\/[^/]+$/.test(p) && req.method === 'POST') {
      return sendJson(res, { success: true, data: null });
    }

    // ── API 令牌（GET /api/tokens）──
    // 此前**没有桩**：/keys 落到 EMPTY_LIST，令牌表恒空 —— 「提供商」列、
    // 白名单多选、密钥值长度下限三处一条都没被 E2E 碰过（smoke 只证明页面能渲染）。
    if (p === '/api/tokens' && req.method === 'GET') {
      return sendJson(res, {
        success: true,
        // 顶层候选表与 data 同级（前端读 tokenRes.providers / modelsByProvider）
        providers: TOKEN_PROVIDERS,
        modelsByProvider: TOKEN_MODELS_BY_PROVIDER,
        // 后端口径：列表**脱敏**（明文只经单条 /key 与 rotate 一次性给出）
        data: tokensSeed.map(maskTokenRow),
        total: tokensSeed.length,
        page: 1,
        size: 20,
      });
    }
    if (p === '/api/tokens' && req.method === 'POST') {
      const body = JSON.parse(bodyText || '{}');
      lastTokenWrite = { method: 'POST', body };
      const created = {
        id: `tk-new-${tokensSeed.length + 1}`,
        name: body.name ?? '', user_email: 'admin@aigx.local', group: body.group ?? 'default',
        status: body.status ?? 'active', key: 'sk-e2e-new-****',
        allowed_providers: body.allowed_providers ?? null,
        allowed_models: body.allowed_models ?? '', ip_limit: body.ip_limit ?? [],
        quota_limit: body.quota_limit ?? null, used_quota: 0,
        expires_at: body.expires_at ?? null, created_at: Math.floor(Date.now() / 1000),
        // 内部明文（不进响应，列表也会被 maskTokenRow 剥掉）；留一条 path 让
        // 「新建后查看/轮换」在桩里也有明文可取，否则 GET /key 会给出 undefined。
        _plain: body.custom_key ?? `sk-e2e-new-plain-${tokensSeed.length + 1}`,
      };
      tokensSeed.unshift(created);
      // 响应形状保持原样（不带 plain_key）：keys-providers.spec 的「创建响应不带
      // plain_key 时不该弹一次性密钥框」这条断言建在这个形状上。真后端创建响应
      // 会带 plain_key（tokens.rs:318-320），那是另一条待补的判据，改这里会顺手
      // 把那条件断言弄红 —— 不在本轮改动面内。
      return sendJson(res, { success: true, data: maskTokenRow(created) });
    }
    // 写侧留痕回读（R1「直打桩」配套）：spec 用它确认 PUT 里到底发了什么，
    // 而不是只能从界面上猜「清空有没有真发出去」。
    if (p === '/api/__e2e/token-last-write' && req.method === 'GET') {
      return sendJson(res, { success: true, data: lastTokenWrite });
    }
    if (p === '/api/__e2e/token-last-write' && req.method === 'DELETE') {
      lastTokenWrite = null;
      return sendJson(res, { success: true, data: null });
    }
    // 令牌种子复位：desktop / mobile 两个 project 串行共用本进程，写入会跨 project
    // 残留（desktop 的「编辑删空白名单」把 tk-seed-1 的 allowed_providers 清成 null，
    // mobile 再断言「两个 provider chip」就红）。与 `__e2e/reset-proxy-pool` 同口径。
    if (p === '/api/__e2e/reset-tokens' && req.method === 'POST') {
      tokensSeed.length = 0;
      tokensSeed.push(...TOKENS_SEED());
      lastTokenWrite = null;
      return sendJson(res, { success: true, data: null });
    }
    {
      const m = p.match(/^\/api\/tokens\/([^/]+)$/);
      if (m && req.method === 'PUT') {
        const body = JSON.parse(bodyText || '{}');
        lastTokenWrite = { method: 'PUT', id: decodeURIComponent(m[1]), body };
        const tk = tokensSeed.find((it) => String(it.id) === decodeURIComponent(m[1]));
        if (tk) Object.assign(tk, body, { allowed_providers: body.allowed_providers ?? tk.allowed_providers });
        return sendJson(res, { success: true, data: tk ? maskTokenRow(tk) : null });
      }
      if (m && req.method === 'DELETE') {
        const idx = tokensSeed.findIndex((it) => String(it.id) === decodeURIComponent(m[1]));
        if (idx >= 0) tokensSeed.splice(idx, 1);
        return sendJson(res, { success: true, data: null });
      }
    }
    // ── 令牌单条明文 / 重置已用 / 轮换（keys-ops.spec.ts）──
    // 形状逐字段对齐真实 handler（不是自创）：
    //  · GET  /api/tokens/:id/key        → `{success,data:{id, plain_key}}`
    //    （src/api/admin/tokens.rs:279-281 handle_get_token_key）；
    //  · POST /api/tokens/:id/reset_used → `{success,data:null}`，used_quota 归零
    //    （tokens.rs:472-476 handle_reset_token_used → reset_used_quota）；
    //  · POST /api/tokens/:id/rotate     → `{success,data:{id,key,rotated_at,message}}`，
    //    新 key 只在此响应出现一次、旧 key 立即失效
    //    （src/api/admin/legacy.rs:2870-2877 handle_rotate_token）。
    {
      const m = p.match(/^\/api\/tokens\/([^/]+)\/(key|reset_used|rotate)$/);
      if (m) {
        const id = decodeURIComponent(m[1]);
        const action = m[2];
        const tk = tokensSeed.find((it) => String(it.id) === id);
        if (!tk) return sendJson(res, { success: false, error: 'Token not found' }, 404);
        if (action === 'key' && req.method === 'GET') {
          return sendJson(res, { success: true, data: { id: tk.id, plain_key: tk._plain } });
        }
        if (action === 'reset_used' && req.method === 'POST') {
          tk.used_quota = 0;
          return sendJson(res, { success: true, data: null });
        }
        if (action === 'rotate' && req.method === 'POST') {
          // 新明文每次不同（形如后端 `sk-<uuid 去横线>`）：spec 用它断「拿到的确实是新的」
          const fresh = `sk-e2e-rot-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
          tk._plain = fresh;
          tk.key = `sk-e2e-****-${fresh.slice(-4)}`;
          return sendJson(res, {
            success: true,
            data: { id: tk.id, key: fresh, rotated_at: Math.floor(Date.now() / 1000), message: 'Token rotated; old key invalidated' },
          });
        }
        return sendJson(res, { success: false, error: 'method not allowed' }, 405);
      }
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
      const body = JSON.parse(bodyText || '{}');
      const id = `cs-${++chatSeq}`;
      const now = Math.floor(Date.now() / 1000);
      // title 缺省给「新会话」（与前端默认一致），显式给值就原样存：
      // 删除类用例需要「按标题点名一个一次性会话」——服务端忽略 title 的话，
      // 列表里全是「新会话」，没法只删自己造的那条（会误删种子会话）。
      const session = { id, title: (typeof body.title === 'string' && body.title) || '新会话', model: body.model ?? '', created_at: now, updated_at: now, messages: [] };
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
      return sendJson(res, { success: true, data: REPORT_TOP_SOURCES });
    }
    // 客户端榜此前**没有桩**（落到 EMPTY_LIST）→ TopClients 恒为空表，
    // 排序切换 / 行下钻 / 榜尾占比三处一条都没被覆盖过。
    if (p === '/api/dashboard/report/top_clients') {
      return sendJson(res, { success: true, data: REPORT_TOP_CLIENTS });
    }
    if (p === '/api/dashboard/report/token_trend') {
      return sendJson(res, { success: true, data: [{ date: '2025-10-30', input_tokens: 1000, output_tokens: 500 }, { date: '2025-10-31', input_tokens: 2000, output_tokens: 900 }] });
    }
    // 模型榜此前只给 `{model, count}`：缺 input_tokens/output_tokens ⇒
    // 模型分布饼图的 donutData 全为 0 ⇒ DonutChart 的 `rows.length === 0` 早退
    // ⇒ 饼图在 E2E 里从未渲染过（「双饼图 + 阈值」零覆盖，看着绿而已）。
    if (p === '/api/dashboard/report/models') {
      return sendJson(res, { success: true, data: REPORT_MODELS });
    }
    if (p === '/api/dashboard/report/cache_rates') {
      return sendJson(res, { success: true, data: [{ window: 'last24h', hit_tokens: 100, input_tokens: 500, rate: 20 }] });
    }
    // 用量趋势：此前只有**一行**（单点既画不出柱也画不出折线，「指标 × 形态」
    // 两个 SegmentedControl 在 E2E 里点了看不出差别）。三行让两种形态、三种指标
    // 都有可区分的读数：请求数合计 60 / Token 合计 7000 / 消费合计 176。
    if (p === '/api/dashboard/report/daily_trend') {
      return sendJson(res, {
        success: true,
        data: [
          { date: '2025-10-29', count: 20, cost: 123, input_tokens: 1000, output_tokens: 500 },
          { date: '2025-10-30', count: 30, cost: 45, input_tokens: 3000, output_tokens: 1000 },
          { date: '2025-10-31', count: 10, cost: 8, input_tokens: 1200, output_tokens: 300 },
        ],
      });
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
