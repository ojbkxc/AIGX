// 仪表盘统一读数出口（Agent2API units.js / report-charts.ts 的 A2A 口径移植）。
//
// 口径约定（阶段 4 定案，修「两套成本显示」bug）：
// - cost 是**原始配额单位**（pricing.calculate_cost 向上取整的 i64，
//   充值倍率 epay.price 是系统配置级换算，报表层不做币种折算）——
//   与 Logs 页 `¥{cost}` 同口径可对账；旧的 /100 显示是 bug，已废。
// - token 量级：中文「万/亿」1 位小数 ⇄ 英文 k/M（随界面语言联动，
//   照 A2A units.js 的 localTokens/englishTokens 算法）。
// - 百分比统一 1 位小数；请求数千分位（量级缩写反而看不出量级差）。

/** 与 zh 语言判定共用的窄类型（避免 lib 层依赖 i18n 实例） */
type Lang = 'zh' | 'en' | string;

const isZh = (lang?: Lang): boolean => (lang ?? 'zh').startsWith('zh');

const NUMBER_LOCALES: Record<string, string> = {
  zh: 'zh-CN',
  en: 'en-US',
};

/** 千分位整数（请求数这类整数计数走它，不用量级缩写） */
export function fmtInt(value: number | null | undefined, lang?: Lang): string {
  const n = Number(value) || 0;
  return n.toLocaleString(NUMBER_LOCALES[isZh(lang) ? 'zh' : 'en'] ?? 'en-US');
}

/** 四舍五入到 digits 位并去掉尾随零（2.40 → 2.4、1.00 → 1） */
function fix(value: number, digits: number): string {
  return String(Number(value.toFixed(digits)));
}

/** 中文量级：一万以内精确千分位，再往上按 万/亿 两档缩写（A2A localTokens 同款，
 *  含 9999.95 万向上进位成「1亿」的直觉修正） */
function localTokens(num: number): string {
  if (num < 10_000) return fmtInt(num, 'zh');
  if (num < 100_000_000) {
    const wan = Number((num / 10_000).toFixed(1));
    return wan >= 10_000 ? `${fix(num / 100_000_000, 1)}亿` : `${wan}万`;
  }
  return `${fix(num / 100_000_000, 1)}亿`;
}

/** 英文量级：<10k 千分位；<1M 用 k 1 位；其余 M 2 位（A2A englishTokens） */
function englishTokens(num: number): string {
  if (num < 10_000) return fmtInt(num, 'en');
  if (num < 1_000_000) return `${fix(num / 1000, 1)}k`;
  return `${fix(num / 1_000_000, 2)}M`;
}

/** Token 读数：概览卡、图表标注、tooltip 共用这一个出口 */
export function fmtTokens(value: number | null | undefined, lang?: Lang): string {
  if (value == null) return '—';
  const n = Number(value) || 0;
  return isZh(lang) ? localTokens(n) : englishTokens(n);
}

/**
 * 纵轴刻度文案。`max` 只在英文档下用到（整条轴统一单位，读起来不用逐刻度
 * 换算量级）；中文档按各刻度自己的量级（与 A2A formatAxis 一致）。
 */
export function fmtAxis(value: number, max: number, lang?: Lang): string {
  if (isZh(lang)) return localTokens(value);
  if (max >= 1_000_000) return `${fix(value / 1_000_000, 2)}M`;
  if (max >= 10_000) return `${fix(value / 1000, 1)}k`;
  return Number.isInteger(value) ? fmtInt(value, 'en') : value.toFixed(1);
}

/** 百分比统一 1 位小数（rate 已是 0~100 数值；null → —） */
export function fmtPct(rate: number | null | undefined): string {
  if (rate == null) return '—';
  return `${(Number(rate) || 0).toFixed(1)}%`;
}

/**
 * 成本读数：¥ + 原始配额值（口径定案见文件头；null → —）。
 * 全仪表盘唯一出口——旧 fmtMoney 与 cost/100 两套并存是 bug。
 */
export function fmtCost(value: number | null | undefined, lang?: Lang): string {
  if (value == null) return '—';
  return `¥${fmtInt(Math.round(Number(value) || 0), lang)}`;
}

// ── 图表几何（A2A report-charts.ts 移植，纯计算便于单测） ──────────

/** 每格步长的「好读」档位（A2A STEP_LADDER 逐字一致） */
const STEP_LADDER = [1, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10];

function niceStep(value: number): number {
  if (!(value > 0)) return 1;
  const pow = 10 ** Math.floor(Math.log10(value));
  const norm = value / pow;
  const step = STEP_LADDER.find((item) => item >= norm - 1e-9) ?? 10;
  return step * pow;
}

