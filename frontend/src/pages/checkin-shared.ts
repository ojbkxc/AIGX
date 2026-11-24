/**
 * 签到中心的共享判据（对齐 Agent2API `ui-islands/src/islands/checkin-types.ts`
 * 与 `checkin-shared.tsx`，按 AIGX 的既有形状落地）。
 *
 * ── 为什么单独一层 ──────────────────────────────────────────
 * 「每日签到 / 新手福利 / 活动福利 / 成长任务」四个分区的**结构**是同一套：
 * 分区标题 + 提供商分类栏 + 状态筛选（全部 / 已完成 / 未完成）+ 账号行。
 * 判据写在分区里就会各写一份，然后必然漂移（A2A 把这层抽出来的原因相同）。
 *
 * ── 只放判据与文案表，不放 JSX ───────────────────────────────
 * `t()` 只在 `.tsx` 里调用（i18n 扫描器只扫 `.tsx`），这里给的是中文原文
 * 常量，调用方负责 `t(...)`。
 */

import type { CheckinAccountRow, CheckinCenterData, CheckinProviderGroup } from '../types';

/** 签到中心的四个「任务」分区（A2A `CheckinSection`） */
export type CheckinSection = 'daily' | 'onboarding' | 'plans' | 'growth';

/** 一级 Tab：四个任务分区 + 定时签到 / 定时补签 / 签到记录（+ AIGX 的福利通道） */
export type CheckinTab = CheckinSection | 'schedule' | 'makeup' | 'records' | 'benefits';

/** A2A `CHECKIN_SECTION_OPTIONS`：顺序即 Tab 顺序（每日签到在最前） */
export const CHECKIN_SECTION_TABS: ReadonlyArray<{ value: CheckinSection; label: string }> = [
  { value: 'daily', label: '每日签到' },
  { value: 'onboarding', label: '新手福利' },
  { value: 'plans', label: '活动福利' },
  { value: 'growth', label: '成长任务' },
];

/** 视图类 Tab（A2A 的三个二级页 + AIGX 保留的福利通道） */
export const CHECKIN_VIEW_TABS: ReadonlyArray<{ value: CheckinTab; label: string }> = [
  { value: 'schedule', label: '定时签到' },
  { value: 'makeup', label: '定时补签' },
  { value: 'records', label: '签到记录' },
  { value: 'benefits', label: '福利通道' },
];

export function isCheckinSection(value: string): value is CheckinSection {
  return CHECKIN_SECTION_TABS.some((item) => item.value === value);
}

/** 旧版本把首 Tab 存成 `center`：读回时映射到「每日签到」，不让记忆把页签弄丢 */
export function normalizeCheckinTab(saved: string | null): CheckinTab {
  if (!saved) return 'daily';
  if (saved === 'center') return 'daily';
  if (isCheckinSection(saved)) return saved;
  return CHECKIN_VIEW_TABS.some((item) => item.value === saved) ? (saved as CheckinTab) : 'daily';
}

/** 状态筛选三档（A2A `StatusFilter`） */
export type StatusFilter = 'all' | 'done' | 'pending';

/**
 * 账号当日状态（A2A `AccountCheckinStatus` 的 AIGX 取值面）。
 *
 * - `checkedInToday` 优先：那是台账里今天真领到的判据（success / already）；
 * - 其次看 `dailyCheckin.status`（`CheckinOutcome` 的字符串：pending / captcha / failed）；
 * - 凭据不可用单独一档：它不是「没签」，点按钮也签不上。
 */
export type AccountStatus = 'done' | 'pending' | 'captcha' | 'failed' | 'unavailable';

export function accountStatus(account: CheckinAccountRow): AccountStatus {
  if (account.checkedInToday) return 'done';
  if (account.available === false) return 'unavailable';
  const daily = account.dailyCheckin as { status?: unknown } | null | undefined;
  const status = typeof daily?.status === 'string' ? daily.status : '';
  if (status === 'captcha') return 'captcha';
  if (status === 'failed') return 'failed';
  return 'pending';
}

/** 账号状态 → 文案（调用方 `t(...)`） */
export const ACCOUNT_STATUS_LABEL: Record<AccountStatus, string> = {
  done: '已签',
  pending: '待签',
  captcha: '需人工处理',
  failed: '失败',
  unavailable: '凭据不可用',
};

