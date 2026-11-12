import { cloneElement, Fragment, isValidElement, useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type ReactElement, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ChevronsDownUp, ChevronsUpDown, Columns3, Download, Eye, EyeOff, FlaskConical, Gift, Globe, GripVertical,
  KeyRound, ListChecks, Loader2, MoreHorizontal, Pencil, Plus, Power, RotateCcw, SlidersHorizontal,
  Trash2, Upload, User, Wallet, Wrench,
} from 'lucide-react';
import { api } from '../api';
import { providerIcon } from '../lib/providerIcons';
import { addConfigOf, cardKindOf, isCardHidden } from '../lib/addAccountConfigs';
import { CHECKIN_PROVIDERS } from '../lib/checkinProviders';
import { useToast } from '../components/Toast';
import AddAccountDialog from '../components/AddAccountDialog';
import AccountBatchDialog from '../components/AccountBatchDialog';
import LimiterGroupEditor from '../components/LimiterGroupEditor';
import { describeGroup, flatToGroups } from '../lib/limiterGroups';
import ProxySelector from '../components/ProxySelector';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import TableColumnSettings from '../components/ui/TableColumnSettings';
import { useColumnResize } from '../components/ui/useColumnResize';
import { CLIENT_PAGE_SIZES, TablePaginationFooter, usePageSize } from '../components/ui/TableShell';
import {
  applyTableColumns,
  readTableColumns,
  resetTableColumns,
  writeTableColumns,
  type TableColumnDef,
  type TableColumnItem,
  type TableColumnState,
} from '../lib/tableColumns';
import { Button, Card, EmptyState, Input, Loading, Select, SkeletonTable, Switch, Tabs, Textarea, Tooltip } from '../components/ui';
import type {
  AccountBalance,
  AccountLimiterRule,
  AccountProviderItem,
  AccountSessionItem,
  AccountSessionStatsItem,
  AccountUsageWindow,
  CatalogSyncAccount,
  ChannelItem,
  ImportSourceEntry,
  ProxyPoolItem,
} from '../types';
import type { LimiterGroup } from '../types/limiter';
import './AccountSessions.css';

interface SessionFormState {
  channel_id: string;
  name: string;
  provider: string;
  base_url: string;
  /** 对话路径覆盖（如 `/api/chat`，留空走默认 /chat/completions） */
  chat_path: string;
  api_key: string;
  proxy: string;
  status: string;
  priority: string;
  weight: string;
  max_concurrency: string;
  models: string;
  /** 额外请求头（每行 `Key: Value`，登录态型供应商补专用头） */
  extra_headers: string;
  /** 余额自动查询间隔（秒；'0' = 关闭） */
  usage_query_interval_secs: string;
  expires_at: string;
  clear_key: boolean;
  clear_proxy: boolean;
}

/** 相对时间（「3 分钟前」），用于展示余额快照的采集时刻。 */
function formatAgo(unixSeconds: number): string {
  const delta = Math.max(0, Math.floor(Date.now() / 1000) - unixSeconds);
  if (delta < 60) return '刚刚';
  if (delta < 3600) return `${Math.floor(delta / 60)} 分钟前`;
  if (delta < 86400) return `${Math.floor(delta / 3600)} 小时前`;
  return `${Math.floor(delta / 86400)} 天前`;
}

/** 用量窗口展示名（对齐上游 usageWindowLabel：5h/weekly/monthly/daily → 中文短名） */
function usageWindowTitle(w: AccountUsageWindow): string {
  const code = (w.window || '').trim();
  const lower = code.toLowerCase();
  const name = lower === '5h' ? '5 小时'
    : lower === 'weekly' ? '每周'
    : lower === 'monthly' ? '每月'
    : lower === 'daily' ? '每日'
    : code;
  return (w.label || [w.group, name].filter(Boolean).join(' · ') || name || '窗口').trim();
}

const EMPTY_FORM: SessionFormState = {  channel_id: '',
  name: '',
  provider: 'openai_compatible',
  base_url: '',
  chat_path: '',
  api_key: '',
  proxy: '',
  status: 'enabled',
  priority: '0',
  weight: '1',
  max_concurrency: '3',
  models: '',
  extra_headers: '',
  usage_query_interval_secs: '0',
  expires_at: '',
  clear_key: false,
  clear_proxy: false,
};

interface BulkFormState {
  channel_id: string;
  provider: string;
  base_url: string;
  proxy: string;
  name_prefix: string;
  priority: string;
  weight: string;
  max_concurrency: string;
  models: string;
  credentials: string;
}

const EMPTY_BULK: BulkFormState = {
  channel_id: '',
  provider: 'openai_compatible',
  base_url: '',
  proxy: '',
  name_prefix: 'account',
  priority: '0',
  weight: '1',
  max_concurrency: '3',
  models: '',
  credentials: '',
};

/** 运行态 → badge 样式 */
function statusClass(status?: string): string {
  if (status === 'enabled') return 'badge-success';
  if (status === 'expired') return 'badge-warning';
  return 'badge-neutral';
}

/** 支持余额查询的 provider（与后端 account_session/balance.rs 名单一致） */
const BALANCE_PROVIDERS: ReadonlySet<string> = new Set([
  'qoder', 'qoder-intl', 'autoclaw', 'autoclaw-intl', 'xiaohuanxiong',
  'zcode', 'zcode-intl', 'kuku', 'loomy', 'officeace', 'accio', 'accio-cn',
  'catpaw', 'orcaterm', 'cline', 'cline-free', 'cline-pass', 'antigravity',
  'workbuddy', 'workbuddy-intl', 'trae', 'codearts',
]);

/* ══ localStorage 工具（页面级偏好记忆：列宽/列显隐/分组/脱敏/自动余额） ══ */

function readLS<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : fallback;
  } catch {
    return fallback;
  }
}

function writeLS(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // 隐私模式等存不了就算了：本次会话内仍然生效
  }
}

/* ══ 姓名脱敏（对齐 Agent2API 表头眼睛：整列打码） ══ */

/** 保留首尾字符、中间打码；过短的全打。旁人扫屏时看不出是谁。 */
function maskName(name: string): string {
  const s = (name || '').trim();
  if (!s) return '';
  if (s.length <= 2) return '*'.repeat(s.length);
  return `${s[0]}***${s[s.length - 1]}`;
}

/* ══ 余额主额度桶（对齐 Agent2API .usage-pool 两行形态：套餐名 + 进度条 + 读数） ══ */

/** 余额单元格的两行形态视图 */
interface UsagePoolView {
  /** 套餐 / 额度桶名（第一行；可为空） */
  plan: string;
  /** 读数（第二行，已拼好「数值 单位」） */
  text: string;
  /** 剩余比例（0~1；上游没给总额时为 null，不画进度条） */
  ratio: number | null;
  /** 读数是否已耗尽（红色警示） */
  low: boolean;
}

/** 从余额读数提取主额度桶：优先取第一个给出总额的桶，否则第一个桶 / 顶层 available */
function usagePoolOf(bal: AccountBalance | null | undefined): UsagePoolView | null {
  if (!bal) return null;
  const wallets = Array.isArray(bal.wallets) ? bal.wallets : [];
  const main = wallets.find((w) => Number(w?.total) > 0) || wallets[0] || null;
  const available = main != null && main.balance != null ? Number(main.balance) : bal.available;
  if (available == null || !Number.isFinite(available)) return null;
  const total = main ? Number(main.total) || 0 : 0;
  const subPlan = bal.subscription
    && typeof (bal.subscription as Record<string, unknown>).plan === 'string'
    ? String((bal.subscription as Record<string, unknown>).plan)
    : '';
  const plan = (main && typeof main.displayName === 'string' && main.displayName) || subPlan;
  return {
    plan,
    text: `${available}${bal.unit ? ` ${bal.unit}` : ''}`,
    ratio: total > 0 ? Math.max(0, Math.min(1, available / total)) : null,
    low: available <= 0,
  };
}

/* ══ 凭证有效期列（对齐 Agent2API ExpiryCell：到期时间归一 + 剩余时长收短展示） ══ */

/** 到期值归一到毫秒：后端各家给的 expires_at / expireAt 在 10 位秒与 13 位毫秒
 *  之间漂过（对齐上游 expiryMillis 的归一规则，不归一会把 10 位秒读成 1970）。 */
function expiryMillis(value: unknown): number | null {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return null;
  return n < 1e11 ? n * 1000 : n;
}

/** 距离到期的人话文案（「30 分钟后 / 5 小时后 / 3 天后」；完整时刻留给 title） */
function expiryLeftText(leftMs: number): string {
  if (leftMs < 3600_000) return `${Math.max(1, Math.round(leftMs / 60_000))} 分钟后`;
  if (leftMs < 48 * 3600_000) return `${(leftMs / 3600_000).toFixed(1)} 小时后`;
  return `${Math.floor(leftMs / (24 * 3600_000))} 天后`;
}

/** 账号的最早到期毫秒（凭证 expires_at 优先，缺省再看余额快照的套餐到期）。 */
function accountExpiryOf(s: AccountSessionItem): { at: number | null; source: 'credential' | 'plan' | null } {
  const cred = expiryMillis(s.expires_at);
  if (cred != null) return { at: cred, source: 'credential' };
  const bal = s.last_balance as AccountBalance | null | undefined;
  const sub = bal?.subscription as Record<string, unknown> | null | undefined;
  if (sub) {
    const plan = expiryMillis(sub.expireAt ?? sub.expire_at);
    if (plan != null) return { at: plan, source: 'plan' };
  }
  return { at: null, source: null };
}

/* ══ 待生效套餐徽章（对齐 Agent2API pendingPlans：plans[] 里 state=pending 的权益） ══ */

/** 余额快照里待生效的套餐（后端给的 plans 数组带 pending 态才有；缺省 = 空）。 */
function pendingPlansOf(s: AccountSessionItem): Array<Record<string, unknown>> {
  const bal = s.last_balance as AccountBalance | null | undefined;
  const plans = (bal as Record<string, unknown> | null | undefined)?.plans;
  if (!Array.isArray(plans)) return [];
  return plans.filter(
    (p) => p && typeof p === 'object' && String((p as Record<string, unknown>).state || '') === 'pending',
  ) as Array<Record<string, unknown>>;
}

/* ══ 账号限制器（对齐 Agent2API accounts-dialog-limiter：余额 / Token 两类规则） ══ */
/* 规则类型 AccountLimiterRule 定义在 ../types（后端 PATCH limiters 键整块替换） */

/** Token 固定周期的上下限（对齐 Agent2API TOKEN_PERIOD_*） */
const TOKEN_PERIOD_MIN_SECS = 30 * 60;
const TOKEN_PERIOD_MAX_SECS = 24 * 3600;

/** 一条规则的「触发条件 → 触发动作」一句话（规则列表用） */
function describeLimiterRule(rule: AccountLimiterRule): string {
  const action = rule.action === 'disable'
    ? '禁用该账号（需手动重新启用）'
    : '跳过该账号（自动恢复）';
  if (rule.type === 'balance') {
    return `余额 < ${rule.threshold} 时 → ${action}`;
  }
  const quota = rule.threshold >= 10000 && rule.threshold % 10000 === 0
    ? `${rule.threshold / 10000} 万`
    : `${rule.threshold}`;
  if (rule.reset === 'daily') {
    return `每日消耗 ≥ ${quota} Token（每天 0 点重置）时 → ${action}`;
  }
  const period = rule.period ?? 0;
  const periodText = period < 60 ? `${period}s` : period < 3600 ? `${Math.round(period / 60)} 分钟` : `${Math.round(period / 3600) * 3600 === period ? period / 3600 : (period / 3600).toFixed(1)} 小时`;
  return `每 ${periodText}消耗 ≥ ${quota} Token 时 → ${action}`;
}

/** 编辑器草稿（数字输入框以字符串中转，输入中途允许为空） */
interface LimiterDraft {
  type: 'balance' | 'token';
  action: 'skip' | 'disable';
  thresholdInput: string;
  /** Token 阈值的输入单位（万 / 个），落库统一换算成个数 */
  tokenUnit: 'wan' | 'unit';
  reset: 'fixed' | 'daily';
  periodInput: string;
  periodUnit: 'minutes' | 'hours';
}

function limiterDraftOf(rule: AccountLimiterRule | null): LimiterDraft {
  if (!rule) {
    return { type: 'balance', action: 'skip', thresholdInput: '', tokenUnit: 'wan', reset: 'fixed', periodInput: '1', periodUnit: 'hours' };
  }
  const unit: LimiterDraft['tokenUnit'] = rule.type === 'token' && rule.threshold % 10000 === 0 && rule.threshold > 0 ? 'wan' : 'unit';
  const thresholdInput = rule.type === 'token' && unit === 'wan'
    ? String(rule.threshold / 10000)
    : String(rule.threshold);
  let periodInput = '1';
  let periodUnit: LimiterDraft['periodUnit'] = 'hours';
  if (rule.type === 'token' && rule.reset !== 'daily' && rule.period) {
    if (rule.period % 3600 === 0) {
      periodInput = String(rule.period / 3600);
      periodUnit = 'hours';
    } else {
      periodInput = String(Math.round(rule.period / 60));
      periodUnit = 'minutes';
    }
  }
  return { type: rule.type, action: rule.action, thresholdInput, tokenUnit: unit, reset: rule.reset === 'daily' ? 'daily' : 'fixed', periodInput, periodUnit };
}

/** 草稿 → 规则（校验不过返回错误文案；与参考实现同一套规则） */
function limiterRuleOfDraft(draft: LimiterDraft): { error: string } | { rule: AccountLimiterRule } {
  const thresholdRaw = Number(draft.thresholdInput);
  if (!Number.isFinite(thresholdRaw) || thresholdRaw <= 0) {
    return { error: '限制阈值必须是大于 0 的数字' };
  }
  if (draft.type === 'balance') {
    return { rule: { type: 'balance', action: draft.action, threshold: thresholdRaw, enabled: true } };
  }
  const threshold = Math.round(draft.tokenUnit === 'wan' ? thresholdRaw * 10000 : thresholdRaw);
  if (draft.reset === 'daily') {
    return { rule: { type: 'token', action: draft.action, threshold, reset: 'daily', enabled: true } };
  }
  const period = Math.round(Number(draft.periodInput) * (draft.periodUnit === 'hours' ? 3600 : 60));
  if (!Number.isFinite(period) || period <= 0) {
    return { error: '请填写重置周期' };
  }
  if (period < TOKEN_PERIOD_MIN_SECS || period > TOKEN_PERIOD_MAX_SECS) {
    return { error: '重置周期必须是 30 分钟 ~ 24 小时' };
  }
  return { rule: { type: 'token', action: draft.action, threshold, period, reset: 'fixed', enabled: true } };
}

/* ══ 列定义（对齐 Agent2API accounts-columns：列宽拖动 + 列显隐 + 横滚底线） ══ */

type AsCol = 'pick' | 'name' | 'provider' | 'proxy' | 'status' | 'priority' | 'conn' | 'expiry' | 'usage' | 'rate' | 'cooldown' | 'act';

/** 全部列：默认宽（px）；flex 列没有用户覆盖时不写宽度，吃表格剩余宽度 */
const AS_COLS: Array<{ key: AsCol; label: string; w: number; lock?: boolean; flex?: boolean }> = [
  { key: 'pick', label: '选择', w: 44, lock: true },
  { key: 'name', label: '账号', w: 210, lock: true, flex: true },
  { key: 'provider', label: 'Provider', w: 126 },
  { key: 'proxy', label: '代理', w: 132 },
  { key: 'status', label: '状态', w: 92, lock: true },
  { key: 'priority', label: '优先级', w: 88 },
  { key: 'conn', label: '连接数', w: 60 },
  { key: 'expiry', label: '有效期', w: 96 },
  { key: 'usage', label: '余额', w: 132 },
  { key: 'rate', label: '成功率', w: 64 },
  { key: 'cooldown', label: '冷却', w: 128 },
  { key: 'act', label: '操作', w: 150, lock: true },
];

/** 弹性列的下限：容器窄于「定宽列合计 + 它」时交给横向滚动，而不是继续压列 */
const AS_FLEX_MIN = 220;
/** 拖动的下限：再窄里面的控件就点不准了 */
const AS_MIN_COLW = 48;

const LS_COLCFG = 'aigx-as-colcfg'; // 旧显隐表（公共层 v1 迁移源，读后由公共层接管）
const LS_COLW = 'aigx-as-colw'; // 旧列宽覆盖（init 时一次性并进 v2 后删键）
/** 公共列设置键（tableColumns v2：顺序/显隐/对齐/列宽 + defaults 快照） */
const LS_TCOLS = 'aigx-tbl-cols.accounts';

/** 公共列声明的锁定/弹性标注映射（w: px 默认宽；flex 列不写默认宽吃剩余空间） */
const AS_COL_DEFS: TableColumnDef[] = AS_COLS.map((c) => ({
  key: c.key,
  label: c.label,
  align: c.key === 'pick' || c.key === 'status' ? 'center' : 'left',
  ...(c.flex ? {} : { width: c.w }),
  ...(c.lock ? { locked: true } : {}),
}));
const LS_GROUPED = 'aigx-as-grouped';
const LS_NAMEMASK = 'aigx-as-namemask';
const LS_AUTOBAL = 'aigx-as-autobal'; // 自动余额轮询间隔（分钟）
/** 分组折叠记忆（对齐 Agent2API accounts-store COLLAPSED_KEY：收起的分组跨次启动保留） */
const LS_COLLAPSED = 'aigx-as-collapsed';
/** 列表排序记忆（对齐上游 accounts-sort 的落盘偏好；'list' = 调度优先序） */
const LS_SORTKEY = 'aigx-as-sortkey';
/** 筛选偏好（SPEC §2.5：只落 provider/status/limit，不落搜索词） */
const LS_FILTER = 'aigx-as-filter';
/** 每页条数（SPEC §2.1：与全站 aigx-tbl-pagesize.<page> 命名一致） */
const LS_PSZ = 'aigx-tbl-pagesize.accounts';

type StatusSeg = 'all' | 'enabled' | 'disabled' | 'cooling';
type LimitSeg = 'all' | 'normal' | 'limited';

interface FilterPrefs {
  provider: string;
  status: StatusSeg;
  limit: LimitSeg;
}

/** 读取筛选偏好并做档位合法校验（脏值回落「全部」） */
function readFilterPrefs(): FilterPrefs {
  const raw = readLS<Partial<FilterPrefs>>(LS_FILTER, {});
  const status: StatusSeg =
    raw.status === 'enabled' || raw.status === 'disabled' || raw.status === 'cooling' ? raw.status : 'all';
  const limit: LimitSeg = raw.limit === 'normal' || raw.limit === 'limited' ? raw.limit : 'all';
  return { provider: typeof raw.provider === 'string' ? raw.provider : '', status, limit };
}

/**
 * 连接数快照走 api.getAccountSessionConnections（已收敛进 api/index.ts）：
 * 端点未上线 / 网络异常时静默返回 null，页面降级用列表自带的 in_flight
 * 字段展示，绝不崩溃。
 */

/** 账号设置弹窗的五个分组（对齐 Agent2API acct-dlg 左栏导航） */
const DLG_SEGS = [
  { key: 'basic', label: '基本信息', icon: 'user' },
  { key: 'credential', label: '凭证', icon: 'key' },
  { key: 'proxy', label: '代理', icon: 'globe' },
  { key: 'limiter', label: '限制器', icon: 'sliders' },
  { key: 'advanced', label: '高级', icon: 'wrench' },
] as const;

type DlgSeg = (typeof DLG_SEGS)[number]['key'];