/** 纵轴上限 = 好读步长 × 4 段（刻度线固定 5 条） */
export function axisMax(peak: number): number {
  return niceStep(peak / 4) * 4;
}

/** 坐标保留 1 位小数 */
export function round1(value: number): number {
  return Math.round(value * 10) / 10;
}

/** 标注文字宽度粗估（CJK 满格、数字半格；给避让算法用，差几像素无妨） */
export function textWidth(text: string): number {
  return [...text].reduce((sum, ch) => sum + (/[\u2e80-\u9fff]/.test(ch) ? 9.7 : 5.4), 0);
}

/** 热力图分档步长的向下取整档位（A2A HEAT_STEPS） */
const HEAT_STEPS = [1, 1.25, 1.5, 2, 2.5, 3, 4, 5, 6, 8, 10];

function heatStepFloor(value: number): number {
  if (!(value > 0)) return 1;
  const pow = 10 ** Math.floor(Math.log10(value));
  const norm = value / pow;
  let best = 1;
  for (const item of HEAT_STEPS) if (item <= norm + 1e-9) best = item;
  return best * pow;
}

/** 5 档阈值（0/step/2step/3step/∞）随窗口峰值现算——固定阈值在量大时
 *  会把全部格子压进最深一档失去信息量（A2A 旧 bug 的修法）。 */
export function heatLevelsFor(peak: number): number[] {
  const top = Number(peak) || 0;
  if (top <= 0) return [0, 1, 2, 3, Infinity];
  const step = Math.max(1, heatStepFloor(top / 4));
  return [0, step, step * 2, step * 3, Infinity];
}

/** 每档图例文字（`≤6000万` / `6000万–1.2亿` / `>2.4亿`） */
export function heatLevelText(level: number, thresholds: number[], lang?: Lang): string {
  if (level === 0) return '0';
  const lower = thresholds[level - 1];
  const upper = thresholds[level];
  if (!Number.isFinite(upper)) return `>${fmtTokens(lower, lang)}`;
  return lower === 0
    ? `≤${fmtTokens(upper, lang)}`
    : `${fmtTokens(lower, lang)}–${fmtTokens(upper, lang)}`;
}

/**
 * 环形图色板（与 A2A SLICE_COLORS 逐字一致：前 6 优先色按用量排名分配，
 * 后 10 补充色已校验色相/明度间距）。旧 PieChart 色板前两项重复
 * （'#7ca4f5' 出现两次）是 bug，这里一并消灭。
 */
export const SLICE_COLORS: readonly string[] = [
  '#f73b00', '#f7bb07', '#4aaa4d', '#1872cb', '#3444a3', '#13c2c2',
  '#722ed1', '#eb2f96', '#a0d911', '#34d399', '#d946ef', '#ff85c0',
  '#b37feb', '#9d174d', '#ffa39e', '#69b1ff',
];

/**
 * 名字 → 颜色 的映射：环形图扇区、图例与趋势 tooltip 的模型色点共用一份。
 * 两处各取各的下标迟早错位（一边过滤了零值一边没过滤，颜色整体串位还不
 * 报错）——按「列表原序、全零组不占号、同名只分配一次」统一分配。
 */
export function nameColorMap<T>(
  list: T[],
  nameOf: (item: T) => string,
  valueOf: (item: T) => number,
): Map<string, string> {
  const map = new Map<string, string>();
  for (const item of list) {
    const key = (nameOf(item) || '').trim();
    if (!key || map.has(key)) continue;
    if (!((valueOf(item) || 0) > 0)) continue;
    map.set(key, SLICE_COLORS[map.size % SLICE_COLORS.length]);
  }
  return map;
}

const rad = (angle: number): number => ((angle - 90) * Math.PI) / 180;

/** 环形扇区路径（12 点起顺时针；整圆特判两段半圆拼，配 evenodd 挖空中心） */
export function donutSlicePath(
  cx: number,
  cy: number,
  outer: number,
  inner: number,
  startAngle: number,
  endAngle: number,
): string {
  const point = (radius: number, angle: number): [number, number] => [
    round1(cx + radius * Math.cos(rad(angle))),
    round1(cy + radius * Math.sin(rad(angle))),
  ];
  const sweep = endAngle - startAngle;
  if (sweep >= 359.999) {
    const [ox1, oy1] = point(outer, 0);
    const [ox2, oy2] = point(outer, 180);
    const [ix1, iy1] = point(inner, 0);
    const [ix2, iy2] = point(inner, 180);
    return (
      `M ${ox1} ${oy1} A ${outer} ${outer} 0 1 1 ${ox2} ${oy2}` +
      ` A ${outer} ${outer} 0 1 1 ${ox1} ${oy1} Z` +
      ` M ${ix1} ${iy1} A ${inner} ${inner} 0 1 0 ${ix2} ${iy2}` +
      ` A ${inner} ${inner} 0 1 0 ${ix1} ${iy1} Z`
    );
  }
  const [ox1, oy1] = point(outer, startAngle);
  const [ox2, oy2] = point(outer, endAngle);
  const [ix2, iy2] = point(inner, endAngle);
  const [ix1, iy1] = point(inner, startAngle);
  const large = sweep > 180 ? 1 : 0;
  return (
    `M ${ox1} ${oy1} A ${outer} ${outer} 0 ${large} 1 ${ox2} ${oy2}` +
    ` L ${ix2} ${iy2} A ${inner} ${inner} 0 ${large} 0 ${ix1} ${iy1} Z`
  );
}