/** 账号状态 → Badge tone */
export const ACCOUNT_STATUS_TONE: Record<
  AccountStatus,
  'success' | 'warning' | 'danger' | 'neutral' | 'info'
> = {
  done: 'success',
  pending: 'info',
  captcha: 'warning',
  failed: 'danger',
  unavailable: 'neutral',
};

/** 档位 → 按钮文案（A2A `MODE_LABEL`；`checkin` 是兜底档） */
export const MODE_LABEL: Record<string, string> = {
  checkin: '签到',
  keepalive: '保活',
  claim: '领取',
  full: '保活 + 领取',
};

/** 档位 → 悬停说明（A2A `MODE_TITLE`） */
export const MODE_TITLE: Record<string, string> = {
  checkin: '走这一家的每日签到接口',
  keepalive: '用免费模型与网页会话维持账号活跃 · 不领取奖励',
  claim: '只领取当天的奖励 · 不做保活',
  full: '保活并领取奖励',
};

/**
 * 分档档位 → 福利通道动作 id。
 *
 * A2A 的 `signSingle(id, mode)` 是一个端点跑三档；AIGX 把这三档落在福利通道的
 * `keepalive` / `claim` / `keepalive_claim` 三个动作上（后端 `benefits::run`
 * 的分派表）。语义一一对应，所以界面照旧出三颗按钮，只是调用路径不同。
 */
export const MODE_BENEFIT_ACTION: Record<string, string> = {
  keepalive: 'keepalive',
  claim: 'claim',
  full: 'keepalive_claim',
};

/** 该家申报的档位（A2A `modesOf`：空 = 只有签到） */
export function modesOf(group: CheckinProviderGroup | undefined): string[] {
  const modes = group?.modes;
  if (!Array.isArray(modes) || modes.length === 0) return ['checkin'];
  return modes.filter((mode): mode is string => typeof mode === 'string' && mode.length > 0);
}

/** 一组行按提供商归类（新手福利 / 活动福利 / 成长任务共用） */
export function groupByProvider<T extends { provider?: string }>(
  rows: T[],
): Array<{ id: string; rows: T[] }> {
  const map = new Map<string, T[]>();
  for (const row of rows) {
    const id = row.provider ?? '';
    const list = map.get(id);
    if (list) list.push(row);
    else map.set(id, [row]);
  }
  return [...map].map(([id, list]) => ({ id, rows: list }));
}

/** 按状态过滤（A2A `byStatus`：`doneOf` 由分区自己给判据） */
export function byStatus<T>(rows: T[], filter: StatusFilter, doneOf: (row: T) => boolean): T[] {
  if (filter === 'all') return rows;
  return rows.filter((row) => (filter === 'done' ? doneOf(row) : !doneOf(row)));
}

/** 筛选三档的计数（A2A `filterOptions`：全部 / 已完成 / 未完成） */
export function filterCounts(total: number, done: number): { all: number; done: number; pending: number } {
  return { all: total, done, pending: Math.max(0, total - done) };
}

/** providerId → 展示名（快照只给 id 时退到 id） */
export function providerLabel(center: CheckinCenterData | null, id: string): string {
  const group = center?.daily.providers.find((item) => item.id === id);
  if (group?.label) return group.label;
  const option = center?.auto?.providerOptions?.find((item) => item.id === id);
  return option?.label || id;
}

/** 北京时间（UTC+8）当天序号：`claimPlans` 的毫秒值按它判「今天领没领」 */
export function beijingDayIndex(value: unknown): number {
  const stamp = Number(value);
  if (!Number.isFinite(stamp) || stamp <= 0) return NaN;
  return Math.floor((stamp + 8 * 3_600_000) / 86_400_000);
}

/** 今天在 `claimPlans` 里领过的套餐 id（A2A `claimedPlansToday` 同口径） */
export function claimedPlansToday(claimPlans: Record<string, number> | null | undefined): string[] {
  if (!claimPlans || typeof claimPlans !== 'object') return [];
  const today = beijingDayIndex(Date.now());
  return Object.entries(claimPlans)
    .filter(([, at]) => beijingDayIndex(at) === today)
    .map(([id]) => id);
}