/** 五分组导航图标（key 与 DLG_SEGS 的 icon 字段一一对应） */
const DLG_ICONS: Record<(typeof DLG_SEGS)[number]['icon'], ReactNode> = {
  user: <User size={15} />,
  key: <KeyRound size={15} />,
  globe: <Globe size={15} />,
  sliders: <SlidersHorizontal size={15} />,
  wrench: <Wrench size={15} />,
};

interface AccountSessionsProps {
  /** 作为「供应商」页「账号池」Tab 的子页内嵌时隐藏自身标题区 */
  embedded?: boolean;
  /**
   * 供应商列表下钻：`?provider=<channel_id>` 作为初始目标渠道。
   * 只做初值（无 UI 切换），调度逻辑与内部筛选行为保持原样。
   */
  initialChannelFilter?: string;
}

export default function AccountSessions({ embedded = false, initialChannelFilter }: AccountSessionsProps = {}): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [sessions, setSessions] = useState<AccountSessionItem[]>([]);
  const [accountChannels, setAccountChannels] = useState<ChannelItem[]>([]);
  const [providers, setProviders] = useState<AccountProviderItem[]>([]);
  const [stats, setStats] = useState<AccountSessionStatsItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  /** 目标渠道（新增/导入时的落点）；不再是用户可见的筛选维度。
   *  初值来自 URL `?provider=`（供应商列表「管理账号」下钻），之后不再变化。 */
  const [channelFilter] = useState(initialChannelFilter ?? '');
  /** 提供商筛选（对齐 Agent2API 账号页的 provider 下拉）：'' = 全部；偏好落盘（§2.5） */
  const [providerFilter, setProviderFilter] = useState<string>(() => readFilterPrefs().provider);
  const [search, setSearch] = useState('');

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<AccountSessionItem | null>(null);
  const [form, setForm] = useState<SessionFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);

  // 从浏览器抓包粘贴的原始请求头（解析后填充凭证与额外请求头）
  const [rawHeaders, setRawHeaders] = useState('');

  // 账号路由（轮换策略）：冷却窗口与单请求最大换号次数
  const [showRotation, setShowRotation] = useState(false);
  const [rotation, setRotation] = useState({ base: '60', max: '1800', auth: '900', attempts: '5' });
  const [rotationSaving, setRotationSaving] = useState(false);
  const [rotationError, setRotationError] = useState('');

  const [showBulk, setShowBulk] = useState(false);
  const [bulk, setBulk] = useState<BulkFormState>(EMPTY_BULK);

  const [testingId, setTestingId] = useState<string | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  // 状态分段过滤（对齐 Agent2API 账号页：全部/启用/禁用/冷却中）
  const [statusSeg, setStatusSeg] = useState<StatusSeg>(() => readFilterPrefs().status);
  // 限额分段过滤（SPEC §2.4：全部/正常/已限流；唯一判据是后端 limiter_state）
  const [limitSeg, setLimitSeg] = useState<LimitSeg>(() => readFilterPrefs().limit);
  // 列表排序（对齐上游 accounts-sort：展示序不改调度 priority；
  // credit 档按凭证/套餐到期（最急着过期的排最前，没到期值排最后），
  // added 档按创建时间新→旧；默认 list = 调度优先序，选择落盘 localStorage）
  const [sortKey, setSortKey] = useState<'list' | 'credit' | 'added'>(() => {
    const v = readLS<string>(LS_SORTKEY, 'list');
    return v === 'credit' || v === 'added' ? v : 'list';
  });
  // 勾选与批量操作（批量启停/删除，循环既有 PATCH/DELETE 端点）
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [batchRunning, setBatchRunning] = useState(false);
  // 批量操作对话框（六类 + 限制器五模式 + 逐条回执，对齐上游 accounts-batch-dialog；
  // AccountBatchDialog 内部优先走批量端点、未上线自动降级循环 PATCH/DELETE）
  const [batchDlgOpen, setBatchDlgOpen] = useState(false);
  // 余额查询（后端 :id/balance 支持 12 家）与手动刷新凭证（:id/refresh）
  // ok 分支同时带主额度桶视图（.as-usage-pool 两行形态的数据）
  type BalanceView =
    | { state: 'loading' }
    | { state: 'ok'; pool: UsagePoolView | null; text: string; windows?: AccountUsageWindow[]; raw?: AccountBalance | null }
    | { state: 'err'; text: string };
  const [balances, setBalances] = useState<Record<string, BalanceView>>({});
  const [refreshingId, setRefreshingId] = useState<string | null>(null);
  // 拖拽排序（HTML5 DnD，落盘后重赋 priority 落盘）
  const [dragId, setDragId] = useState<string | null>(null);
  const [dragOverId, setDragOverId] = useState<string | null>(null);
  const [orderSaving, setOrderSaving] = useState(false);
  /** 冷却明细展开行（对齐 Agent2API row-panel：点冷却格展开完整明细） */
  const [cooldownOpenId, setCooldownOpenId] = useState<string | null>(null);
  /** 冷却读数的抓取时刻（后端返回的是「剩余秒数」，本地按秒表往前推） */
  const cooldownLoadedAt = useRef<number>(Date.now());
  // 有冷却时挂一个 1 秒定时器把倒计时推走（对齐 Agent2API 的 live「x 后恢复」）
  const [, setCooldownTick] = useState(0);
  /** 自动余额查询只做一次（load 内触发，见 load） */
  const autoBalanceDone = useRef(false);

  // ── 列显隐 / 列宽 / 分组 / 姓名脱敏 / 自动余额周期轮询（对齐 Agent2API 账号表）──
  // 列设置已迁公共层（tableColumns v2：顺序/显隐/对齐/列宽统一记忆）。
  // 旧键一次性搬迁：as-colw 列宽覆盖并进 v2；as-colcfg 显隐表转成 v1 形态
  // 喂给公共层迁移（v1 = {columns:[{key,visible,align}]}），搬完删旧键。
  const [colState, setColState] = useState<TableColumnState>(() => {
    let migrated = readTableColumns(LS_TCOLS, AS_COL_DEFS);
    const legacyHidden = readLS<AsCol[]>(LS_COLCFG, []);
    const legacyW = readLS<Record<string, number>>(LS_COLW, {});
    if (legacyHidden.length > 0 && localStorage.getItem(LS_TCOLS) === null) {
      // v1 形态只表达显隐：顺序/对齐取默认，hidden 的进 items 为 visible:false
      const v1 = {
        columns: AS_COLS.map((c) => ({
          key: c.key,
          visible: !legacyHidden.includes(c.key),
          align: c.key === 'pick' || c.key === 'status' ? 'center' : 'left',
        })),
      };
      try {
        migrated = readTableColumns(LS_TCOLS, AS_COL_DEFS, () => JSON.stringify(v1));
      } catch { /* 迁移失败退默认，不炸表 */ }
    }
    if (Object.keys(legacyW).length === 0) return migrated;
    const merged: TableColumnState = {
      ...migrated,
      items: migrated.items.map((i) =>
        legacyW[i.key] != null && i.width == null ? { ...i, width: legacyW[i.key] } : i,
      ),
    };
    writeTableColumns(LS_TCOLS, merged);
    try {
      localStorage.removeItem(LS_COLCFG);
      localStorage.removeItem(LS_COLW);
    } catch { /* 静默 */ }
    return merged;
  });
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);
  const [grouped, setGrouped] = useState<boolean>(() => readLS(LS_GROUPED, false));
  const [nameMasked, setNameMasked] = useState<boolean>(() => readLS(LS_NAMEMASK, false));
  /** 分组折叠记忆（localStorage；上游同款偏好：收起的组下次打开还是收着的） */
  const [collapsedGroups, setCollapsedGroups] = useState<ReadonlySet<string>>(
    () => new Set(readLS<string[]>(LS_COLLAPSED, [])),
  );
  const [autoBalanceMins, setAutoBalanceMins] = useState<number>(() => readLS(LS_AUTOBAL, 0));
  /** 分组带 ⋯ 菜单（对齐上游 GroupMenu：整组启停/折叠；一次只开一份） */
  const [groupMenuFor, setGroupMenuFor] = useState<string | null>(null);
  const groupMenuRef = useRef<HTMLDivElement | null>(null);
  /** 连接数轮询读数（端点未上线时为空表，展示层用 in_flight 兜底） */
  const [connCounts, setConnCounts] = useState<Record<string, number>>({});

  // ── SPEC 1.0.44：行内代理下拉 / 领套餐 / ⋯ 聚合菜单 / 余额明细弹层 / 分页 ──
  /** 代理池快照（§2.11 行内下拉选项；一次拉取，失败退化为「直连 + 当前值」） */
  const [proxyPool, setProxyPool] = useState<ProxyPoolItem[] | null>(null);
  const [proxySavingId, setProxySavingId] = useState<string | null>(null);
  const [claimingId, setClaimingId] = useState<string | null>(null);
  /** 操作列 ⋯ 聚合菜单（§2.13：刷新 / 设为首选 / 并发 / 删除；一次只开一份） */
  const [actMenuFor, setActMenuFor] = useState<string | null>(null);
  const actMenuRef = useRef<HTMLDivElement | null>(null);
  const [concDraft, setConcDraft] = useState('');
  /** 余额明细弹层（§2.9：点击余额格查看 wallets / windows 明细） */
  const [balancePopFor, setBalancePopFor] = useState<string | null>(null);
  const balancePopRef = useRef<HTMLDivElement | null>(null);
  /** 分页（§2.1：公共 usePageSize + TablePaginationFooter；默认「全部」保持原体验） */
  const [pageSize, setPageSize] = usePageSize(LS_PSZ, CLIENT_PAGE_SIZES, 'all');
  const [rawPage, setRawPage] = useState(1);
  /** 组带拖拽（§2.6）：拖拽中/悬停目标组 */
  const [groupDragActive, setGroupDragActive] = useState<string | null>(null);
  const [groupDragOver, setGroupDragOver] = useState<string | null>(null);

  // ── 账号设置弹窗五分组导航（基本信息/凭证/代理/限制器/高级）──
  const [dlgSeg, setDlgSeg] = useState<DlgSeg>('basic');
  /** 限制器规则草稿（保存随表单主体之后单独 PATCH，失败不拖累主保存） */
  const [limiters, setLimiters] = useState<AccountLimiterRule[]>([]);
  const limitersBaseline = useRef<string>('[]');
  const [limiterEditor, setLimiterEditor] = useState<{ index: number; draft: LimiterDraft } | null>(null);
  const [limiterError, setLimiterError] = useState('');
  /** 条件组草稿（#29）：非空时调度优先按条件组判定，为空回落旧扁平规则 */
  const [limiterGroupsDraft, setLimiterGroupsDraft] = useState<LimiterGroup[]>([]);
  const limiterGroupsBaseline = useRef<string>('[]');
  /** 条件组二级弹层：index = 编辑组下标；-1 = 新增 */
  const [lgEditor, setLgEditor] = useState<{ index: number } | null>(null);

  // ── 按模型清限流（操作列重置按钮：多模型冷却时先弹菜单选模型）──
  const [clearMenuFor, setClearMenuFor] = useState<string | null>(null);
  const clearMenuRef = useRef<HTMLDivElement | null>(null);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const [sessionsRes, channelsRes, providersRes, statsRes] = await Promise.all([
        api.listAccountSessions(),
        api.listChannels({ channel_type: 'account' }),
        api.accountProviders(),
        api.accountSessionStats(),
      ]);
      setSessions(sessionsRes.data ?? []);
      cooldownLoadedAt.current = Date.now();
      const channels = channelsRes.data ?? [];
      setAccountChannels(channels.filter((c) => c.channel_type === 'account'));
      setProviders(providersRes.data ?? []);
      setStats(statsRes.data?.channels ?? []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 筛选偏好落盘（§2.5：provider / 状态档 / 限额档；搜索词不落盘）
  useEffect(() => {
    writeLS(LS_FILTER, { provider: providerFilter, status: statusSeg, limit: limitSeg });
  }, [providerFilter, statusSeg, limitSeg]);

  // 失效自愈（§2.5）：存的 provider 在当前 sessions 里已不存在 → 静默回到「全部」。
  // loading / error 期间不动，避免一次网络抖动把偏好清掉。
  useEffect(() => {
    if (loading || error || !providerFilter) return;
    if (sessions.some((s) => s.provider === providerFilter)) return;
    setProviderFilter('');
  }, [loading, error, sessions, providerFilter]);

  // 代理池快照（§2.11 行内下拉的来源；失败静默退化为「直连 + 当前值 + 轮询」）
  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const res = await api.listProxyPool();
        // 后端 /api/proxy-pool 的条目在响应顶层（{success, items, total}），
        // 兼容未来包一层 data 的形态；两种取法都认。
        const top = res as unknown as { items?: ProxyPoolItem[] };
        const items = Array.isArray(top.items) ? top.items : (res.data?.items ?? []);
        if (alive) setProxyPool(items);
      } catch {
        if (alive) setProxyPool([]);
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  // 冷却倒计时：任一账号仍有未到期冷却就每秒重画一次（数据本身不重拉）
  const hasActiveCooldown = sessions.some((s) =>
    Object.values(s.cooldowns || {}).some((v) => Number(v) > 0),
  );
  useEffect(() => {
    if (!hasActiveCooldown) return undefined;
    const timer = window.setInterval(() => setCooldownTick((c) => c + 1), 1000);
    return () => window.clearInterval(timer);
  }, [hasActiveCooldown]);

  // ── 连接数轮询（对齐 Agent2API getAccountConnections 2 秒轮询）──
  // 端点未上线（404）或网络异常时静默降级：拉长到 15 秒再试，展示层用 in_flight 兜底。
  useEffect(() => {
    let alive = true;
    let timer = 0;
    const tick = async (): Promise<void> => {
      const counts = await api.getAccountSessionConnections();
      if (!alive) return;
      if (counts) {
        setConnCounts(counts);
        timer = window.setTimeout(() => void tick(), 2000);
      } else {
        timer = window.setTimeout(() => void tick(), 15000);
      }
    };
    void tick();
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, []);

  const openCreate = (): void => {
    setEditing(null);
    setForm({
      ...EMPTY_FORM,
      channel_id: channelFilter || accountChannels[0]?.id || '',
    });

    setRawHeaders('');
    setShowModal(true);
  };


  const openEdit = (session: AccountSessionItem): void => {
    setEditing(session);
    setForm({
      channel_id: session.channel_id,
      name: session.name,
      provider: session.provider || 'openai_compatible',
      base_url: session.base_url || '',
      chat_path: session.chat_path || '',
      api_key: '',
      // 接口返回的代理已脱敏（userinfo 打码），回填会被误存；留空 + 占位提示
      proxy: '',
      // 表单只提供启用/停用两态；已过期账号按停用处理（保存即显式置为停用）
      status: (session.configured_status || session.status) === 'enabled' ? 'enabled' : 'disabled',
      priority: String(session.priority ?? 0),
      weight: String(session.weight ?? 1),
      max_concurrency: String(session.max_concurrency ?? 0),
      models: (session.models || []).join(', '),
      extra_headers: Object.entries(session.extra_headers || {})
        .map(([k, v]) => `${k}: ${v}`)
        .join('\n'),
      usage_query_interval_secs: String(session.usage_query_interval_secs ?? 0),
      expires_at: session.expires_at ? String(session.expires_at) : '',
      clear_key: false,
      clear_proxy: false,
    });
    // 限制器草稿：读账号自带规则（没有 = 空），基线序列化用于保存时判断是否变更
    const rules = session.limiters ?? [];
    setLimiters(rules.map((rule) => ({ ...rule })));
    limitersBaseline.current = JSON.stringify(rules);
    // 条件组草稿（#29）：同基线策略；浅拷两层避免编辑时污染原对象
    const groups = session.limiter_groups ?? [];
    setLimiterGroupsDraft(groups.map((g) => ({ ...g, conditions: g.conditions.map((c) => ({ ...c })) })));
    limiterGroupsBaseline.current = JSON.stringify(groups);
    setDlgSeg('basic');
    setLimiterEditor(null);
    setLimiterError('');
    setLgEditor(null);
    setRawHeaders('');
    setShowModal(true);
  };

  const closeModal = (): void => {
    setShowModal(false);
    setEditing(null);
    setSaving(false);
    setLimiterEditor(null);
    setLgEditor(null);
  };

  const openBulk = (): void => {
    setBulk({
      ...EMPTY_BULK,
      channel_id: channelFilter || accountChannels[0]?.id || '',
    });
    setShowBulk(true);
  };

  // ── 账号路由（轮换策略）──
  const openRotation = async (): Promise<void> => {
    setRotationError('');
    try {
      const res = await api.accountRotationPolicy();
      const d = res?.data;
      if (d) {
        setRotation({
          base: String(d.base_cooldown_secs),
          max: String(d.max_cooldown_secs),
          auth: String(d.auth_cooldown_secs),
          attempts: String(d.max_attempts),
        });
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
    setShowRotation(true);
  };

  const saveRotation = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setRotationError('');
    setRotationSaving(true);
    try {
      const res = await api.saveAccountRotationPolicy({
        base_cooldown_secs: Math.max(5, Number(rotation.base) || 60),
        max_cooldown_secs: Math.max(5, Number(rotation.max) || 1800),
        auth_cooldown_secs: Math.max(60, Number(rotation.auth) || 900),
        max_attempts: Math.max(1, Number(rotation.attempts) || 5),
      });
      const d = res?.data;
      if (d) {
        setRotation({
          base: String(d.base_cooldown_secs),
          max: String(d.max_cooldown_secs),
          auth: String(d.auth_cooldown_secs),
          attempts: String(d.max_attempts),
        });
      }
      addToast(t('账号路由设置已保存'));
      setShowRotation(false);
    } catch (err) {
      setRotationError(err instanceof Error ? err.message : String(err));
    } finally {
      setRotationSaving(false);
    }
  };

  const splitModels = (raw: string): string[] =>
    raw
      .split(/[,\s]+/)
      .map((m) => m.trim())
      .filter((m) => m.length > 0);

  // 「额外请求头」文本 → 对象（每行 `Key: Value`，忽略空行/无冒号行）
  const parseHeaders = (raw: string): Record<string, string> => {
    const out: Record<string, string> = {};
    for (const line of raw.split('\n')) {
      const idx = line.indexOf(':');
      if (idx <= 0) continue;
      const name = line.slice(0, idx).trim();
      const value = line.slice(idx + 1).trim();
      if (name) out[name] = value;
    }
    return out;
  };

  /**
   * 解析抓包得到的原始请求头并回填表单：
   * - `Cookie` → 凭证（登录态型供应商的整段 cookie）
   * - `Authorization: Bearer xxx` → 凭证（JWT / API Key）
   * - 其余头 → 额外请求头（保留原始大小写）
   */
  const applyRawHeaders = (): void => {
    const extra: Record<string, string> = { ...parseHeaders(form.extra_headers) };
    let credential = form.api_key;
    for (const line of rawHeaders.split('\n')) {
      const idx = line.indexOf(':');
      if (idx <= 0) continue;
      const rawName = line.slice(0, idx).trim();
      const value = line.slice(idx + 1).trim();
      if (!rawName || !value) continue;
      const lower = rawName.toLowerCase();
      if (lower === 'cookie') {
        credential = value;
        continue;
      }
      if (lower === 'authorization') {
        const bearer = /^bearer\s+(.+)$/i.exec(value);
        if (bearer) {
          credential = bearer[1].trim();
          continue;
        }
      }
      extra[rawName] = value;
    }
    setForm((prev) => ({
      ...prev,
      api_key: credential,
      extra_headers: Object.entries(extra)
        .map(([k, v]) => `${k}: ${v}`)
        .join('\n'),
    }));
    setRawHeaders('');
    addToast(t('已填充凭证与额外请求头'));
  };

  const handleSave = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setError('');
    // 登录态型 / 自填地址型供应商必须提供上游地址
    if (!editing && selectedProvider?.requires_base_url && !form.base_url.trim()) {
      setError(t('该供应商需要填写上游地址'));
      return;
    }
    setSaving(true);
    try {
      let channelId = form.channel_id;
      // 没有账号型渠道时：按所选供应商自动创建渠道，避免「添加账号」点不动
      if (!editing && !channelId) {
        const name = `${selectedProviderLabel} ${t('账号池')}`.trim();
        const created = await api.addChannel({
          name,
          channel_type: 'account',
          base_url: '',
          models: [],
        });
        channelId = created?.data?.id != null ? String(created.data.id) : '';
        if (!channelId) {
          throw new Error(t('创建账号池失败'));
        }
      }
      const payload: Record<string, unknown> = {
        channel_id: channelId,
        name: form.name.trim(),
        provider: form.provider,
        base_url: form.base_url.trim(),
        chat_path: form.chat_path.trim(),
        status: form.status,
        priority: Number(form.priority || 0),
        // u32 字段：前端先夹紧，避免负数触发后端 422
        weight: Math.max(1, Number(form.weight || 1)),
        max_concurrency: Math.max(0, Number(form.max_concurrency || 0)),
        models: splitModels(form.models),
        extra_headers: parseHeaders(form.extra_headers),
        usage_query_interval_secs: Math.max(0, Number(form.usage_query_interval_secs || 0)),
        expires_at: form.expires_at.trim() ? Number(form.expires_at) : null,
      };
      // 代理三态：新增直传；更新时缺省保持原值、"" 清除、非空覆盖
      if (!editing) {
        payload.proxy = form.proxy.trim();
      } else if (form.clear_proxy) {
        payload.proxy = '';
      } else if (form.proxy.trim()) {
        payload.proxy = form.proxy.trim();
      }
      if (editing) {
        if (form.clear_key) {
          payload.api_key = '';
        } else if (form.api_key.trim()) {
          payload.api_key = form.api_key.trim();
        }
        await api.updateAccountSession(editing.id, payload);
        // 限制器随主保存之后单独 PATCH（校验失败/网络异常）时
        // 只提示本段失败，不拖累账号主体的保存
        if (JSON.stringify(limiters) !== limitersBaseline.current) {
          try {
            await api.patchAccountSession(editing.id, { limiters });
            limitersBaseline.current = JSON.stringify(limiters);
          } catch (limiterErr) {
            addToast(
              `${t('账号限制器保存失败')}：${limiterErr instanceof Error ? limiterErr.message : String(limiterErr)}`,
              'error',
            );
          }
        }
        // 条件组（#29）随限制器之后单独 PATCH：缺省=保持、空数组=清空回落旧规则
        if (JSON.stringify(limiterGroupsDraft) !== limiterGroupsBaseline.current) {
          try {
            await api.patchAccountSession(editing.id, { limiter_groups: limiterGroupsDraft });
            limiterGroupsBaseline.current = JSON.stringify(limiterGroupsDraft);
          } catch (lgErr) {
            addToast(
              `${t('条件组保存失败')}：${lgErr instanceof Error ? lgErr.message : String(lgErr)}`,
              'error',
            );
          }
        }
        addToast(t('账号已保存'));
      } else {
        payload.api_key = form.api_key.trim();
        // 弹窗内配置的限制规则/条件组随创建一并落库（有值才带，缺省 = 空）
        if (limiters.length > 0) payload.limiters = limiters;
        if (limiterGroupsDraft.length > 0) payload.limiter_groups = limiterGroupsDraft;
        await api.createAccountSession(payload);
        addToast(t('账号已创建'));
      }
      closeModal();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleBulk = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setError('');
    if (!bulk.channel_id) {
      setError(t('请选择供应商'));
      return;
    }
    if (!bulk.credentials.trim()) {
      setError(t('请粘贴至少一条凭证'));
      return;
    }
    setSaving(true);
    try {
      const res = await api.bulkImportAccountSessions({
        channel_id: bulk.channel_id,
        provider: bulk.provider,
        base_url: bulk.base_url.trim(),
        proxy: bulk.proxy.trim(),
        name_prefix: bulk.name_prefix.trim(),
        priority: Number(bulk.priority || 0),
        weight: Math.max(1, Number(bulk.weight || 1)),
        max_concurrency: Math.max(0, Number(bulk.max_concurrency || 0)),
        models: splitModels(bulk.models),
        credentials: bulk.credentials,
      });
      addToast(
        t('导入完成：成功 {{created}} 个，跳过 {{skipped}} 个', {
          created: res.data?.created ?? 0,
          skipped: res.data?.skipped ?? 0,
        })
      );
      setShowBulk(false);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  // ── 从本机客户端导入（cc-switch）──
  const [showImportScan, setShowImportScan] = useState(false);
  const [importScan, setImportScan] = useState<{
    available: boolean;
    path: string;
    reason: string;
    providers: ImportSourceEntry[];
  } | null>(null);
  const [importPicked, setImportPicked] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState(false);

  const openImportScan = async (): Promise<void> => {
    setShowImportScan(true);
    setImportScan(null);
    setImportPicked(new Set());
    try {
      const res = await api.scanImportSources();
      const data = res.data;
      if (data) {
        setImportScan({
          available: data.available ?? false,
          path: data.path ?? '',
          reason: data.reason ?? '',
          providers: data.providers ?? [],
        });
        // 默认勾选全部可导入项
        setImportPicked(
          new Set(
            (data.providers ?? [])
              .filter((p) => p.importable)
              .map((p) => p.id),
          ),
        );
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleRunImport = async (): Promise<void> => {
    if (!importScan) return;
    const picked = importScan.providers.filter((p) => importPicked.has(p.id));
    if (picked.length === 0) {
      addToast(t('请至少勾选一项'), 'error');
      return;
    }
    setImporting(true);
    try {
      const res = await api.runImport(
        picked.map((p) => ({
          id: p.id,
          name: p.name,
          base_url: p.baseUrl,
          api_key: p.apiKey,
          protocol: p.protocol,
          models: p.models ?? [],
          model_mappings: p.modelMappings ?? [],
        })),
      );
      const data = res.data;
      addToast(
        `${t('导入完成')}: ${data?.created_count ?? 0} ${t('个供应商')}` +
          (data?.error_count ? ` · ${data?.error_count} ${t('个失败')}` : ''),
      );
      setShowImportScan(false);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setImporting(false);
    }
  };

  // ── 上游模型目录发现（抄 Agent2API 的 provider 模型目录）──
  const [syncingId, setSyncingId] = useState<string | null>(null);
  const [catalogModal, setCatalogModal] = useState<CatalogSyncAccount[] | null>(null);

  const handleSyncCatalog = async (session: AccountSessionItem): Promise<void> => {
    setSyncingId(session.id);
    setError('');
    try {
      const res = await api.syncAccountCatalog({
        account_id: session.id,
        force: true,
        apply: true,
        replace: false,
      });
      const data = res.data;
      const ok = data?.results ?? [];
      const failed = data?.errors ?? [];
      if (ok.length > 0) {
        setCatalogModal(ok);
        // 清理反馈：后端同步后自动清掉的失效全局映射条数（0 条不显示）
        const removed = Number(data?.removed_mapping_count ?? 0);
        const first = ok[0];
        if (first.applied === false) {
          // 目录拉到了但**没能写回白名单**（账号未启用/凭证过期等）。此处
          // 必须报错而不是报「已同步」，否则用户以为同步成功、模型管理里却
          // 一行都没有（2026-11-11 生产实测：停用账号点拉取模型就是这样）。
          addToast(
            `${t('已拉取模型目录')} · ${first.count} ${t('个模型')} · ${t('未写入')}: `
              + (first.apply_error || t('账号未处于启用状态')),
            'error',
          );
        } else {
          addToast(
            t('已同步模型目录') + ` · ${first.count} ${t('个模型')}`
            + (removed > 0 ? ` · ${t('已清理失效映射 {{n}} 条', { n: removed })}` : ''),
          );
        }
        await load();
      }
      if (failed.length > 0) {
        addToast(`${t('同步失败')}: ${failed[0].error}`, 'error');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSyncingId(null);
    }
  };

  const handleTest = async (session: AccountSessionItem): Promise<void> => {
    setTestingId(session.id);
    setError('');
    try {
      const res = await api.testAccountSession(session.id);
      const data = res.data;
      if (data?.success) {
        addToast(`${t('测试成功')} · ${data.model || ''} · ${data.latency_ms ?? 0}ms`);
      } else {
        addToast(`${t('测试失败')}: ${data?.message || ''}`, 'error');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setTestingId(null);
    }
  };

  const handleResetCooldowns = async (session: AccountSessionItem): Promise<void> => {
    setError('');
    try {
      await api.resetAccountSessionCooldowns(session.id);
      addToast(t('冷却已清除'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  // 列表内快捷启停（PATCH 只改状态，不触碰地址/模型/代理/凭证）
  const handleToggleStatus = async (session: AccountSessionItem): Promise<void> => {
    const next = session.status === 'enabled' ? 'disabled' : 'enabled';
    setError('');
    try {
      await api.patchAccountSession(session.id, { status: next });
      addToast(next === 'enabled' ? t('已启用') : t('已停用'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleDelete = (session: AccountSessionItem): void => {
    setConfirmState({
      title: t('删除账号'),
      message: t('确定删除账号「{{name}}」？该账号的凭证与冷却记录会一并删除。', {
        name: session.name,
      }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteAccountSession(session.id);
          addToast(t('账号已删除'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  // ── 冷却展示（对齐 Agent2API：剩余秒本地倒推 + `*` 哨兵 = 整号冷却） ──

  /** 本地剩余秒数：抓取时刻的「剩余秒」减去已过时间，钳到 0 */
  const cooldownRemaining = (secs: number): number => {
    const elapsed = (Date.now() - cooldownLoadedAt.current) / 1000;
    return Math.max(0, Math.ceil(Number(secs) - elapsed));
  };

  /** 未到期冷却条目，按剩余时间升序（最早的恢复在最前） */
  const activeCooldowns = (session: AccountSessionItem): Array<[string, number]> =>
    Object.entries(session.cooldowns || {})
      .map(([model, secs]) => [model, cooldownRemaining(Number(secs))] as [string, number])
      .filter(([, rem]) => rem > 0)
      .sort((a, b) => a[1] - b[1]);

  /** 秒数 → 人话（`90s` / `5m 30s` / `1h 5m`，不带 i18n 单位） */
  const fmtDuration = (secs: number): string => {
    if (secs < 60) return `${secs}s`;
    if (secs < 3600) {
      const m = Math.floor(secs / 60);
      const s = secs % 60;
      return s ? `${m}m ${s}s` : `${m}m`;
    }
    const h = Math.floor(secs / 3600);
    return `${h}h ${Math.floor((secs % 3600) / 60)}m`;
  };

  // 清除单个模型的冷却标记（对齐 Agent2API rate-limits/clear 的 {model} 语义）
  const handleClearOneCooldown = async (session: AccountSessionItem, model: string): Promise<void> => {
    setError('');
    try {
      await api.resetAccountSessionCooldowns(session.id, model);
      addToast(t('已清除标记'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  /** 操作列「重置冷却」：只有单模型冷却时直接清；多模型时先弹菜单按模型挑
   *  （对齐 Agent2API 行内 clearRateLimits 可按模型的形态） */
  const onResetCooldownClick = (session: AccountSessionItem): void => {
    const active = activeCooldowns(session);
    if (active.length > 1) {
      setClearMenuFor(clearMenuFor === session.id ? null : session.id);
    } else {
      setClearMenuFor(null);
      void handleResetCooldowns(session);
    }
  };

  // ── 账号限制器编辑器（对齐 Agent2API accounts-dialog-limiter：列表 + 二级弹层）──

  /** 二级弹层的草稿更新（字段改动只落在草稿里，「保存规则」才上抛） */
  const updateLimiterDraft = (next: Partial<LimiterDraft>): void => {
    setLimiterError('');
    setLimiterEditor((prev) => (prev ? { ...prev, draft: { ...prev.draft, ...next } } : prev));
  };

  const openLimiterEditor = (index: number): void => {
    setLimiterError('');
    setLimiterEditor({ index, draft: limiterDraftOf(index >= 0 ? limiters[index] : null) });
  };

  const saveLimiterEditor = (): void => {
    if (!limiterEditor) return;
    const result = limiterRuleOfDraft(limiterEditor.draft);
    if ('error' in result) {
      setLimiterError(result.error);
      return;
    }
    const next = [...limiters];
    if (limiterEditor.index >= 0) next[limiterEditor.index] = result.rule;
    else next.push(result.rule);
    setLimiters(next);
    setLimiterEditor(null);
    setLimiterError('');
  };

  const removeLimiter = (index: number): void => {
    setLimiters(limiters.filter((_, i) => i !== index));
  };

  const toggleLimiter = (index: number, enabled: boolean): void => {
    setLimiters(limiters.map((rule, i) => (i === index ? { ...rule, enabled } : rule)));
  };

  // 运行态 → 本地化文案（与渠道管理的启用/禁用口径一致）
  const statusLabel = (status?: string): string => {
    if (status === 'enabled') return t('启用');
    if (status === 'expired') return t('凭证已过期');
    return t('禁用');
  };

  const successRate = (session: AccountSessionItem): string => {
    const ok = Number(session.success_count || 0);
    const failed = Number(session.error_count || 0);
    const total = ok + failed;
    if (total === 0) return '—';
    return `${((ok / total) * 100).toFixed(0)}%`;
  };

  // 最近错误过长会撑破单元格：截断展示，完整内容放 title
  const shortError = (raw?: string | null): string => {
    if (!raw) return '';
    return raw.length > 48 ? `${raw.slice(0, 48)}…` : raw;
  };

  const needle = search.trim().toLowerCase();
  /** 账号是否处于冷却中（任一上游真名还有剩余冷却秒数） */
  const isCooling = (s: AccountSessionItem): boolean =>
    Object.values(s.cooldowns || {}).some((v) => Number(v) > 0);
  /**
   * 「已限流」完整口径（SPEC §2.4 / 上游 accounts-domain.ts isLimitedNow）：
   * 模型冷却中，或限制器把账号拦在选路外（整号跳过；Token 限额禁用只对非启用账号有意义）。
   * 判据只用后端 limiter_state，不在前端按 limiters 重算阈值。
   */
  const isLimitedNow = (s: AccountSessionItem): boolean =>
    isCooling(s)
    || s.limiter_state?.account_skip === true
    || (s.limiter_state?.disable === true && s.status !== 'enabled');
  /** 状态分段：全部 / 启用（不含冷却）/ 禁用 / 冷却中 */
  const matchesStatusSeg = (s: AccountSessionItem): boolean => {
    if (statusSeg === 'enabled') return s.status === 'enabled' && !isCooling(s);
    if (statusSeg === 'disabled') return s.status !== 'enabled';
    if (statusSeg === 'cooling') return s.status === 'enabled' && isCooling(s);
    return true;
  };
  /** 限额分段：禁用账号既不算「正常」也不算「已限流」（SPEC §2.4） */
  const matchesLimitSeg = (s: AccountSessionItem): boolean => {
    if (limitSeg === 'all') return true;
    if (s.status !== 'enabled') return false;
    return limitSeg === 'limited' ? isLimitedNow(s) : !isLimitedNow(s);
  };
  /** 基础集合：渠道 + 提供商 + 搜索（两个分段计数的共同 scope） */
  const scopeBase = sessions.filter((s) => {
    if (channelFilter && s.channel_id !== channelFilter) return false;
    if (providerFilter && s.provider !== providerFilter) return false;
    if (!needle) return true;
    return (
      s.name.toLowerCase().includes(needle) ||
      (s.channel_name || '').toLowerCase().includes(needle) ||
      (s.provider || '').toLowerCase().includes(needle) ||
      (s.base_url || '').toLowerCase().includes(needle)
    );
  });
  // 双级计数（SPEC §2.4 / 上游 filterCounts）：
  // 状态段 scope = provider + 限额 + 搜索（不含自身状态维度）；
  // 限额段 scope = provider + 状态 + 搜索（不含自身限额维度）。
  const statusScope = scopeBase.filter(matchesLimitSeg);
  const segCounts = {
    all: statusScope.length,
    enabled: statusScope.filter((s) => s.status === 'enabled' && !isCooling(s)).length,
    disabled: statusScope.filter((s) => s.status !== 'enabled').length,
    cooling: statusScope.filter((s) => s.status === 'enabled' && isCooling(s)).length,
  };
  const limitScope = scopeBase.filter(matchesStatusSeg);
  const limitCounts = {
    all: limitScope.length,
    normal: limitScope.filter((s) => s.status === 'enabled' && !isLimitedNow(s)).length,
    limited: limitScope.filter((s) => s.status === 'enabled' && isLimitedNow(s)).length,
  };
  const visible = scopeBase.filter(matchesStatusSeg).filter(matchesLimitSeg);

  // 列表排序（纯展示序，不改调度 priority）
  const visibleSorted = useMemo(() => {
    if (sortKey === 'list') return visible;
    return [...visible].sort((a, b) => {
      if (sortKey === 'credit') {
        // credit：凭证/套餐到期升序（最急着过期最前），无到期值排最后（对齐上游 compareExpiry soon 档）
        const ea = accountExpiryOf(a).at ?? Number.POSITIVE_INFINITY;
        const eb = accountExpiryOf(b).at ?? Number.POSITIVE_INFINITY;
        return ea - eb;
      }
      // added：创建时间新→旧（对齐上游 compareAdded new 档；缺 created_at 按 0 = 最早）
      const ta = Number(a.created_at) || 0;
      const tb = Number(b.created_at) || 0;
      return tb - ta;
    });
  }, [visible, sortKey]);

  // ── 列显隐 / 列宽拖动（对齐 Agent2API accounts-columns + 模型管理同款交互）──

  /** 应用公共列设置：顺序 + 显隐（含列宽/对齐，公共层负责） */
  const appliedCols = useMemo(() => applyTableColumns(colState, AS_COL_DEFS), [colState]);
  /** 当前可见列 key（隐藏的已从 DOM 摘掉，colSpan / colgroup / minWidth 都按它算） */
  const visibleCols = useMemo<AsCol[]>(() => appliedCols.map((c) => c.key as AsCol), [appliedCols]);

  /** 列宽（colgroup 用）：弹性列没有覆盖时不写宽度，吃表格剩余宽度 */
  const widthOf = (key: AsCol): number | undefined => {
    const def = AS_COLS.find((c) => c.key === key);
    const applied = appliedCols.find((c) => c.key === key);
    return def?.flex ? applied?.width : applied?.width ?? def?.w;
  };

  /** 表格横滚底线：∑可见列宽（弹性列按下限），窄容器宁可横滚不压列 */
  const tableMinWidth = useMemo(
    () => visibleCols.reduce((sum, key) => {
      const override = widthOf(key);
      if (override != null) return sum + override;
      const def = AS_COLS.find((c) => c.key === key);
      return sum + (def?.flex ? AS_FLEX_MIN : def?.w ?? AS_MIN_COLW);
    }, 0),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [visibleCols, colState],
  );

  /** 列设置变更（面板回调；persist=true 才落盘） */
  const commitColState = useCallback((next: TableColumnItem[], persist: boolean) => {
    setColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);

  /** 恢复默认：顺序+显隐+对齐+列宽一并还原（公共层 reset） */
  const resetColConfig = useCallback((): void => {
    setColState(resetTableColumns(LS_TCOLS, AS_COL_DEFS));
  }, []);

  /** 列宽拖动改公共 hook（pointer events + 双击还原 + 末列无把手） */
  const colResize = useColumnResize({
    widths: Object.fromEntries(
      colState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number]),
    ),
    setWidths: (next) => {
      commitColState(
        colState.items.map((i) => {
          const w = next[i.key];
          const defW = AS_COL_DEFS.find((d) => d.key === i.key)?.width;
          // 与声明默认相等视为「还原」——删覆盖而不是存一个等于默认的值
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(
      AS_COL_DEFS.map((d) => [d.key, d.width]),
    ),
    lockedKeys: new Set(AS_COL_DEFS.filter((d) => d.locked).map((d) => d.key)),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });

  // 列设置浮层点外关闭
  useEffect(() => {
    if (!colPanelOpen) return undefined;
    const onDown = (e: MouseEvent): void => {
      if (colPanelRef.current && !colPanelRef.current.contains(e.target as Node)) {
        setColPanelOpen(false);
      }
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [colPanelOpen]);

  // 按模型清限流菜单点外关闭
  useEffect(() => {
    if (!clearMenuFor) return undefined;
    const onDown = (e: MouseEvent): void => {
      if (clearMenuRef.current && !clearMenuRef.current.contains(e.target as Node)) {
        setClearMenuFor(null);
      }
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [clearMenuFor]);

  // 分组带 ⋯ 菜单点外关闭
  useEffect(() => {
    if (!groupMenuFor) return undefined;
    const onDown = (e: MouseEvent): void => {
      if (groupMenuRef.current && !groupMenuRef.current.contains(e.target as Node)) {
        setGroupMenuFor(null);
      }
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [groupMenuFor]);

  // 操作列 ⋯ 聚合菜单点外关闭
  useEffect(() => {
    if (!actMenuFor) return undefined;
    const onDown = (e: MouseEvent): void => {
      if (actMenuRef.current && !actMenuRef.current.contains(e.target as Node)) {
        setActMenuFor(null);
      }
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [actMenuFor]);

  // 余额明细弹层点外关闭
  useEffect(() => {
    if (!balancePopFor) return undefined;
    const onDown = (e: MouseEvent): void => {
      if (balancePopRef.current && !balancePopRef.current.contains(e.target as Node)) {
        setBalancePopFor(null);
      }
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [balancePopFor]);

  // ── 分组折叠（对齐上游 toggleGroupCollapsed：收起/展开随点随存，跨次启动保留）──
  const toggleGroupCollapsed = (provider: string): void => {
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(provider)) next.delete(provider);
      else next.add(provider);
      writeLS(LS_COLLAPSED, [...next]);
      return next;
    });
  };

  /** 整组启用/禁用（对齐上游 setGroupEnabled：循环既有 PATCH 端点，含被筛选隐藏的行） */
  const runGroupEnabled = async (rows: AccountSessionItem[], enabled: boolean): Promise<void> => {
    const targets = rows.filter((r) => (r.status === 'enabled') !== enabled);
    if (!targets.length) return;
    setBatchRunning(true);
    setError('');
    try {
      const results = await Promise.allSettled(
        targets.map((r) => api.patchAccountSession(r.id, { status: enabled ? 'enabled' : 'disabled' })),
      );
      const ok = results.filter((r) => r.status === 'fulfilled').length;
      addToast(
        enabled
          ? t('整组启用完成：{{ok}}/{{total}}', { ok, total: targets.length })
          : t('整组停用完成：{{ok}}/{{total}}', { ok, total: targets.length }),
      );
      await load();
    } finally {
      setBatchRunning(false);
    }
  };

  /** 勾选（列表重载后清掉已不存在的 id，避免幽灵选择） */
  const togglePick = (id: string): void => {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };
  const togglePickAll = (): void => {
    setPicked((prev) =>
      prev.size === visible.length && visible.length > 0
        ? new Set()
        : new Set(visible.map((s) => s.id)),
    );
  };

  /** 批量启停/删除：循环既有 PATCH/DELETE 端点（账号量通常几十，无需专用批量端点） */
  const runBatch = async (action: 'enable' | 'disable' | 'delete'): Promise<void> => {
    const ids = [...picked];
    if (!ids.length) return;
    setBatchRunning(true);
    setError('');
    try {
      const results = await Promise.allSettled(
        ids.map((id) =>
          action === 'delete'
            ? api.deleteAccountSession(id)
            : api.patchAccountSession(id, { status: action === 'enable' ? 'enabled' : 'disabled' }),
        ),
      );
      const ok = results.filter((r) => r.status === 'fulfilled').length;
      addToast(t('批量{{action}}完成：{{ok}}/{{total}}', { action: action === 'delete' ? t('删除') : action === 'enable' ? t('启用') : t('停用'), ok, total: ids.length }));
      setPicked(new Set());
      await load();
    } finally {
      setBatchRunning(false);
    }
  };

  const askBatchDelete = (): void => {
    setConfirmState({
      title: t('批量删除账号'),
      message: t('确定删除选中的 {{count}} 个账号？凭证与冷却记录会一并删除。', { count: picked.size }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => { await runBatch('delete'); },
    });
  };

  /** 查询余额（后端支持 12 家；不支持的 provider 内联显示原因） */
  const handleBalance = useCallback(async (session: AccountSessionItem): Promise<void> => {
    setBalances((prev) => ({ ...prev, [session.id]: { state: 'loading' } }));
    try {
      const res = await api.getAccountSessionBalance(session.id);
      const d = res.data;
      const pool = usagePoolOf(d);
      const text = pool?.text ?? t('读数不可用');
      setBalances((prev) => ({ ...prev, [session.id]: { state: 'ok', pool, text, windows: d?.windows, raw: d ?? null } }));
    } catch (err) {
      setBalances((prev) => ({
        ...prev,
        [session.id]: { state: 'err', text: err instanceof Error ? err.message : String(err) },
      }));
    }
  }, [t]);

  /** 查询全部（仅支持余额的 provider；并发受限，避免同时打爆上游） */
  const queryAllBalances = useCallback(async (list: AccountSessionItem[]): Promise<void> => {
    const targets = list.filter((s) => BALANCE_PROVIDERS.has(s.provider));
    if (!targets.length) {
      addToast(t('当前列表没有支持余额查询的账号'));
      return;
    }
    const CONCURRENCY = 3;
    for (let i = 0; i < targets.length; i += CONCURRENCY) {
      // 并发窗口：每批最多 3 个，避免瞬间 N 个请求打向上游
      await Promise.all(targets.slice(i, i + CONCURRENCY).map((s) => handleBalance(s)));
    }
  }, [addToast, handleBalance, t]);

  // 首次加载后自动查询一次支持余额的账号（对齐 Agent2API 列表常驻余额读数；
  // 只查一次不做周期轮询，避免页面常驻打上游）。
  // 注意定义位置必须在 queryAllBalances 之后（hooks 求值顺序 = TDZ 安全）。
  useEffect(() => {
    if (autoBalanceDone.current || loading || !sessions.length) return;
    autoBalanceDone.current = true;
    void queryAllBalances(sessions);
  }, [loading, sessions, queryAllBalances]);

  // 自动余额周期轮询（对齐 Agent2API 的可配置间隔；用户在工具条选间隔，
  // 关闭时只保留首次那一次，避免无人值守时持续打上游）。
  const sessionsRef = useRef<AccountSessionItem[]>(sessions);
  sessionsRef.current = sessions;
  useEffect(() => {
    if (!autoBalanceMins) return undefined;
    const intervalMs = Math.max(30, autoBalanceMins * 60) * 1000;
    const timer = window.setInterval(() => {
      const list = sessionsRef.current;
      if (list.length) void queryAllBalances(list);
    }, intervalMs);
    return () => window.clearInterval(timer);
  }, [autoBalanceMins, queryAllBalances]);

  /** 手动刷新凭证（仅持有续期令牌的账号可用；force=true 无条件刷新） */
  const handleRefreshCredential = async (session: AccountSessionItem): Promise<void> => {
    setRefreshingId(session.id);
    setError('');
    try {
      const res = await api.refreshAccountSession(session.id, true);
      addToast(res.data?.message || t('凭证已刷新'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRefreshingId(null);
    }
  };

  /** 导出全部账号为 JSON（含明文凭证；浏览器触发下载） */
  const handleExport = async (): Promise<void> => {
    setError('');
    try {
      const res = await api.exportAccountSessions();
      const file = res.data;
      if (!file || !Array.isArray(file.accounts)) {
        throw new Error(t('导出数据为空'));
      }
      const blob = new Blob([JSON.stringify(file, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `aigx-accounts-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
      addToast(t('已导出 {{count}} 个账号（文件含明文凭证，请妥善保管）', { count: file.accounts.length }));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  /** 导入 JSON：选文件 → 解析 → 选渠道 → 上传 */
  const importFileRef = useRef<HTMLInputElement | null>(null);
  const [importJsonChannel, setImportJsonChannel] = useState('');
  const [importJsonEntries, setImportJsonEntries] = useState<Array<Record<string, unknown>> | null>(null);
  const [importJsonRunning, setImportJsonRunning] = useState(false);

  const handleImportFile = async (file: File): Promise<void> => {
    setError('');
    try {
      const text = await file.text();
      const parsed = JSON.parse(text) as { accounts?: Array<Record<string, unknown>> };
      if (!parsed || !Array.isArray(parsed.accounts) || parsed.accounts.length === 0) {
        throw new Error(t('文件里没有 accounts 数组'));
      }
      if (parsed.accounts.length > 500) {
        throw new Error(t('单次最多导入 500 个账号'));
      }
      setImportJsonEntries(parsed.accounts);
      setImportJsonChannel(channelFilter || accountChannels[0]?.id || '');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const runImportJson = async (): Promise<void> => {
    if (!importJsonEntries) return;
    setImportJsonRunning(true);
    setError('');
    try {
      const res = await api.importAccountSessionsJson({
        channel_id: importJsonChannel || undefined,
        entries: importJsonEntries,
      });
      const d = res.data ?? {};
      addToast(
        t('导入完成：新建 {{created}}、跳过 {{skipped}}', {
          created: d.created ?? 0,
          skipped: d.skipped ?? 0,
        }) + (d.errors?.length ? `（${d.errors.length} 条失败）` : ''),
      );
      setImportJsonEntries(null);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setImportJsonRunning(false);
    }
  };

  /** 拖拽排序：落点后把可见列表按新顺序重赋递减 priority（循环 PATCH 落盘）。
      语义对齐 Agent2API 队列：列表顺序 = 调度优先顺序（AIGX priority 越大越优先）。 */
  const handleDropRow = async (targetId: string): Promise<void> => {
    const srcId = dragId;
    setDragId(null);
    setDragOverId(null);
    if (!srcId || srcId === targetId || orderSaving) return;
    const ids = visible.map((s) => s.id);
    const from = ids.indexOf(srcId);
    const to = ids.indexOf(targetId);
    if (from < 0 || to < 0) return;
    const reordered = [...visible];
    const [moved] = reordered.splice(from, 1);
    reordered.splice(to, 0, moved);
    // 重赋 priority：队首最大，递减 10（留间隙便于以后插队）
    const base = reordered.length * 10;
    setOrderSaving(true);
    setError('');
    try {
      const results = await Promise.allSettled(
        reordered.map((s, i) =>
          api.patchAccountSession(s.id, { priority: base - i * 10 }),
        ),
      );
      const ok = results.filter((r) => r.status === 'fulfilled').length;
      if (ok < reordered.length) {
        addToast(t('排序保存：{{ok}}/{{total}} 成功', { ok, total: reordered.length }), 'error');
      } else {
        addToast(t('顺序已保存'));
      }
      await load();
    } finally {
      setOrderSaving(false);
    }
  };

  /** 组带拖拽（§2.6）：整组重排后把全量账号按新组序重赋 priority（队首最大、递减 10，
   *  与 handleDropRow 同一套口径）。用「全量账号」而不是可见行重排，避免筛选态下
   *  把隐藏账号的 priority 撞车。 */
  const handleDropGroup = async (targetProvider: string): Promise<void> => {
    const srcProvider = groupDragActive;
    setGroupDragActive(null);
    setGroupDragOver(null);
    if (!srcProvider || srcProvider === targetProvider || orderSaving) return;
    const order = groupedList.map((g) => g.provider);
    const from = order.indexOf(srcProvider);
    const to = order.indexOf(targetProvider);
    if (from < 0 || to < 0) return;
    const nextOrder = [...order];
    const [moved] = nextOrder.splice(from, 1);
    nextOrder.splice(to, 0, moved);
    const byProvider = (provider: string): AccountSessionItem[] =>
      sessions
        .filter((s) => (s.provider || 'unknown') === provider
          && (!channelFilter || s.channel_id === channelFilter))
        .sort((a, b) => (Number(b.priority) || 0) - (Number(a.priority) || 0));
    const sequence = nextOrder.flatMap(byProvider);
    if (!sequence.length) return;
    const base = sequence.length * 10;
    setOrderSaving(true);
    setError('');
    try {
      const results = await Promise.allSettled(
        sequence.map((s, i) => api.patchAccountSession(s.id, { priority: base - i * 10 })),
      );
      const ok = results.filter((r) => r.status === 'fulfilled').length;
      if (ok < sequence.length) {
        addToast(t('排序保存：{{ok}}/{{total}} 成功', { ok, total: sequence.length }), 'error');
      } else {
        addToast(t('顺序已保存'));
      }
      await load();
    } finally {
      setOrderSaving(false);
    }
  };

  /** 设为首选（§2.13）：priority 调到全池最大值 + 10（与行拖拽的队首口径一致） */
  const handleSetFirst = async (session: AccountSessionItem): Promise<void> => {
    const maxPriority = sessions
      .filter((s) => !channelFilter || s.channel_id === channelFilter)
      .reduce((max, s) => Math.max(max, Number(s.priority) || 0), 0);
    setError('');
    try {
      await api.patchAccountSession(session.id, { priority: maxPriority + 10 });
      addToast(t('已设为首选'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  /** 并发上限快捷（§2.13）：PATCH max_concurrency（0 = 不限） */
  const handleSetConcurrency = async (session: AccountSessionItem): Promise<void> => {
    const n = Math.max(0, Math.floor(Number(concDraft) || 0));
    setError('');
    try {
      await api.patchAccountSession(session.id, { max_concurrency: n });
      addToast(t('并发上限已更新'));
      setActMenuFor(null);
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    }
  };

  /** 行内领套餐（§2.12）：POST /api/checkin-center/run（单账号），toast 回执后刷新 */
  const handleClaim = async (session: AccountSessionItem): Promise<void> => {
    setClaimingId(session.id);
    setError('');
    try {
      const res = await api.runCheckin({ account_ids: [session.id] });
      const first = res.data?.results?.[0];
      addToast(first?.message || t('领取完成'), first && first.success === false ? 'error' : 'success');
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setClaimingId(null);
    }
  };

  /** 行内代理「选中即存」（§2.11）：成功后用后端返回的 data 回填本行；
   *  失败不改 state —— 受控 select 随下一次渲染回退到原值。 */
  const handleProxyChange = async (session: AccountSessionItem, value: string): Promise<void> => {
    setProxySavingId(session.id);
    setError('');
    try {
      const res = await api.patchAccountSession(session.id, { proxy: value });
      const data = res.data;
      const nextProxy = data && 'proxy' in data ? (data.proxy ?? null) : (value || null);
      setSessions((prev) => prev.map((x) => (x.id === session.id
        ? { ...x, proxy: nextProxy, proxy_state: data?.proxy_state ?? x.proxy_state }
        : x)));
      addToast(t('代理已保存'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setProxySavingId(null);
    }
  };

  /** §2.2 折叠全部 / 展开全部（仅分组模式渲染；两者都落 LS_COLLAPSED） */
  const collapseAllGroups = (): void => {
    const all = new Set(sessions.map((s) => s.provider || 'unknown'));
    setCollapsedGroups(all);
    writeLS(LS_COLLAPSED, [...all]);
  };

  const expandAllGroups = (): void => {
    setCollapsedGroups(new Set());
    writeLS(LS_COLLAPSED, []);
  };

  /** §2.3 清除筛选：只重置筛选维度，不动分组/排序/余额间隔/列设置 */
  const clearFilters = (): void => {
    setProviderFilter('');
    setSearch('');
    setStatusSeg('all');
    setLimitSeg('all');
  };

  const filterActive = providerFilter !== '' || search.trim() !== '' || statusSeg !== 'all' || limitSeg !== 'all';

  // ── 供应商选择（仅新增账号时使用）──
  const providerLabel = (kind: string): string =>
    providers.find((p) => p.kind === kind)?.label ?? kind;


  // 卡内「账号版本」（如 WorkBuddy 国内版/国际版）：form.provider 始终存真实
  // provider id，这里由它反推母卡与选中档位（对齐 Agent2API WorkBuddyBlock）
  const editionHost = cardKindOf(form.provider);
  const hostEditions = addConfigOf(editionHost)?.editions ?? [];
  const selectedEditionId = hostEditions.some((item) => item.id === form.provider)
    ? form.provider
    : (hostEditions[0]?.id || '');

  const selectedProviderLabel = providerLabel(form.provider);


  // 登录态型供应商（Cookie / JWT）：凭证输入标签与提示按鉴权方式变化
  const selectedProvider = providers.find((p) => p.kind === form.provider);
  const sessionAuth = selectedProvider?.auth ?? null;
  const credentialLabel =
    sessionAuth === 'cookie' ? t('登录 Cookie') : sessionAuth === 'bearer' ? t('访问令牌（JWT）') : t('凭证');
  const credentialHint = editing
    ? t('留空保持原凭证；勾选下方可清除')
    : selectedProvider?.credential_hint || undefined;

  // ── 分组模式（对齐 Agent2API accountGroups）：按 provider 聚组 ──
  // 组序 = 首次出现先后（列表本身已按调度优先序排列，组内保持同序）。
  const groupedList = useMemo<Array<{ provider: string; rows: AccountSessionItem[] }>>(() => {
    if (!grouped) return [];
    const groups: Array<{ provider: string; rows: AccountSessionItem[] }> = [];
    const indexOf = new Map<string, number>();
    for (const s of visibleSorted) {
      const key = s.provider || 'unknown';
      let gi = indexOf.get(key);
      if (gi == null) {
        gi = groups.length;
        indexOf.set(key, gi);
        groups.push({ provider: key, rows: [] });
      }
      groups[gi].rows.push(s);
    }
    return groups;
  }, [grouped, visibleSorted]);

  /** 组顺位（§2.6）：按分组顺序编号，跨页的组带重复时仍显示全局顺位 */
  const groupRankOf = useMemo(() => {
    const ranks = new Map<string, number>();
    groupedList.forEach((g, i) => ranks.set(g.provider, i + 1));
    return ranks;
  }, [groupedList]);

  // ── 分页（SPEC §2.1：作用在筛选+排序后的结果上）──
  /** 分组分页：每组尽量整组落在同一页；组本身超过一页时按页切块（组带重复出现）。 */
  const groupPages = useMemo<Array<Array<{ provider: string; rows: AccountSessionItem[] }>>>(() => {
    if (!grouped) return [];
    if (pageSize === 'all') return [groupedList];
    const pages: Array<Array<{ provider: string; rows: AccountSessionItem[] }>> = [];
    let cur: Array<{ provider: string; rows: AccountSessionItem[] }> = [];
    let used = 0;
    const flush = (): void => {
      if (cur.length) {
        pages.push(cur);
        cur = [];
        used = 0;
      }
    };
    for (const g of groupedList) {
      if (g.rows.length > pageSize) {
        flush();
        for (let i = 0; i < g.rows.length; i += pageSize) {
          const chunk: { provider: string; rows: AccountSessionItem[] } = { provider: g.provider, rows: g.rows.slice(i, i + pageSize) };
          if (chunk.rows.length === pageSize) pages.push([chunk]);
          else {
            cur = [chunk];
            used = chunk.rows.length;
          }
        }
        continue;
      }
      if (used > 0 && used + g.rows.length > pageSize) flush();
      cur.push(g);
      used += g.rows.length;
    }
    flush();
    return pages.length ? pages : [[]];
  }, [grouped, groupedList, pageSize]);
  const totalPages = grouped
    ? Math.max(1, groupPages.length)
    : pageSize === 'all'
      ? 1
      : Math.max(1, Math.ceil(visibleSorted.length / pageSize));
  // 筛选/搜索/排序/分组/每页档变化 → 回第 1 页（render 期收敛，同 TableShell 的派生模式）
  const pageResetKey = `${channelFilter}|${providerFilter}|${statusSeg}|${limitSeg}|${needle}|${sortKey}|${grouped}|${String(pageSize)}`;
  const [prevPageResetKey, setPrevPageResetKey] = useState(pageResetKey);
  if (prevPageResetKey !== pageResetKey) {
    setPrevPageResetKey(pageResetKey);
    if (rawPage !== 1) setRawPage(1);
  }
  const page = Math.min(Math.max(1, rawPage), totalPages);
  const pageGroups = grouped ? (groupPages[page - 1] ?? []) : [];
  const pageRows = grouped
    ? []
    : pageSize === 'all'
      ? visibleSorted
      : visibleSorted.slice((page - 1) * pageSize, page * pageSize);

  /** §2.8 账号详情气泡：本页数据能提供的「邮箱/标识、更新于、来源、桌面登录态」 */
  const accountTitleLines = (s: AccountSessionItem): string[] => {
    const lines: string[] = [];
    if (s.name) lines.push(t('账号：{{name}}', { name: nameMasked ? maskName(s.name) : s.name }));
    if (s.updated_at) {
      const raw = Number(s.updated_at);
      const at = raw < 1e11 ? raw * 1000 : raw;
      lines.push(t('更新于 {{time}}', { time: new Date(at).toLocaleString() }));
    }
    lines.push(t('来源：{{provider}}', { provider: providerLabel(s.provider) }));
    if (s.provider === 'desktop' || s.relogin_due_at != null) {
      lines.push(t('桌面端实时登录态'));
    }
    return lines;
  };

  /**
   * §2.2 模型可见性状态（SPEC-1.0.46）：账号行内展示后端 `models_status`。
   * 契约先写、后端未落地时不下发该字段 → 返回 null（不占位、不崩）。
   *
   * 紧凑形态：账号名单元格仅 220px，长中文句子 + 按钮会被 nowrap/overflow 裁出格
   * （T4 红 2：failed 态重试按钮 real desktop 点不到）。故行内只留「短标签 + 状态字形」，
   * 完整解释（含后端合成的长 error）搬到 title；failed 的重试做成图标按钮，
   * flex 布局下 `flex:0 0 auto` 保证永不被文本挤出。
   */
  const modelStatusNode = (s: AccountSessionItem): ReactNode => {
    const ms = s.models_status;
    if (!ms) return null;
    switch (ms.state) {
      case 'whitelist':
        return (
          <div
            className="form-hint as-model-status"
            title={t('模型清单：白名单 {{n}} 个', { n: ms.count })}
          >
            <span className="as-model-glyph ok" aria-hidden="true">✓</span>
            <span className="as-model-text">{t('白名单 {{n}}', { n: ms.count })}</span>
          </div>
        );
      case 'catalog':
        return (
          <div
            className="form-hint as-model-status"
            title={`${t('模型清单：目录发现 {{n}} 个', { n: ms.count })} · ${t('模型由上游目录自动发现；可在编辑弹窗写入白名单固化')}`}
          >
            <span className="as-model-glyph info" aria-hidden="true">⌛</span>
            <span className="as-model-text">{t('目录 {{n}}', { n: ms.count })}</span>
          </div>
        );
      case 'none':
        return (
          <div
            className="form-hint as-model-status"
            title={t('模型清单：无（该供应商不支持自动发现，请手填）')}
          >
            <span className="as-model-glyph muted" aria-hidden="true">⚠</span>
            <span className="as-model-text">{t('无模型')}</span>
          </div>
        );
      case 'failed':
        return (
          <div
            className="form-hint as-model-status as-model-status-err"
            title={t('模型同步失败：{{error}}', { error: ms.error || t('未知原因') })}
          >
            <span className="as-model-glyph err" aria-hidden="true">✗</span>
            <span className="as-model-text">{t('同步失败')}</span>
            <button
              type="button"
              className="btn btn-outline btn-sm as-model-retry"
              aria-label={t('重试')}
              title={t('重试')}
              onClick={() => void handleSyncCatalog(s)}
              disabled={syncingId === s.id}
            >
              {syncingId === s.id ? <Loader2 size={11} className="spin" /> : <RotateCcw size={11} />}
            </button>
          </div>
        );
      default:
        return null;
    }
  };

  /** §2.9 余额明细：wallets[] 逐桶 + windows[] 逐窗口；都空 → 「上游未返回明细」 */
  const balanceDetailRows = (bal: AccountBalance | null | undefined): ReactNode => {
    const wallets = Array.isArray(bal?.wallets) ? bal.wallets : [];
    const wins = bal?.windows ?? [];
    if (!wallets.length && !wins.length) {
      return <div className="as-pop-empty">{t('上游未返回明细')}</div>;
    }
    const unit = bal?.unit ? ` ${bal.unit}` : '';
    return (
      <>
        {wallets.length > 0 && (
          <>
            <div className="as-pop-section">{t('额度桶')}</div>
            {wallets.map((w, i) => {
              const balance = Number(w?.balance);
              const total = Number(w?.total) || 0;
              const used = Number(w?.used) || 0;
              return (
                <div key={`w-${i}`} className="as-wallet-row">
                  <span className="as-wallet-name">{String(w?.displayName || w?.type || t('额度'))}</span>
                  <span className="as-wallet-view">
                    {Number.isFinite(balance) ? `${balance}${unit}` : '—'}
                    {total > 0 ? ` / ${total}${unit}` : ''}
                    {used > 0 ? ` · ${t('已用')} ${used}${unit}` : ''}
                  </span>
                </div>
              );
            })}
          </>
        )}
        {wins.length > 0 && (
          <>
            <div className="as-pop-section">{t('用量窗口')}</div>
            {wins.map((w, i) => {
              const label = usageWindowTitle(w);
              const pct = typeof w.remaining_percent === 'number' ? Math.round(w.remaining_percent) : null;
              const remaining = typeof w.remaining === 'number' ? w.remaining : null;
              const reset = w.resets_at && w.resets_at > 0 ? new Date(w.resets_at * 1000).toLocaleString() : '';
              return (
                <div key={w.key || `win-${i}`} className="as-wallet-row">
                  <span className="as-wallet-name">{label}</span>
                  <span className="as-wallet-view">
                    {pct != null ? t('剩 {{pct}}%', { pct }) : ''}
                    {remaining != null ? ` · ${remaining}` : ''}
                    {reset ? ` · ${t('重置于 {{time}}', { time: reset })}` : ''}
                  </span>
                </div>
              );
            })}
          </>
        )}
      </>
    );
  };

  // ── 表格单元格渲染（列配置驱动：列显隐 / 列宽 / 分组模式共用一套）──
  const renderCell = (col: AsCol, s: AccountSessionItem, orderIdx: number): ReactNode => {
    switch (col) {
      case 'pick':
        return (
          <td key={col} className="pick-col">
            <input
              type="checkbox"
              aria-label={t('选择账号')}
              checked={picked.has(s.id)}
              onChange={() => togglePick(s.id)}
            />
          </td>
        );
      case 'name': {
        const titleLines = accountTitleLines(s);
        return (
          <td key={col} className="as-c-name">
            {/* §2.8 悬停气泡：名称上显示账号详情（邮箱/标识、更新于、来源、桌面登录态） */}
            <div className="as-name" title={titleLines.length ? titleLines.join('\n') : undefined}>
              {nameMasked ? maskName(s.name) : s.name}
            </div>
            <div className="form-hint">{s.api_key || '—'}</div>
            {s.proxy && <div className="form-hint">{s.proxy}</div>}
            {s.last_error && (
              <div className="form-hint" title={s.last_error}>
                {shortError(s.last_error)}
              </div>
            )}
            {modelStatusNode(s)}
          </td>
        );
      }
      case 'provider':
        return (
          <td key={col} className="as-c-provider">
            <div className="provider-cell">
              {providerIcon(s.provider) && (
                <img
                  className="provider-card-logo"
                  src={providerIcon(s.provider) as string}
                  alt=""
                />
              )}
              <span>{s.provider}</span>
            </div>
            {s.base_url && <div className="form-hint">{s.base_url}</div>}
          </td>
        );
      case 'proxy': {
        // §2.11 行内代理下拉「选中即存」：直连 + 代理池条目 + 轮询全部启用 + 当前未知值补位
        const raw = (s.proxy || '').trim();
        const options: Array<{ value: string; label: string }> = [{ value: '', label: t('直连') }];
        for (const it of proxyPool ?? []) {
          options.push({
            value: `pool:${it.id}`,
            label: `${it.name}${it.enabled ? '' : ` · ${t('已禁用')}`}`,
          });
        }
        options.push({ value: 'rotate', label: t('轮询全部启用') });
        if (raw && !options.some((o) => o.value === raw)) {
          options.push({ value: raw, label: t('自定义：{{value}}', { value: raw }) });
        }
        const proxyOk = s.proxy_state?.ok !== false;
        return (
          <td key={col} className="as-c-proxy">
            <div className="as-proxy-cell">
              <select
                className="form-input as-proxy-select"
                aria-label={t('出口代理')}
                value={raw}
                disabled={proxySavingId === s.id}
                title={
                  s.proxy_state?.ok === false
                    ? t('代理不可用：{{error}}', { error: s.proxy_state.error || '' })
                    : t('代理可用')
                }
                onChange={(e) => void handleProxyChange(s, e.target.value)}
              >
                {options.map((o) => (
                  <option key={o.value} value={o.value}>{o.label}</option>
                ))}
              </select>
              {/* 状态点：绿 = 代理解析可用；红 = 引用失效（title 显示原因） */}
              <span
                className={`as-proxy-dot ${proxyOk ? 'ok' : 'err'}`}
                aria-hidden="true"
                title={proxyOk ? undefined : (s.proxy_state?.error || undefined)}
              />
            </div>
          </td>
        );
      }
      case 'status': {
        // §2.10 健康标签（有则该标签，无则不占位）：凭证 / 代理 / Token 限额禁用
        const healthTags: ReactNode[] = [];
        if (!s.api_key || s.api_key.trim() === '' || s.api_key.trim() === '—') {
          healthTags.push(
            <span key="cred" className="as-status-tag" title={t('账号没有可用凭证，转发会失败')}>
              {t('未配置凭证')}
            </span>,
          );
        }
        if (s.proxy_state?.ok === false) {
          healthTags.push(
            <span
              key="proxy"
              className="as-status-tag warn"
              title={t('代理不可用：{{error}}', { error: s.proxy_state.error || '' })}
            >
              {t('代理异常')}
            </span>,
          );
        }
        if (s.status !== 'enabled' && s.limiter_state?.disable === true) {
          healthTags.push(
            <span
              key="token"
              className="as-status-tag bad"
              title={t('Token 限额命中且账号已被自动禁用（窗口重置也不恢复，需手动启用）')}
            >
              {t('Token 限额已禁用')}
            </span>,
          );
        }
        return (
          <td key={col} className="as-c-status">
            {/* 行内启停 Switch（对齐 Agent2API StatusCell：直接 PATCH 状态，无需进操作列） */}
            <div className="as-status-line">
              <Switch
                className="as-status-switch"
                checked={s.status === 'enabled'}
                disabled={s.status === 'expired'}
                ariaLabel={t('启用/停用')}
                title={
                  s.status === 'expired'
                    ? t('凭证已过期，请先更新凭证或到期时间')
                    : s.status === 'enabled'
                      ? t('停用')
                      : t('启用')
                }
                onChange={() => void handleToggleStatus(s)}
              />
              <span className={`badge ${statusClass(s.status)}`}>
                {statusLabel(s.status)}
              </span>
            </div>
            {healthTags.length > 0 && <div className="as-status-tags">{healthTags}</div>}
          </td>
        );
      }
      case 'priority':
        return (
          <td key={col} className="as-c-priority">
            {/* §2.7 组内拖拽把手：整行 draggable 的视觉锚点（分组模式行拖拽仍关闭） */}
            <span className="as-grip" title={t('拖动调整组内顺序')}>
              <GripVertical size={12} />
            </span>
            {orderIdx >= 0 ? (
              // 分组模式：格内改显组内序号（对齐 Agent2API OrderCell 的 #N）
              <span
                className="as-order"
                title={t('组内顺序 · 优先级 {{p}} / 权重 {{w}}', { p: s.priority ?? 0, w: s.weight ?? 1 })}
              >
                #{orderIdx + 1}
              </span>
            ) : (
              <span>{s.priority ?? 0} / w{s.weight ?? 1}</span>
            )}
          </td>
        );
      case 'conn':
        return (
          <td
            key={col}
            className="as-c-conn"
            title={t('此刻正在使用该账号的请求数（2 秒轮询）')}
          >
            {(() => {
              const cur = connCounts[s.id] ?? s.in_flight ?? 0;
              if (!cur && !s.max_concurrency) return <span className="form-hint">—</span>;
              return (
                <span className="as-conn-cell">
                  <span className={`as-conn${cur > 0 ? '' : ' as-conn-zero'}`}>{cur}</span>
                  {s.max_concurrency ? <span className="form-hint">/{s.max_concurrency}</span> : null}
                </span>
              );
            })()}
          </td>
        );
      case 'expiry': {
        // 凭证有效期列（对齐上游 ExpiryCell：收短「N 天后」，完整时刻进 title；
        // 套餐到期次选 —— 凭证没有到期值时用它顶上，区分来源）
        const exp = accountExpiryOf(s);
        const pending = pendingPlansOf(s);
        if (exp.at == null && pending.length === 0) {
          return (
            <td key={col} className="as-c-expiry">
              <span className="form-hint" title={t('记录里没有过期时间')}>—</span>
            </td>
          );
        }
        const left = exp.at != null ? exp.at - Date.now() : null;
        return (
          <td key={col} className="as-c-expiry">
            {left != null && left <= 0 ? (
              <span className="badge badge-danger" title={t('凭证已过期；更新凭证或到期时间后自动恢复')}>
                {t('已过期')}
              </span>
            ) : left != null ? (
              <span
                className={`as-expiry-left${left < 48 * 3600_000 ? ' soon' : ''}`}
                title={`${expiryLeftText(left)}过期（${new Date(exp.at as number).toLocaleString()}）${exp.source === 'plan' ? ` · ${t('套餐到期，非凭证到期')}` : ''}`}
              >
                {expiryLeftText(left)}
              </span>
            ) : null}
            {pending.length > 0 && (
              <span
                className="as-pending-badge"
                title={t('已领到、还没到生效时间的套餐（生效后余额列会按它显示）：{{lines}}', {
                  lines: pending
                    .map((p) => {
                      const name = String(p.name || p.planId || t('套餐'));
                      const at = expiryMillis(p.pendingUntil ?? p.startsAt);
                      return at != null
                        ? t('{{name}}（{{time}} 起生效）', { name, time: new Date(at).toLocaleString() })
                        : name;
                    })
                    .join('\n'),
                })}
              >
                {t('待生效 {{n}}', { n: pending.length })}
              </span>
            )}
          </td>
        );
      }
      case 'usage': {
        // 余额列（主额度桶两行形态：套餐名 + 进度条 + 读数，对齐 Agent2API .usage-pool）
        const bal = balances[s.id];
        if (bal?.state === 'loading') {
          return <td key={col} className="as-c-usage"><span className="form-hint">{t('查询中…')}</span></td>;
        }
        if (bal?.state === 'err') {
          return <td key={col} className="as-c-usage"><span className="form-hint" title={bal.text}>{t('余额不可用')}</span></td>;
        }
        const pool = bal?.state === 'ok' ? bal.pool : usagePoolOf(s.last_balance);
        const ago = !bal && s.last_balance_at ? formatAgo(s.last_balance_at) : '';
        // §2.9 明细数据源：本次查询返回的 raw 优先，其次列表自带的余额快照
        const rawBal = bal?.state === 'ok' ? (bal.raw ?? s.last_balance ?? null) : (s.last_balance ?? null);
        // §2.9 跳过徽章：判据只用后端 limiter_state，不按 limiters 重算阈值。
        // 细分「余额 / Token」用 spec 允许的近似：skip_models 为空 = 整号命中（余额类）。
        const skip = s.limiter_state;
        const skipModels = skip?.skip_models ?? [];
        const balanceSkipped = skip?.account_skip === true
          && s.last_balance?.available != null
          && skipModels.length === 0;
        const tokenSkipped = skipModels.length > 0;
        const popOpen = balancePopFor === s.id;
        return (
          <td key={col} className="as-c-usage">
            <div className="as-usage-wrap" ref={popOpen ? balancePopRef : undefined}>
              {pool ? (
                // 点击余额格 → 明细弹层（§2.9：wallets 逐桶 + windows 逐窗口）
                <button
                  type="button"
                  className="as-usage-btn"
                  title={t('点击查看余额明细')}
                  onClick={() => setBalancePopFor(popOpen ? null : s.id)}
                >
                  <div className="as-usage-pool">
                    {pool.plan && (
                      <span className="as-usage-pool-name" title={pool.plan}>{pool.plan}</span>
                    )}
                    <div className="as-usage-pool-line">
                      {pool.ratio != null && (
                        <span className="as-usage-pool-bar" aria-hidden="true">
                          <span style={{ width: `${Math.round(pool.ratio * 100)}%` }} />
                        </span>
                      )}
                      <span className={`as-usage-pool-view${pool.low ? ' warn' : ' ok'}`}>
                        {pool.text}
                        {ago ? `（${ago}）` : ''}
                      </span>
                    </div>
                    {pool.low && <div className="balance-low">{t('不足，请充值或停用')}</div>}
                  </div>
                </button>
              ) : (
                <button
                  type="button"
                  className="balance-link"
                  onClick={() => void handleBalance(s)}
                >
                  <Wallet size={12} /> {t('查询余额')}
                </button>
              )}
              {balanceSkipped && (
                <span
                  className="as-skip-badge"
                  title={t('余额低于限制器的阈值，转发时会跳过该账号（余额回升自动恢复）')}
                >
                  {t('余额不足 · 已跳过')}
                </span>
              )}
              {tokenSkipped && (
                <span
                  className="as-skip-badge"
                  title={t('本周期 Token 消耗达到限制器的上限，转发时会跳过该账号（窗口重置自动恢复）')}
                >
                  {t('Token 限额 · 已跳过')}
                </span>
              )}
              {popOpen && (
                <div className="as-usage-pop" role="dialog" aria-label={t('余额明细')}>
                  <div className="as-pop-title">
                    <span>{t('余额明细')}</span>
                    <button
                      type="button"
                      className="as-pop-close"
                      aria-label={t('关闭')}
                      onClick={() => setBalancePopFor(null)}
                    >
                      ✕
                    </button>
                  </div>
                  {balanceDetailRows(rawBal)}
                </div>
              )}
            </div>
            {!bal && s.last_balance_error && (
              <div className="form-hint" title={s.last_balance_error}>{t('自动查询失败')}</div>
            )}
            {tokenSkipped && (
              <div className="form-hint as-token-skip-hint" title={skipModels.join(', ')}>
                {t('Token 限额 {{n}} 个模型已跳过', { n: skipModels.length })}
              </div>
            )}
            {(() => {
              // 分窗口额度读数（上游 usage windows；后端用量窗口端点起下发，
              // 旧后端无 windows 字段 → 不渲染，余额格维持主额度桶形态）
              const wins = bal?.state === 'ok' ? (bal.windows ?? []) : (s.last_balance?.windows ?? []);
              if (!wins.length) return null;
              return (
                <div className="as-usage-windows">
                  {wins.map((w, i) => {
                    const pct = typeof w.remaining_percent === 'number'
                      ? Math.max(0, Math.min(100, w.remaining_percent))
                      : null;
                    const unlimited = w.unlimited === true;
                    const tone = unlimited || (pct != null && pct >= 20) ? 'ok' : pct != null ? 'warn' : 'muted';
                    const reset = w.resets_at && w.resets_at > 0
                      ? new Date(w.resets_at * 1000).toLocaleString()
                      : '';
                    const label = usageWindowTitle(w);
                    return (
                      <div
                        key={w.key || `${w.group ?? ''}-${w.window ?? ''}-${i}`}
                        className={`as-usage-window ${tone}`}
                        title={reset ? `${label} · ${t('重置于 {{time}}', { time: reset })}` : label}
                      >
                        <span className="as-usage-window-label">{label}</span>
                        <span className="as-usage-window-bar" aria-hidden="true">
                          <span style={pct != null ? { width: `${Math.round(pct)}%` } : undefined} />
                        </span>
                        <span className="as-usage-window-view">
                          {unlimited ? t('不限') : pct != null ? t('剩 {{pct}}%', { pct: Math.round(pct) }) : t('读数未知')}
                        </span>
                      </div>
                    );
                  })}
                </div>
              );
            })()}
          </td>
        );
      }
      case 'rate':
        return <td key={col} className="as-c-rate">{successRate(s)}</td>;
      case 'cooldown':
        return (
          <td key={col} className="as-c-cooldown">
            {(() => {
              const active = activeCooldowns(s);
              if (active.length === 0) {
                return (
                  <span className="as-ok">
                    <span className="as-ok-dot" aria-hidden="true" />
                    {t('正常')}
                  </span>
                );
              }
              const whole = active.some(([model]) => model === '*');
              const open = cooldownOpenId === s.id;
              return (
              <Tooltip label={t('点击展开完整冷却明细')}>
                <button
                  type="button"
                  className={`as-limits-badge${open ? ' is-open' : ''}`}
                  onClick={() => setCooldownOpenId(open ? null : s.id)}
                >
                  <span className="as-limits-main">
                    {whole
                      ? t('整号冷却')
                      : t('{{n}} 个模型', { n: active.length })}
                    <span className="as-limits-caret">{open ? '▴' : '▾'}</span>
                  </span>
                  <span className="as-limits-sub">
                    {t('最早 {{time}} 恢复', { time: fmtDuration(active[0][1]) })}
                  </span>
                </button>
              </Tooltip>
              );
            })()}
          </td>
        );
      case 'act': {
        const maxPriority = sessions
          .filter((x) => !channelFilter || x.channel_id === channelFilter)
          .reduce((max, x) => Math.max(max, Number(x.priority) || 0), 0);
        const atFirst = (Number(s.priority) || 0) >= maxPriority && maxPriority > 0;
        const menuOpen = actMenuFor === s.id;
        return (
          <td key={col} className="as-c-act">
            <div className="actions-cell as-actions">
              <button
                type="button"
                className="as-act"
                onClick={() => handleTest(s)}
                disabled={testingId === s.id}
                title={testingId === s.id ? t('测试中…') : t('沿真实链路发一条最小对话验证可用性')}
              >
                <FlaskConical size={13} />
              </button>
              <button
                type="button"
                className="as-act"
                onClick={() => void handleSyncCatalog(s)}
                disabled={syncingId === s.id}
                title={t('从上游拉取模型清单并写入本账号')}
              >
                {syncingId === s.id ? <Loader2 size={13} className="spin" /> : <Download size={13} />}
              </button>
              <Tooltip label={t('编辑')}>
                <button
                  type="button"
                  className="as-act"
                  aria-label={t('编辑')}
                  onClick={() => openEdit(s)}
                >
                  <Pencil size={13} />
                </button>
              </Tooltip>
              {/* 重置冷却：单模型直接清；多模型先弹菜单按模型挑（对齐 Agent2API 可按模型清除） */}
              <div
                className="as-act-wrap"
                ref={clearMenuFor === s.id ? clearMenuRef : undefined}
              >
                <button
                  type="button"
                  className="as-act"
                  onClick={() => onResetCooldownClick(s)}
                  disabled={Object.keys(s.cooldowns || {}).length === 0}
                  title={t('重置冷却（多模型冷却时可按模型清除）')}
                >
                  <RotateCcw size={13} />
                </button>
                {clearMenuFor === s.id && (
                  <div className="as-clear-menu">
                    <button
                      type="button"
                      className="as-clear-menu-item"
                      onClick={() => {
                        setClearMenuFor(null);
                        void handleResetCooldowns(s);
                      }}
                    >
                      {t('全部清除')}
                    </button>
                    {activeCooldowns(s).map(([model, rem]) => (
                      <button
                        key={model}
                        type="button"
                        className="as-clear-menu-item"
                        onClick={() => {
                          setClearMenuFor(null);
                          void handleClearOneCooldown(s, model);
                        }}
                      >
                        <code>{model === '*' ? t('全部模型') : model}</code>
                        <span className="form-hint">{fmtDuration(rem)}</span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
              {/* §2.12 领套餐：仅 checkin 白名单 provider 出现（白名单与后端同步维护） */}
              {CHECKIN_PROVIDERS.has(s.provider) && (
                <button
                  type="button"
                  className="as-act"
                  onClick={() => void handleClaim(s)}
                  disabled={claimingId === s.id}
                  title={t('替该账号领取今日套餐 / 签到')}
                  aria-label={t('领套餐')}
                >
                  {claimingId === s.id ? <Loader2 size={13} className="spin" /> : <Gift size={13} />}
                </button>
              )}
              {/* §2.13 ⋯ 聚合菜单：刷新 Token / 设为首选 / 并发上限 / 删除（删除仍二次确认） */}
              <div className="as-act-menu-wrap" ref={menuOpen ? actMenuRef : undefined}>
                <button
                  type="button"
                  className="as-act"
                  aria-label={t('更多操作')}
                  title={t('更多操作')}
                  onClick={() => {
                    setActMenuFor(menuOpen ? null : s.id);
                    setConcDraft(String(s.max_concurrency ?? 0));
                  }}
                >
                  <MoreHorizontal size={13} />
                </button>
                {menuOpen && (
                  <div className="as-act-menu">
                    {/* 手动刷新凭证：仅持有续期令牌的账号可用（qoder/autoclaw/trae/codearts 等） */}
                    <button
                      type="button"
                      className="as-act-menu-item"
                      onClick={() => {
                        setActMenuFor(null);
                        void handleRefreshCredential(s);
                      }}
                      disabled={!s.has_refresh_token || refreshingId === s.id}
                      title={
                        s.has_refresh_token
                          ? t('刷新凭证（用续期令牌换新 access token）')
                          : t('该账号没有续期令牌，无法在线刷新')
                      }
                    >
                      {t('刷新 Token')}
                    </button>
                    <button
                      type="button"
                      className="as-act-menu-item"
                      onClick={() => {
                        setActMenuFor(null);
                        void handleSetFirst(s);
                      }}
                      disabled={atFirst}
                      title={
                        atFirst
                          ? t('已在全局队列第一位')
                          : t('把优先级调到全池最高，不改变启用状态')
                      }
                    >
                      {t('设为首选')}
                    </button>
                    <div className="as-act-menu-row">
                      <span>{t('并发上限')}</span>
                      <input
                        className="form-input as-act-menu-input"
                        type="number"
                        min={0}
                        value={concDraft}
                        aria-label={t('并发上限')}
                        onChange={(e) => setConcDraft(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') {
                            e.preventDefault();
                            void handleSetConcurrency(s);
                          }
                        }}
                      />
                      <button
                        type="button"
                        className="as-act-menu-apply"
                        onClick={() => void handleSetConcurrency(s)}
                      >
                        {t('应用')}
                      </button>
                    </div>
                    <div className="as-act-menu-sep" />
                    <button
                      type="button"
                      className="as-act-menu-item danger"
                      onClick={() => {
                        setActMenuFor(null);
                        handleDelete(s);
                      }}
                    >
                      {t('删除账号')}
                    </button>
                  </div>
                )}
              </div>
            </div>
          </td>
        );
      }
      default:
        return null;
    }
  };

  /** 行渲染：分组模式下禁用整行拖拽（组间顺序由分组带表达，组内拖拽语义另做） */
  const renderRow = (s: AccountSessionItem, orderIdx: number): JSX.Element => (
    <tr
      key={s.id}
      className={dragOverId === s.id && dragId !== s.id ? 'drag-over' : ''}
      draggable={!grouped}
      onDragStart={(e) => {
        // 行内表单控件（代理下拉等）不触发整行拖拽，避免改选择时误拖行
        if ((e.target as Element).closest('select, input')) {
          e.preventDefault();
          return;
        }
        setDragId(s.id);
        e.dataTransfer.effectAllowed = 'move';
      }}
      onDragOver={(e) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'move';
        if (dragOverId !== s.id) setDragOverId(s.id);
      }}
      onDrop={(e) => {
        e.preventDefault();
        void handleDropRow(s.id);
      }}
      onDragEnd={() => {
        setDragId(null);
        setDragOverId(null);
      }}
      title={grouped ? undefined : t('拖拽行可调整调度顺序')}
    >
      {visibleCols.map((col) => {
        const cell = renderCell(col, s, orderIdx);
        // §1 列对齐修复：td 也要挂 ta-<align>（此前只有列设置面板存了 align，渲染从未使用）
        const align = appliedCols.find((c) => c.key === col)?.align ?? 'left';
        if (!isValidElement(cell)) return cell;
        const el = cell as ReactElement<{ className?: string }>;
        return cloneElement(el, {
          className: [el.props.className, `ta-${align}`].filter(Boolean).join(' '),
        });
      })}
    </tr>
  );

  if (loading) return <SkeletonTable columns={7} rows={5} />;

  return (
    <div>
      {!embedded && (
        <div className="page-header">
          <h1>{t('账号池管理')}</h1>
          <p>
            {t(
              '为账号池型供应商管理多把上游凭证：账号级优先级/权重/并发上限与按上游模型的冷却，失败自动轮换'
            )}
          </p>
        </div>
      )}

      {error && <div className="error-message">{error}</div>}

      {accountChannels.length === 0 && (
        <div className="error-message">
          {t('还没有账号池：添加账号并保存时会按所选供应商自动创建账号池')}
        </div>
      )}

      {stats.length > 0 && (
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8, marginBottom: 12 }}>
          {stats.map((st) => (
            <span key={st.channel_id} className="badge badge-info">
              {st.channel_name || st.channel_id}: {st.enabled}/{st.total}
              {st.cooling > 0 ? ` · ${t('冷却中')} ${st.cooling}` : ''}
              {st.in_flight > 0 ? ` · ${t('并发')} ${st.in_flight}` : ''}
            </span>
          ))}
        </div>
      )}

      <Card
        title={`${t('账号池')} (${visible.length})`}
        actions={
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <div className="as-colset" ref={colPanelRef}>
              <Tooltip label={t('列设置（显隐 / 顺序 / 对齐 / 列宽拖动）')}>
                <button
                  type="button"
                  className={`btn btn-outline btn-sm${colPanelOpen ? ' as-colset-open' : ''}`}
                  onClick={() => setColPanelOpen((v) => !v)}
                  aria-label={t('列设置')}
                >
                  <Columns3 size={14} />
                </button>
              </Tooltip>
              {colPanelOpen && (
                <TableColumnSettings
                  open
                  onClose={() => setColPanelOpen(false)}
                  defs={AS_COL_DEFS}
                  items={colState.items}
                  onChange={commitColState}
                  onReset={resetColConfig}
                  anchorRef={colPanelRef}
                />
              )}
            </div>
            <Button variant="outline" onClick={() => void queryAllBalances(visible)}>
              <Wallet size={14} /> {t('查询全部余额')}
            </Button>
            <Button variant="outline" onClick={() => void handleExport()} disabled={sessions.length === 0}>
              <Download size={14} /> {t('导出')}
            </Button>
            <Button
              variant="outline"
              onClick={() => importFileRef.current?.click()}
              disabled={accountChannels.length === 0}
            >
              <Upload size={14} /> {t('导入 JSON')}
            </Button>
            {/* 隐藏的文件选择器：解析后弹渠道选择确认 */}
            <input
              ref={importFileRef}
              type="file"
              accept="application/json,.json"
              style={{ display: 'none' }}
              onChange={(e) => {
                const f = e.target.files?.[0];
                if (f) void handleImportFile(f);
                e.target.value = '';
              }}
            />
            <Button variant="outline" onClick={() => void openRotation()}>
              <SlidersHorizontal size={14} /> {t('账号路由')}
            </Button>
            <Button variant="outline" onClick={() => void openImportScan()}>
              <Download size={14} /> {t('从本机导入')}
            </Button>
            <Button variant="outline" onClick={openBulk} disabled={accountChannels.length === 0}>
              <Upload size={14} /> {t('批量导入')}
            </Button>
            <Button onClick={openCreate}>
              <Plus size={14} /> {t('添加账号')}
            </Button>
          </div>
        }
      >
        <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', marginBottom: 12 }}>
          {/* 提供商筛选：对齐 Agent2API 账号页的左栏 provider 下拉。
              渠道是内部容器，不该出现在筛选器里（用户心智里只有供应商）。 */}
          <Select
            value={providerFilter}
            onChange={(e) => setProviderFilter(e.target.value)}
            aria-label={t('选择供应商')}
          >
            <option value="">{t('全部供应商')}</option>
            {Array.from(new Set(sessions.map((s) => s.provider).filter(Boolean)))
              .sort()
              .map((kind) => {
                // §2.14 provider 下拉带每家账号数（value 仍是 kind，不影响筛选）
                const count = sessions.filter((s) => s.provider === kind).length;
                return (
                  <option key={kind} value={kind}>
                    {providers.find((p) => p.kind === kind)?.label || kind} ({count})
                  </option>
                );
              })}
          </Select>
          <Input
            placeholder={t('搜索名称 / provider / 地址')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label={t('搜索')}
          />
          {/* 分组模式（对齐 Agent2API accountGroups：按家分组带 + 组内序号替代优先级数字） */}
          <label className="as-toggle" title={t('按提供商分组显示；组内序号即调度顺序')}>
            <input
              type="checkbox"
              checked={grouped}
              onChange={(e) => {
                setGrouped(e.target.checked);
                writeLS(LS_GROUPED, e.target.checked);
              }}
            />
            <span>{t('分组')}</span>
          </label>
          {/* §2.2 折叠全部 / 展开全部（仅分组模式下出现；两者都落 LS_COLLAPSED） */}
          {grouped && (
            <>
              <button
                type="button"
                className="btn btn-outline btn-sm"
                onClick={collapseAllGroups}
                title={t('收起全部供应商分组，只留组带')}
              >
                <ChevronsDownUp size={14} /> {t('折叠全部')}
              </button>
              <button
                type="button"
                className="btn btn-outline btn-sm"
                onClick={expandAllGroups}
                title={t('展开全部供应商分组')}
              >
                <ChevronsUpDown size={14} /> {t('展开全部')}
              </button>
            </>
          )}
          {/* 列表排序（对齐上游 accounts-sort）：到期最早/最新添加置顶，纯展示序不改调度 */}
          <Select
            aria-label={t('列表排序')}
            className="as-autobal-select"
            value={sortKey}
            onChange={(e) => {
              const v = e.target.value === 'credit' || e.target.value === 'added' ? e.target.value : 'list';
              setSortKey(v);
              writeLS(LS_SORTKEY, v);
            }}
          >
            <option value="list">{t('调度优先序')}</option>
            <option value="credit">{t('按到期排序')}</option>
            <option value="added">{t('按添加时间（新→旧）')}</option>
          </Select>
          {/* 自动余额周期轮询（间隔可配置；关闭时仅保留首次加载那一次查询） */}
          <Select
            aria-label={t('自动余额查询间隔')}
            value={String(autoBalanceMins)}
            className="as-autobal-select"
            onChange={(e) => {
              const v = Number(e.target.value) || 0;
              setAutoBalanceMins(v);
              writeLS(LS_AUTOBAL, v);
            }}
          >
            <option value="0">{t('余额自动：关')}</option>
            <option value="30">{t('每 30 分钟')}</option>
            <option value="60">{t('每小时')}</option>
            <option value="360">{t('每 6 小时')}</option>
            <option value="720">{t('每 12 小时')}</option>
            <option value="1440">{t('每天')}</option>
          </Select>
          {/* 限额分段（SPEC §2.4）：全部/正常/已限流；判据 = 后端 limiter_state。
              放在状态分段之前：e2e 的「全部」定位取第一个，限额「全部」才是这一组。 */}
          <div className="seg-filter" role="group" aria-label={t('按限额状态过滤')}>
            {(
              [
                ['all', t('全部')],
                ['normal', t('正常')],
                ['limited', t('已限流')],
              ] as const
            ).map(([key, label]) => (
              <button
                key={key}
                type="button"
                className={`seg-filter-btn${limitSeg === key ? ' active' : ''}`}
                onClick={() => setLimitSeg(key)}
              >
                {label} {limitCounts[key]}
              </button>
            ))}
          </div>
          {/* 状态分段（对齐 Agent2API 账号页）：启用口径=enabled 且不在冷却中 */}
          <div className="seg-filter" role="group" aria-label={t('按状态过滤')}>
            {(
              [
                ['all', t('全部')],
                ['enabled', t('启用')],
                ['disabled', t('禁用')],
                ['cooling', t('冷却中')],
              ] as const
            ).map(([key, label]) => (
              <button
                key={key}
                type="button"
                className={`seg-filter-btn${statusSeg === key ? ' active' : ''}`}
                onClick={() => setStatusSeg(key)}
              >
                {label} {segCounts[key]}
              </button>
            ))}
          </div>
          {/* §2.3 清除筛选：只重置筛选维度（分组/排序/余额间隔/列设置是偏好，不动） */}
          <button
            type="button"
            className="btn btn-outline btn-sm"
            disabled={!filterActive}
            onClick={clearFilters}
            title={t('清空搜索与全部筛选档位，回到全部账号')}
          >
            {t('清除筛选')}
          </button>
        </div>

        {picked.size > 0 && (
          <div className="batch-bar">
            <span>{t('已选 {{count}} 个账号', { count: picked.size })}</span>
            <Button
              variant="outline"
              size="sm"
              disabled={batchRunning}
              onClick={() => void runBatch('enable')}
            >
              <Power size={14} /> {t('批量启用')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={batchRunning}
              onClick={() => void runBatch('disable')}
            >
              <Power size={14} /> {t('批量停用')}
            </Button>
            <Button
              variant="danger"
              size="sm"
              disabled={batchRunning}
              onClick={askBatchDelete}
            >
              <Trash2 size={14} /> {t('批量删除')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={batchRunning}
              onClick={() => setBatchDlgOpen(true)}
            >
              <ListChecks size={14} /> {t('批量操作…')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => setPicked(new Set())}>
              {t('清除选择')}
            </Button>
          </div>
        )}

        {visible.length === 0 ? (
          <EmptyState
            message={
              accountChannels.length === 0
                ? t('还没有账号：点击「添加账号」，保存时会按所选供应商自动创建账号池')
                : t('暂无账号，点击「添加账号」或「批量导入」开始')
            }
            action={<Button onClick={openCreate}>{t('添加账号')}</Button>}
          />
        ) : (
          <>
            <div className="table-wrapper as-scroll">
            <table className="as-table" style={{ tableLayout: 'fixed', minWidth: tableMinWidth }}>
              {/* 列宽由 colgroup 驱动（拖动表头右缘把手改这里）；弹性列不写宽度吃剩余 */}
              <colgroup>
                {visibleCols.map((col) => (
                  <col key={col} style={widthOf(col) != null ? { width: widthOf(col) } : undefined} />
                ))}
              </colgroup>
              <thead>
                <tr>
                  {visibleCols.map((col, idx) => {
                    const colDef = AS_COL_DEFS.find((c) => c.key === col);
                    const grip = colResize.gripProps(col, {
                      isLast: idx === visibleCols.length - 1,
                      locked: colDef?.locked,
                      defaultWidth: colDef?.width,
                    });
                    return (
                    <th
                      key={col}
                      className={`as-h-${col} ta-${appliedCols.find((c) => c.key === col)?.align ?? 'left'}`}
                    >
                      {col === 'pick' ? (
                        <input
                          type="checkbox"
                          aria-label={t('全选')}
                          checked={visible.length > 0 && picked.size === visible.length}
                          onChange={togglePickAll}
                        />
                      ) : col === 'name' ? (
                        <>
                          <span>{t('账号')}</span>
                          {/* 姓名脱敏（对齐 Agent2API .name-eye：开启后整列打码） */}
                          <button
                            type="button"
                            className={`as-name-eye${nameMasked ? ' on' : ''}`}
                            title={t('隐藏账号名（脱敏显示）')}
                            aria-label={t('姓名脱敏')}
                            onClick={() => {
                              const next = !nameMasked;
                              setNameMasked(next);
                              writeLS(LS_NAMEMASK, next);
                            }}
                          >
                            {nameMasked ? <EyeOff size={12} /> : <Eye size={12} />}
                          </button>
                        </>
                      ) : (
                        t(AS_COLS.find((c) => c.key === col)?.label || col)
                      )}
                      {col !== 'act' && (
                        <span
                          className="as-col-grip"
                          title={t('拖动调整列宽 · 双击还原')}
                          {...grip}
                        />
                      )}
                    </th>
                    );
                  })}
                </tr>
              </thead>
              <tbody>
                {grouped && pageGroups.length
                  ? pageGroups.map((g, pageIdx) => {
                      const collapsed = collapsedGroups.has(g.provider);
                      // 组统计读整组（分页切块时也用完整组的数据，避免数字随页跳）
                      const groupRows = groupedList.find((x) => x.provider === g.provider)?.rows ?? g.rows;
                      // 组统计：启用/禁用/冷却三档（对齐上游 grp-stat；冷却=启用态且有未到期冷却）
                      const gEnabled = groupRows.filter((r) => r.status === 'enabled' && !isCooling(r)).length;
                      const gDisabled = groupRows.filter((r) => r.status !== 'enabled').length;
                      const gCooling = groupRows.filter((r) => r.status === 'enabled' && isCooling(r)).length;
                      return (
                      <Fragment key={`${g.provider}-${pageIdx}`}>
                        {/* 分组带（对齐 Agent2API grp-band：顺位 + 家图标 + 家名 + 统计 + 折叠箭头 + ⋯ 菜单） */}
                        <tr
                          className={`as-tr-group${groupDragOver === g.provider && groupDragActive !== g.provider ? ' drag-over' : ''}`}
                        >
                          <td colSpan={visibleCols.length}>
                            <div
                              className="as-group-band"
                              /* §2.6 组带拖拽整组重排；组内行拖拽在分组模式下本就关闭
                                 （renderRow 的 draggable={!grouped}），两者不会互相触发 */
                              draggable={!orderSaving}
                              onDragStart={(e) => {
                                if ((e.target as Element).closest('button, a, input, select, label')) {
                                  e.preventDefault();
                                  return;
                                }
                                setGroupDragActive(g.provider);
                                e.dataTransfer.effectAllowed = 'move';
                                e.dataTransfer.setData('text/plain', g.provider);
                              }}
                              onDragOver={(e) => {
                                if (!groupDragActive) return; // 行拖拽不落到组带上
                                e.preventDefault();
                                e.dataTransfer.dropEffect = 'move';
                                if (groupDragOver !== g.provider) setGroupDragOver(g.provider);
                              }}
                              onDrop={(e) => {
                                e.preventDefault();
                                void handleDropGroup(g.provider);
                              }}
                              onDragEnd={() => {
                                setGroupDragActive(null);
                                setGroupDragOver(null);
                              }}
                              onClick={(e) => {
                                // 落在按钮/表单控件/拖拽把手上不顺势折叠（对齐上游 onBandClick 判据）
                                if ((e.target as Element).closest('button, a, input, select, label, .as-group-grip')) return;
                                if (String(window.getSelection?.() || '').length > 0) return;
                                toggleGroupCollapsed(g.provider);
                              }}
                              title={collapsed ? t('展开这一组') : t('点击分组带折叠这一组')}
                            >
                              <span className="as-group-grip" title={t('按住拖动，调整这一组的顺位（整组一起动）')}>
                                <GripVertical size={13} />
                              </span>
                              <span className="as-group-rank" title={t('组顺位：数字越小越先用（拖动分组带调整）')}>
                                {t('第 {{n}} 顺位', { n: groupRankOf.get(g.provider) ?? 0 })}
                              </span>
                              {providerIcon(g.provider) ? (
                                <img className="provider-card-logo" src={providerIcon(g.provider) as string} alt="" />
                              ) : null}
                              <span className="as-group-label">{providerLabel(g.provider)}</span>
                              <span className="as-group-count">{groupRows.length}</span>
                              <span className="as-group-stat">
                                {t('启用 {{n}}', { n: gEnabled })}
                                {gDisabled > 0 ? ` · ${t('禁用 {{n}}', { n: gDisabled })}` : ''}
                                {gCooling > 0 ? ` · ${t('冷却 {{n}}', { n: gCooling })}` : ''}
                              </span>
                              <span className="as-group-fill" />
                              <button
                                type="button"
                                className="as-group-fold"
                                aria-expanded={!collapsed}
                                title={collapsed ? t('展开这一组') : t('折叠这一组')}
                                onClick={() => toggleGroupCollapsed(g.provider)}
                              >
                                {collapsed ? '▸' : '▾'}
                              </button>
                              <div
                                className="as-group-menu-wrap"
                                ref={groupMenuFor === g.provider ? groupMenuRef : undefined}
                              >
                                <button
                                  type="button"
                                  className="as-group-menu-btn"
                                  title={t('本组操作')}
                                  aria-label={t('本组操作')}
                                  onClick={() =>
                                    setGroupMenuFor(groupMenuFor === g.provider ? null : g.provider)
                                  }
                                >
                                  ⋯
                                </button>
                                {groupMenuFor === g.provider && (
                                  <div className="as-group-menu">
                                    <button
                                      type="button"
                                      className="as-group-menu-item"
                                      disabled={batchRunning}
                                      onClick={() => {
                                        setGroupMenuFor(null);
                                        void runGroupEnabled(groupRows, true);
                                      }}
                                    >
                                      {t('全部启用（{{n}}）', { n: groupRows.length })}
                                    </button>
                                    <button
                                      type="button"
                                      className="as-group-menu-item"
                                      disabled={batchRunning}
                                      onClick={() => {
                                        setGroupMenuFor(null);
                                        void runGroupEnabled(groupRows, false);
                                      }}
                                    >
                                      {t('全部停用（{{n}}）', { n: groupRows.length })}
                                    </button>
                                    <div className="as-group-menu-sep" />
                                    <button
                                      type="button"
                                      className="as-group-menu-item"
                                      onClick={() => {
                                        setGroupMenuFor(null);
                                        toggleGroupCollapsed(g.provider);
                                      }}
                                    >
                                      {collapsed ? t('展开这一组') : t('折叠这一组')}
                                    </button>
                                  </div>
                                )}
                              </div>
                            </div>
                          </td>
                        </tr>
                        {!collapsed && g.rows.map((row, idx) => renderRow(row, idx))}
                      </Fragment>
                      );
                    })
                  : pageRows.map((row) => renderRow(row, -1))}
              </tbody>
              {/* 冷却明细展开行（对齐 Agent2API row-panel：通栏面板 + 标签流） */}
              {cooldownOpenId && (() => {
                const open = sessions.find((s) => s.id === cooldownOpenId);
                if (!open) return null;
                const entries = activeCooldowns(open);
                return (
                  <tbody className="as-panel-tbody">
                    <tr className="as-panel-row">
                      <td colSpan={visibleCols.length}>
                        <div className="as-panel-head">
                          <strong>{nameMasked ? maskName(open.name) : open.name}</strong>
                          <span className="form-hint">
                            {t('冷却来自上游 429/限额失败，到期自动解除；手动清除后若仍限流会再次标记')}
                          </span>
                          <button
                            type="button"
                            className="as-panel-clear-all"
                            onClick={() => void handleResetCooldowns(open)}
                          >
                            {t('全部清除')}
                          </button>
                          <button type="button" className="as-panel-close" onClick={() => setCooldownOpenId(null)} title={t('收起')}>
                            ✕
                          </button>
                        </div>
                        {entries.length === 0 ? (
                          <div className="as-panel-body">{t('当前没有冷却中的模型。')}</div>
                        ) : (
                          <div className="as-panel-list">
                            {entries.map(([model, rem]) => (
                              <div key={model} className="as-limit-row">
                                <code className="as-limit-model">{model === '*' ? t('全部模型') : model}</code>
                                <span className="as-badge-cooling">{t('冷却中')}</span>
                                <span className="form-hint">{t('剩 {{time}}', { time: fmtDuration(rem) })}</span>
                                <button
                                  type="button"
                                  className="as-clear-one"
                                  onClick={() => void handleClearOneCooldown(open, model)}
                                >
                                  {t('清除标记')}
                                </button>
                              </div>
                            ))}
                          </div>
                        )}
                      </td>
                    </tr>
                  </tbody>
                );
              })()}
            </table>
            </div>
            {/* 分页页脚（SPEC §2.1：公共件，作用于筛选+排序后的结果） */}
            <TablePaginationFooter
              total={visibleSorted.length}
              page={page}
              totalPages={totalPages}
              size={pageSize}
              onSizeChange={setPageSize}
              onPageChange={setRawPage}
            />
          </>
        )}
      </Card>

      {showModal && !editing && (
        <AddAccountDialog
          providers={providers}
          sessions={sessions}
          channels={accountChannels}
          onClose={closeModal}
          onCreated={load}
        />
      )}

      {showModal && editing && (
        <div className="modal-overlay">
          <form className="modal as-dlg" onSubmit={handleSave}>
            <div className="modal-header">
              <h3>
                {t('账号设置')}
                {editing.name ? <span className="as-dlg-sub">{editing.name}</span> : null}
              </h3>
              <button type="button" className="modal-close" onClick={closeModal}>
                &times;
              </button>
            </div>
            <div className="as-dlg-body">
              <nav className="as-dlg-rail" aria-label={t('设置分组')}>
                {DLG_SEGS.map((seg) => (
                  <button
                    key={seg.key}
                    type="button"
                    className={`as-dlg-nav${dlgSeg === seg.key ? ' active' : ''}`}
                    onClick={() => setDlgSeg(seg.key)}
                  >
                    {DLG_ICONS[seg.icon]}
                    <span>{t(seg.label)}</span>
                  </button>
                ))}
              </nav>
              <div className="as-dlg-main">
                {/* ── 基本信息：供应商 / 版本 / 渠道 / 名称 ── */}
                {dlgSeg === 'basic' && (
                  <>
                    <Select
                      label={t('Provider')}
                      value={form.provider}
                      onChange={(e) => setForm({ ...form, provider: e.target.value })}
                    >
                      {providers.filter((p) => !isCardHidden(p.kind)).map((p) => (
                        <option key={p.kind} value={p.kind}>
                          {p.label}
                        </option>
                      ))}
                    </Select>
                    {hostEditions.length ? (
                      <div className="form-group">
                        <label>{t('账号版本')}</label>
                        <Tabs<string>
                          items={hostEditions.map((item) => ({ key: item.id, label: item.label }))}
                          active={selectedEditionId}
                          onChange={(id) => setForm({ ...form, provider: id })}
                          ariaLabel={t('账号版本')}
                        />
                        <div className="form-hint">
                          {t('国内版与国际版是两套独立账号，凭证不通用。')}
                        </div>
                      </div>
                    ) : null}
                    {editing || accountChannels.length > 0 ? (
                      <Select
                        label={t('供应商')}
                        value={form.channel_id}
                        onChange={(e) => setForm({ ...form, channel_id: e.target.value })}
                        disabled={Boolean(editing)}
                      >
                        {accountChannels.map((c) => (
                          <option key={c.id} value={c.id}>
                            {c.name}
                          </option>
                        ))}
                      </Select>
                    ) : (
                      <div className="form-hint" style={{ marginBottom: 8 }}>
                        {t('还没有账号池：保存时会按所选供应商自动创建一个账号池')}
                      </div>
                    )}
                    <Input
                      label={t('账号名（可选）')}
                      placeholder="account-1"
                      value={form.name}
                      onChange={(e) => setForm({ ...form, name: e.target.value })}
                    />
                  </>
                )}
                {/* ── 凭证：对话路径 / 抓包导入 / 凭证 / 额外请求头 ── */}
                {dlgSeg === 'credential' && (
                  <>
                    {selectedProvider?.kind === 'trae' ? (
                      <div className="form-hint">
                        {t('Trae 还要 X-Uid / X-Machine-Id / X-Device-Id：从抓包复制后填在下方「额外请求头」。')}
                      </div>
                    ) : null}
                    {selectedProvider?.native_supported ? null : (
                      <Input
                        label={t('对话路径（可选，默认 /chat/completions）')}
                        placeholder={selectedProvider?.chat_path || '/api/chat'}
                        hint={t('登录态型供应商的私有接口路径；留空按 provider 默认或标准 OpenAI 路径拼接。')}
                        value={form.chat_path}
                        onChange={(e) => setForm({ ...form, chat_path: e.target.value })}
                      />
                    )}
                    <div className="form-group">
                      <label>{t('从抓包请求头导入（可选）')}</label>
                      <textarea
                        className="form-input"
                        rows={3}
                        placeholder={
                          'Cookie: sessionid=...\nAuthorization: Bearer eyJ...\nx-token: ...'
                        }
                        value={rawHeaders}
                        onChange={(e) => setRawHeaders(e.target.value)}
                      />
                      <div style={{ marginTop: 6 }}>
                        <button
                          type="button"
                          className="btn btn-outline btn-sm"
                          onClick={applyRawHeaders}
                          disabled={!rawHeaders.trim()}
                        >
                          {t('解析并填充凭证与请求头')}
                        </button>
                      </div>
                      <div className="form-hint">
                        {t('从浏览器开发者工具「请求头」整段复制：Cookie / Bearer 自动填入凭证，其余进入额外请求头。')}
                      </div>
                    </div>
                    <Input
                      label={credentialLabel}
                      type="password"
                      placeholder={
                        editing
                          ? t('留空表示不修改')
                          : sessionAuth === 'cookie'
                            ? 'sessionid=...; token=...'
                            : 'sk-...'
                      }
                      hint={credentialHint}
                      value={form.api_key}
                      onChange={(e) => setForm({ ...form, api_key: e.target.value })}
                    />
                    {editing && (
                      <label className="as-check">
                        <input
                          type="checkbox"
                          checked={form.clear_key}
                          onChange={(e) => setForm({ ...form, clear_key: e.target.checked })}
                        />
                        <span>{t('清除现有凭证')}</span>
                      </label>
                    )}
                    <div className="form-group">
                      <label>{t('额外请求头（每行 Key: Value）')}</label>
                      <textarea
                        className="form-input"
                        rows={3}
                        placeholder={'origin: https://example.com\nx-token: {credential}'}
                        value={form.extra_headers}
                        onChange={(e) => setForm({ ...form, extra_headers: e.target.value })}
                      />
                      <div className="form-hint">
                        {selectedProvider?.native_supported
                          ? t('内置适配已带该供应商的默认请求头；只有 Trae 这类需要设备身份的上游才要在这里补。')
                          : t('登录态型供应商按抓包补齐专用请求头；值里的 {{credential}} 会替换为凭证。')}
                      </div>
                    </div>
                  </>
                )}
                {/* ── 代理与连接：私有协议提示 / 上游地址 / 代理 ── */}
                {dlgSeg === 'proxy' && (
                  <>
                    {selectedProvider?.openai_compatible === false &&
                    !selectedProvider?.native_supported ? (
                      <div className="form-hint form-hint-warn">
                        {t('该供应商上游需要私有协议/签名（请求体与响应都要转换），标准 OpenAI 转发会失败；请填写自建转换网关地址，或等待专用适配。')}
                      </div>
                    ) : null}
                    <Input
                      label={t('上游地址（可选，留空用 provider 默认）')}
                      placeholder={selectedProvider?.default_base_url || 'https://api.deepseek.com'}
                      value={form.base_url}
                      onChange={(e) => setForm({ ...form, base_url: e.target.value })}
                    />
                    <ProxySelector
                      value={form.proxy || editing?.proxy || ''}
                      onChange={(next) => setForm({ ...form, proxy: next, clear_proxy: false })}
                      placeholder={editing?.proxy || 'socks5h://127.0.0.1:1080'}
                    />
                  </>
                )}
                {/* ── 限制器：条件组（#29 主编辑面）+ 旧版扁平规则（兼容保留） ── */}
                {dlgSeg === 'limiter' && (
                  <div className="as-limiter-rules">
                    <div className="as-lg-title">
                      {t('限制条件组')}
                      {/* 生效来源徽标（#29 复核缺口 1）：账号当前持久化的事实——
                          limiter_groups 非空 → 后端按条件组判定；空但有扁平规则 →
                          后端降维（groups_from_flat）按扁平判定；两者都无 → 不显示。
                          判据用 editing（打开弹窗时的持久化快照），不随草稿中间态闪变。 */}
                      {(() => {
                        const hasGroups = (editing?.limiter_groups?.length ?? 0) > 0;
                        const hasFlat = (editing?.limiters?.length ?? 0) > 0;
                        if (!hasGroups && !hasFlat) return null;
                        return (
                          <span className={`as-lg-source-badge${hasGroups ? '' : ' flat'}`}>
                            {hasGroups
                              ? t('当前生效：条件组（{{n}} 组）', { n: editing?.limiter_groups?.length ?? 0 })
                              : t('当前生效：扁平规则（降维）')}
                          </span>
                        );
                      })()}
                    </div>
                    {limiterGroupsDraft.length === 0 ? (
                      <div className="as-limiter-empty">
                        {t('余额 / 百分比 / 本地计数读数 × 且/或分支 × 模型范围，比旧规则更灵活；未添加时按下方旧规则判定。')}
                      </div>
                    ) : (
                      <>
                        {limiterGroupsDraft.map((g, i) => (
                          <div key={i} className="as-limiter-rule">
                            <Switch
                              checked={g.enabled}
                              onChange={(v) =>
                                setLimiterGroupsDraft(limiterGroupsDraft.map((x, j) => (j === i ? { ...x, enabled: v } : x)))
                              }
                              ariaLabel={t('启用条件组')}
                              title={t('启用条件组')}
                            />
                            <span className="as-limiter-text">{describeGroup(g)}</span>
                            <span className="as-limiter-ops">
                              <button
                                type="button"
                                className="as-act"
                                onClick={() => setLgEditor({ index: i })}
                                title={t('编辑')}
                              >
                                <Pencil size={13} />
                              </button>
                              <button
                                type="button"
                                className="as-act as-act-danger"
                                onClick={() => setLimiterGroupsDraft(limiterGroupsDraft.filter((_, j) => j !== i))}
                                title={t('删除')}
                              >
                                <Trash2 size={13} />
                              </button>
                            </span>
                          </div>
                        ))}
                        <div className="as-lg-hint">{t('条件组已配置：调度判定以条件组为准，下方旧规则不再参与。')}</div>
                      </>
                    )}
                    <div className="as-lg-actions">
                      <button
                        type="button"
                        className="btn btn-outline btn-sm"
                        onClick={() => setLgEditor({ index: -1 })}
                      >
                        <Plus size={14} /> {t('新增条件组')}
                      </button>
                      {limiterGroupsDraft.length === 0 && limiters.length > 0 ? (
                        <button
                          type="button"
                          className="btn btn-outline btn-sm"
                          onClick={() => setLimiterGroupsDraft(flatToGroups(limiters))}
                        >
                          {t('从旧规则导入')}
                        </button>
                      ) : null}
                    </div>
                    <div className="as-lg-divider">{t('旧版限制规则（简单余额 / Token 上限）')}</div>
                    {limiters.length === 0 ? (
                      <div className="as-limiter-empty">
                        {t('暂无限流规则。命中规则时自动跳过该账号或直接停用，避免无效重试。')}
                      </div>
                    ) : (
                      limiters.map((rule, i) => (
                        <div key={`${i}-${rule.type}`} className="as-limiter-rule">
                          <Switch
                            checked={rule.enabled !== false}
                            onChange={(v) => toggleLimiter(i, v)}
                            ariaLabel={t('启用规则')}
                            title={t('启用规则')}
                          />
                          <span className="as-limiter-text">{describeLimiterRule(rule)}</span>
                          <span className="as-limiter-ops">
                            <button
                              type="button"
                              className="as-act"
                              onClick={() => openLimiterEditor(i)}
                              title={t('编辑')}
                            >
                              <Pencil size={13} />
                            </button>
                            <button
                              type="button"
                              className="as-act as-act-danger"
                              onClick={() => removeLimiter(i)}
                              title={t('删除')}
                            >
                              <Trash2 size={13} />
                            </button>
                          </span>
                        </div>
                      ))
                    )}
                    <div>
                      <button
                        type="button"
                        className="btn btn-outline btn-sm"
                        onClick={() => openLimiterEditor(-1)}
                      >
                        <Plus size={14} /> {t('新增限制器')}
                      </button>
                      <div className="form-hint">
                        {t('规则保存后写入账号的 limiters 字段；调度器按规则在选号阶段跳过或停用账号。')}
                      </div>
                    </div>
                  </div>
                )}
                {/* ── 高级：调度参数 / 白名单 / 余额查询 / 到期 / 状态 ── */}
                {dlgSeg === 'advanced' && (
                  <>
                    <div className="as-dlg-grid2">
                      <Input
                        label={t('优先级')}
                        type="number"
                        value={form.priority}
                        onChange={(e) => setForm({ ...form, priority: e.target.value })}
                      />
                      <Input
                        label={t('权重')}
                        type="number"
                        min="1"
                        value={form.weight}
                        onChange={(e) => setForm({ ...form, weight: e.target.value })}
                      />
                    </div>
                    <Input
                      label={t('并发上限')}
                      type="number"
                      min="0"
                      hint={t('(0 = 不限)')}
                      value={form.max_concurrency}
                      onChange={(e) => setForm({ ...form, max_concurrency: e.target.value })}
                    />
                    <Input
                      label={t('模型白名单（可选，逗号分隔）')}
                      placeholder="deepseek-chat, deepseek-reasoner"
                      value={form.models}
                      onChange={(e) => setForm({ ...form, models: e.target.value })}
                    />
                    <Select
                      label={t('余额自动查询')}
                      hint={t('按间隔自动拉取上游余额并展示上次读数；仅支持余额查询的供应商生效')}
                      value={form.usage_query_interval_secs}
                      onChange={(e) => setForm({ ...form, usage_query_interval_secs: e.target.value })}
                    >
                      <option value="0">{t('关闭')}</option>
                      <option value="3600">{t('每小时')}</option>
                      <option value="21600">{t('每 6 小时')}</option>
                      <option value="43200">{t('每 12 小时')}</option>
                      <option value="86400">{t('每天')}</option>
                    </Select>
                    <Input
                      label={t('凭证到期时间（可选）')}
                      type="number"
                      hint={t('(留空则永不过期)')}
                      value={form.expires_at}
                      onChange={(e) => setForm({ ...form, expires_at: e.target.value })}
                    />
                    <Select
                      label={t('状态')}
                      value={form.status}
                      onChange={(e) => setForm({ ...form, status: e.target.value })}
                    >
                      <option value="enabled">{t('启用')}</option>
                      <option value="disabled">{t('停用')}</option>
                    </Select>
                  </>
                )}
              </div>
            </div>
            <div className="modal-footer">
              {limiterError ? <span className="as-dlg-err">{limiterError}</span> : null}
              <Button variant="secondary" onClick={closeModal}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('保存中…') : t('保存修改')}
              </Button>
            </div>
          </form>
        </div>
      )}

      {/* 限制器二级弹层：叠在账号设置弹窗之上（对齐 Agent2API 第二层编辑体验） */}
      {limiterEditor && (
        <div className="modal-overlay as-lim-overlay">
          <div className="modal as-lim-modal" role="dialog" aria-modal="true">
            <div className="modal-header">
              <h3>{limiterEditor.index >= 0 ? t('编辑限制器') : t('新增限制器')}</h3>
              <button
                type="button"
                className="modal-close"
                onClick={() => setLimiterEditor(null)}
              >
                &times;
              </button>
            </div>
            <div className="modal-body">
              <div className="as-lim-row">
                <span className="as-lim-label">{t('类型')}</span>
                <div className="as-lim-seg">
                  <button
                    type="button"
                    className={limiterEditor.draft.type === 'balance' ? 'active' : ''}
                    onClick={() => updateLimiterDraft({ type: 'balance' })}
                  >
                    {t('余额')}
                  </button>
                  <button
                    type="button"
                    className={limiterEditor.draft.type === 'token' ? 'active' : ''}
                    onClick={() => updateLimiterDraft({ type: 'token' })}
                  >
                    {t('Token 消耗')}
                  </button>
                </div>
              </div>
              <div className="as-lim-row">
                <span className="as-lim-label">{t('命中后')}</span>
                <div className="as-lim-cond">
                  <Select
                    value={limiterEditor.draft.action}
                    onChange={(e) =>
                      updateLimiterDraft({ action: e.target.value as LimiterDraft['action'] })
                    }
                  >
                    <option value="skip">{t('跳过本次（自动恢复）')}</option>
                    <option value="disable">{t('停用账号（需手动启用）')}</option>
                  </Select>
                </div>
              </div>
              <div className="as-lim-row">
                <span className="as-lim-label">{t('阈值')}</span>
                <div className="as-lim-cond">
                  <Input
                    type="number"
                    min="0"
                    value={limiterEditor.draft.thresholdInput}
                    onChange={(e) => updateLimiterDraft({ thresholdInput: e.target.value })}
                  />
                  {limiterEditor.draft.type === 'token' ? (
                    <Select
                      className="as-lim-unit"
                      value={limiterEditor.draft.tokenUnit}
                      onChange={(e) =>
                        updateLimiterDraft({
                          tokenUnit: e.target.value as LimiterDraft['tokenUnit'],
                        })
                      }
                    >
                      <option value="unit">{t('个')}</option>
                      <option value="wan">{t('万')}</option>
                    </Select>
                  ) : (
                    <span className="as-lim-suffix">{t('余额低于该值时触发')}</span>
                  )}
                </div>
              </div>
              {limiterEditor.draft.type === 'token' ? (
                <div className="as-lim-row">
                  <span className="as-lim-label">{t('周期')}</span>
                  <div className="as-lim-cond">
                    <Select
                      value={limiterEditor.draft.reset}
                      onChange={(e) =>
                        updateLimiterDraft({ reset: e.target.value as LimiterDraft['reset'] })
                      }
                    >
                      <option value="fixed">{t('固定周期')}</option>
                      <option value="daily">{t('每日重置（零点）')}</option>
                    </Select>
                    {limiterEditor.draft.reset === 'fixed' ? (
                      <div className="as-lim-period">
                        <Input
                          type="number"
                          min="0"
                          value={limiterEditor.draft.periodInput}
                          onChange={(e) => updateLimiterDraft({ periodInput: e.target.value })}
                        />
                        <Select
                          value={limiterEditor.draft.periodUnit}
                          onChange={(e) =>
                            updateLimiterDraft({
                              periodUnit: e.target.value as LimiterDraft['periodUnit'],
                            })
                          }
                        >
                          <option value="minutes">{t('分钟')}</option>
                          <option value="hours">{t('小时')}</option>
                        </Select>
                        <span className="as-lim-suffix">{t('30 分钟 ~ 24 小时')}</span>
                      </div>
                    ) : null}
                  </div>
                </div>
              ) : (
                <div className="as-lim-row">
                  <span className="as-lim-label">{t('周期')}</span>
                  <span className="as-lim-suffix">{t('余额型无需周期，始终生效')}</span>
                </div>
              )}
              {limiterError ? <div className="as-lim-error">{limiterError}</div> : null}
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={() => setLimiterEditor(null)}>
                {t('取消')}
              </Button>
              <Button onClick={saveLimiterEditor}>{t('确定')}</Button>
            </div>
          </div>
        </div>
      )}

      {/* 条件组二级弹层（#29）：叠在账号设置弹窗之上，保存回写草稿待主保存 PATCH */}
      {lgEditor ? (
        <LimiterGroupEditor
          initial={lgEditor.index >= 0 ? limiterGroupsDraft[lgEditor.index] ?? null : null}
          onSave={(g) => {
            setLimiterGroupsDraft((prev) =>
              lgEditor.index >= 0
                ? prev.map((x, i) => (i === lgEditor.index ? g : x))
                : [...prev, g],
            );
            setLgEditor(null);
          }}
          onClose={() => setLgEditor(null)}
        />
      ) : null}

      {batchDlgOpen && picked.size > 0 && (
        <AccountBatchDialog
          ids={[...picked]}
          accounts={sessions}
          busy={batchRunning}
          onClose={() => setBatchDlgOpen(false)}
          onApplied={async () => {
            setPicked(new Set());
            addToast(t('批量操作完成'));
            await load();
          }}
        />
      )}

      {showBulk && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={handleBulk}>
            <div className="modal-header">
              <h3>{t('批量导入')}</h3>
              <button type="button" className="modal-close" onClick={() => setShowBulk(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <Select
                label={t('供应商')}
                value={bulk.channel_id}
                onChange={(e) => setBulk({ ...bulk, channel_id: e.target.value })}
              >
                {accountChannels.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </Select>
              <Select
                label={t('Provider')}
                value={bulk.provider}
                onChange={(e) => setBulk({ ...bulk, provider: e.target.value })}
              >
                {providers.map((p) => (
                  <option key={p.kind} value={p.kind}>
                    {p.label}
                  </option>
                ))}
              </Select>
              <Input
                label={t('上游地址（可选）')}
                placeholder="https://api.deepseek.com"
                value={bulk.base_url}
                onChange={(e) => setBulk({ ...bulk, base_url: e.target.value })}
              />
              <Input
                label={t('名称前缀')}
                value={bulk.name_prefix}
                onChange={(e) => setBulk({ ...bulk, name_prefix: e.target.value })}
              />
              <Input
                label={t('出口代理（可选）')}
                placeholder="socks5h://127.0.0.1:1080"
                value={bulk.proxy}
                onChange={(e) => setBulk({ ...bulk, proxy: e.target.value })}
              />
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
                <Input
                  label={t('优先级')}
                  type="number"
                  value={bulk.priority}
                  onChange={(e) => setBulk({ ...bulk, priority: e.target.value })}
                />
                <Input
                  label={t('权重')}
                  type="number"
                  min="1"
                  value={bulk.weight}
                  onChange={(e) => setBulk({ ...bulk, weight: e.target.value })}
                />
              </div>
              <Input
                label={t('并发上限')}
                type="number"
                min="0"
                hint={t('(0 = 不限)')}
                value={bulk.max_concurrency}
                onChange={(e) => setBulk({ ...bulk, max_concurrency: e.target.value })}
              />
              <Input
                label={t('模型白名单（可选）')}
                placeholder="deepseek-chat"
                value={bulk.models}
                onChange={(e) => setBulk({ ...bulk, models: e.target.value })}
              />
              <Textarea
                label={t('凭证列表')}
                rows={8}
                placeholder={'sk-aaa\nname2,sk-bbb\n# 注释行会被忽略'}
                hint={t('每行一条：Name,Key 或仅 Key；# 开头为注释')}
                value={bulk.credentials}
                onChange={(e) => setBulk({ ...bulk, credentials: e.target.value })}
              />
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={() => setShowBulk(false)}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('导入中…') : t('开始导入')}
              </Button>
            </div>
          </form>
        </div>
      )}

      {showRotation && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={(e) => void saveRotation(e)}>
            <div className="modal-header">
              <h3>{t('账号路由')}</h3>
              <button type="button" className="modal-close" onClick={() => setShowRotation(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <p style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                {t('账号失败后的冷却窗口与单请求最多尝试几个账号；保存后立即生效并持久化。')}
              </p>
              {rotationError && <div className="error-message">{rotationError}</div>}
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
                <Input
                  label={t('基础冷却（秒）')}
                  type="number"
                  min="5"
                  max="3600"
                  hint={t('连续失败按指数退避，从该值起算（5–3600）')}
                  value={rotation.base}
                  onChange={(e) => setRotation({ ...rotation, base: e.target.value })}
                />
                <Input
                  label={t('冷却上限（秒）')}
                  type="number"
                  min="5"
                  max="86400"
                  hint={t('单账号单模型的最长冷却（不超过 86400）')}
                  value={rotation.max}
                  onChange={(e) => setRotation({ ...rotation, max: e.target.value })}
                />
              </div>
              <Input
                label={t('认证失败冷却（秒）')}
                type="number"
                min="60"
                max="86400"
                hint={t('密钥失效/鉴权失败的长冷却（60–86400）')}
                value={rotation.auth}
                onChange={(e) => setRotation({ ...rotation, auth: e.target.value })}
              />
              <Input
                label={t('单请求最大换号次数')}
                type="number"
                min="1"
                max="20"
                hint={t('一次请求最多尝试几个账号（1–20）')}
                value={rotation.attempts}
                onChange={(e) => setRotation({ ...rotation, attempts: e.target.value })}
              />
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={() => setShowRotation(false)}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={rotationSaving}>
                {rotationSaving ? t('保存中…') : t('保存修改')}
              </Button>
            </div>
          </form>
        </div>
      )}

      {/* ── 本机客户端导入（cc-switch）── */}
      {showImportScan && (
        <div className="modal-overlay" onClick={() => setShowImportScan(false)}>
          <div className="modal" style={{ maxWidth: 720 }} onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('从本机客户端导入')}</h3>
              <button className="modal-close" onClick={() => setShowImportScan(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {!importScan ? (
                <Loading text={t('扫描中...')} />
              ) : !importScan.available ? (
                <div className="notify-note">{importScan.reason}</div>
              ) : importScan.providers.length === 0 ? (
                <EmptyState icon="📥" message={t('cc-switch 里没有可导入的供应商')} />
              ) : (
                <>
                  <div className="form-hint" style={{ marginBottom: 8 }}>
                    {t('数据来源')}: {importScan.path}
                  </div>
                  <div className="table-wrapper">
                    <table>
                      <thead>
                        <tr>
                          <th style={{ width: 40 }}></th>
                          <th>{t('名称')}</th>
                          <th>{t('类型')}</th>
                          <th>{t('端点')}</th>
                          <th>{t('状态')}</th>
                        </tr>
                      </thead>
                      <tbody>
                        {importScan.providers.map((p) => (
                          <tr key={p.id}>
                            <td>
                              <input
                                type="checkbox"
                                checked={importPicked.has(p.id)}
                                disabled={!p.importable}
                                onChange={(e) => {
                                  setImportPicked((prev) => {
                                    const next = new Set(prev);
                                    if (e.target.checked) next.add(p.id);
                                    else next.delete(p.id);
                                    return next;
                                  });
                                }}
                                aria-label={t('选择')}
                              />
                            </td>
                            <td>{p.name}</td>
                            <td>{p.appType}</td>
                            <td>
                              {p.importable ? (
                                p.baseUrl
                              ) : (
                                <span className="form-hint">{p.reason}</span>
                              )}
                            </td>
                            <td>
                              {p.importable ? (
                                <span className="badge ok">{t('可导入')}</span>
                              ) : (
                                <span className="badge">{t('不支持')}</span>
                              )}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </>
              )}
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={() => setShowImportScan(false)}>
                {t('取消')}
              </Button>
              <Button
                onClick={() => void handleRunImport()}
                disabled={importing || !importScan?.available || importPicked.size === 0}
              >
                {importing ? t('导入中...') : t('导入所选')}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── 模型目录同步结果 ── */}
      {catalogModal && (
        <div className="modal-overlay" onClick={() => setCatalogModal(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('上游模型目录')}</h3>
              <button className="modal-close" onClick={() => setCatalogModal(null)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {catalogModal.map((r) => (
                <div key={r.account_id} style={{ marginBottom: 12 }}>
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 6 }}>
                    <strong>{r.name}</strong>
                    <span className="form-hint">
                      {r.count} {t('个模型')} · {r.source}
                      {r.applied ? ` · ${t('已写入账号')}` : ''}
                    </span>
                  </div>
                  <div className="form-hint">
                    {r.models.map((m) => m.id).join(', ') || t('（空）')}
                  </div>
                </div>
              ))}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setCatalogModal(null)}>
                {t('关闭')}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 导入 JSON 确认：选目标渠道（留空则按文件内 channel_id/channel_name 匹配） */}
      {importJsonEntries && (
        <div className="modal-overlay" onClick={() => !importJsonRunning && setImportJsonEntries(null)}>
          <div className="modal-card" onClick={(e) => e.stopPropagation()}>
            <h3>{t('导入账号')}</h3>
            <p className="form-hint">
              {t('解析到 {{count}} 个账号。选择统一落点供应商，或留空按文件内记录的供应商匹配。同供应商同名账号会跳过（不覆盖在线凭证）。', { count: importJsonEntries.length })}
            </p>
            <Select
              value={importJsonChannel}
              onChange={(e) => setImportJsonChannel(e.target.value)}
              aria-label={t('目标供应商')}
            >
              <option value="">{t('按文件内供应商匹配')}</option>
              {accountChannels.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </Select>
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', marginTop: 12 }}>
              <Button
                variant="outline"
                onClick={() => setImportJsonEntries(null)}
                disabled={importJsonRunning}
              >
                {t('取消')}
              </Button>
              <Button onClick={() => void runImportJson()} disabled={importJsonRunning}>
                {importJsonRunning ? <Loader2 size={14} /> : null}
                {t('开始导入')}
              </Button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog
        state={confirmState}
        onClose={() => setConfirmState(null)}
      />
    </div>
  );
}