/** 环形图几何参数（内 62% / 外 88%，与 A2A DONUT 同值） */
export const DONUT = { size: 220, inner: 0.62, outer: 0.88, padAngle: 2 };

/** tooltip 里最多列几个模型（超出归并「…还有 N 个模型」） */
export const TIP_MODEL_LIMIT = 8;

/** 元素超过这个数不再逐个挂 React tooltip，降级原生 <title>（A2A TIP_LIMIT） */
export const TIP_LIMIT = 400;

// ── 时间范围（A2A 六档含 custom；localStorage 持久化） ────────────

/** YYYY-MM-DD 本地日期键（不用 toISOString：那是 UTC，东八区会差一天） */
export function dayKey(date: Date): string {
  const pad = (v: number): string => String(v).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

export type CustomRange = { start: string; end: string };

export const DAY_KEY_RE = /^\d{4}-\d{2}-\d{2}$/;

const RANGE_KEY = 'aigx-dashboard-range';
const CUSTOM_RANGE_KEY = 'aigx-dashboard-custom-range';

export function readRange(saved: string, allowed: readonly string[]): string {
  try {
    const v = localStorage.getItem(RANGE_KEY);
    return v !== null && allowed.includes(v) ? v : saved;
  } catch {
    return saved;
  }
}

export function saveRange(value: string): void {
  try {
    localStorage.setItem(RANGE_KEY, value);
  } catch {
    /* 存储不可用只影响下次打开 */
  }
}

export function defaultCustomRange(): CustomRange {
  const end = new Date();
  return {
    start: dayKey(new Date(end.getFullYear(), end.getMonth(), end.getDate() - 6)),
    end: dayKey(end),
  };
}

/** 只认「两个都是 YYYY-MM-DD 且 start ≤ end」；其余回落默认（含今天近 7 天） */
export function readCustomRange(): CustomRange {
  try {
    const raw = localStorage.getItem(CUSTOM_RANGE_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as Partial<CustomRange> | null;
      const start = typeof parsed?.start === 'string' ? parsed.start : '';
      const end = typeof parsed?.end === 'string' ? parsed.end : '';
      if (DAY_KEY_RE.test(start) && DAY_KEY_RE.test(end) && start <= end) {
        return { start, end };
      }
    }
  } catch {
    /* 存坏就用默认 */
  }
  return defaultCustomRange();
}

export function saveCustomRange(range: CustomRange): void {
  try {
    localStorage.setItem(CUSTOM_RANGE_KEY, JSON.stringify(range));
  } catch {
    /* 忽略 */
  }
}

/** `YYYY-MM-DD` → 本地零点（避开 new Date(str) 的 UTC 解析陷阱） */
export function parseDay(key: string): Date {
  const [y, m, d] = String(key || '').split('-').map(Number);
  return new Date(y || 1970, (m || 1) - 1, d || 1);
}

/** 日期写成人话（tooltip 用）：`9月17日`（英文档 `Sep 17`） */
export function dayLabel(key: string, lang?: Lang): string {
  const date = parseDay(key);
  if (isZh(lang)) return `${date.getMonth() + 1}月${date.getDate()}日`;
  return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
}

// ── CSV 导出（报表区共用） ────────────────────────────────────────

/** 下载一份文本（BOM 前缀让 Excel 认 UTF-8） */
export function downloadText(filename: string, content: string): void {
  const blob = new Blob(['' + content], { type: 'text/csv;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}

/** 数组 → CSV（表头取键序；null 空值；引号转义照 RFC 4180） */
export function toCsv(rows: Array<Record<string, unknown>>): string {
  if (!rows.length) return '';
  const heads = Object.keys(rows[0]);
  const esc = (v: unknown): string => {
    const s = v == null ? '' : String(v);
    return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  return [heads.join(','), ...rows.map((r) => heads.map((h) => esc(r[h])).join(','))].join('\n');
}