/** 毫秒/秒时间戳 → 本地短时间（`MM-DD HH:MM`；缺失给 `—`） */
export function formatShort(value: unknown): string {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return '—';
  const ms = n < 1e11 ? n * 1000 : n;
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return '—';
  const pad = (v: number): string => String(v).padStart(2, '0');
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 毫秒/秒时间戳 → 本地完整时间（缺失给 `—`） */
export function formatTime(value: unknown): string {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return '—';
  const ms = n < 1e11 ? n * 1000 : n;
  const date = new Date(ms);
  return Number.isNaN(date.getTime()) ? '—' : date.toLocaleString();
}

/** 签到区间文案（A2A `checkinRangeText`：起止相同只显示一个时刻） */
export function checkinRangeText(start: string, end?: string): string {
  return end && end !== start ? `${start}–${end}` : start;
}

/** 一组逐家行按「生效时刻区间」分组（A2A `groupScheduleRows`） */
export function groupProviderRowsByRange<T extends { time: string; timeEnd?: string }>(
  rows: T[],
): Array<{ range: string; rows: T[] }> {
  const groups = new Map<string, T[]>();
  for (const row of rows) {
    const key = checkinRangeText(row.time, row.timeEnd);
    const list = groups.get(key);
    if (list) list.push(row);
    else groups.set(key, [row]);
  }
  return [...groups.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([range, list]) => ({ range, rows: list }));
}

/** 签到记录的结果文案（A2A `OUTCOME_VIEW`；AIGX 取词一致） */
export const RECORD_OUTCOME_LABEL: Record<string, string> = {
  success: '成功领取',
  already: '今日已领',
  pending: '未领取',
  failed: '失败',
  captcha: '需要人工验证',
};

/** 签到记录的结果 tone */
export const RECORD_OUTCOME_TONE: Record<
  string,
  'success' | 'info' | 'warning' | 'danger' | 'neutral'
> = {
  success: 'success',
  already: 'info',
  pending: 'warning',
  failed: 'danger',
  captcha: 'warning',
};

/** 签到记录来源文案（A2A `SOURCE_LABEL`） */
export const RECORD_SOURCE_LABEL: Record<string, string> = {
  schedule: '定时',
  makeup: '补签',
  startup: '补签',
  manual: '手动',
  single: '单签',
  activity: '活动',
  retry: '重试',
};

/** 记录类型文案（`kind`：与 `source` 同值双写，AIGX 契约） */
export const RECORD_KIND_LABEL: Record<string, string> = {
  manual: '手动',
  schedule: '定时',
  makeup: '补签',
  retry: '重试',
};

/**
 * 各家上游的签到链路说明（**AIGX 独有**：A2A 的 provider rail 只有名字没有说明）。
 *
 * 2026-11-24 按 v3.5.1 重构签到中心时这张表被整表删掉过——原左栏的
 * `group.description || PROVIDER_DESC[id]` 槽位没跟着搬进新的 provider rail。
 * 它讲的是「这家的签到链路是什么、一次性福利在哪个入口」：UMID 风控组件与
 * 保活链都指向福利通道，少了这句用户找不到入口。
 *
 * 只做静态展示，不参与任何逻辑。新增提供商时在这里补一句。
 */
export const PROVIDER_DESC: Record<string, string> = {
  loomy: '讯飞 Loomy：每日领取会员权益，签到即领',
  kuku: 'Kuku：每日签到领积分，签到即领',
  qoder: 'Qoder：每日签到领额度，签到即领',
  'qoder-intl': 'Qoder 国际版：签到领额度；风控组件 UMID 见福利通道',
  xiaohuanxiong: '小浣熊：桌面端 / 移动端各领一次登录积分',
  autoclaw: 'AutoClaw：每日签到领额度，签到即领',
  'autoclaw-intl': 'AutoClaw 国际版：每日签到领额度，签到即领',
  workbuddy: 'WorkBuddy：每日签到领额度，签到即领',
  'workbuddy-intl': 'WorkBuddy 国际版：签到领日活奖励；保活链见福利通道',
  trae: 'Trae：每日签到领额度，签到即领',
  codearts: 'CodeArts：每日签到领额度；新人礼见福利通道',
  officeace: 'OfficeAce：每日签到与新人礼同链，签到即领',
};

/** 取某家的签到链路说明；没登记返回空串（不编造）。 */
export function providerDesc(id: string): string {
  return PROVIDER_DESC[id] ?? '';
}
