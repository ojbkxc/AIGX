import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api } from '../api';
import type {
  PurgeAppliedDetail,
  PurgeData,
  PurgePresetBrief,
  PurgePresetDetail,
  PurgePresetList,
  PurgeRewriteRule,
  PurgeSettings,
  PurgeStats,
  PurgeWindowedStats,
  PurgeTemplate,
} from '../types';
import {
  Button,
  Card,
  Input,
  Select,
  Skeleton,
  SkeletonCards,
  SkeletonList,
  StatCard,
  Switch,
  Textarea,
} from '../components/ui';
import { downloadText, toCsv } from '../lib/format';
import { useToast } from '../components/Toast';
import './PurgeConsole.css';

type BoolKey =
  | 'enabled'
  | 'system_normalize'
  | 'dead_zone_rewrite'
  | 'synonym_enabled'
  | 'reduce_reasoning'
  | 'cross_source_fallback'
  | 'skill_inject'
  | 'structured_prompt'
  | 'auto_preset';

const SWITCHES: Array<{ key: BoolKey; label: string; desc: string }> = [
  {
    key: 'enabled',
    label: '破甲总开关',
    desc: '关闭后只做纯转发，不判定、不重试、不改写',
  },
  {
    key: 'system_normalize',
    label: 'system 归一化',
    desc: '合并连续 system、去重、折叠空行，功能性约束保留',
  },
  {
    key: 'dead_zone_rewrite',
    label: '死区词改写',
    desc: '按下方词表把易触发拒绝的措辞换成中性表述',
  },
  {
    key: 'synonym_enabled',
    label: '同义词替换',
    desc: '按同义词表把易触发拒绝的词换成语义相近的中性词（与死区词改写并列）',
  },
  {
    key: 'reduce_reasoning',
    label: '降思考',
    desc: '把 reasoning_effort 压到下限，防预算烧在思考上',
  },
  {
    key: 'cross_source_fallback',
    label: '异源递进',
    desc: '主模型重试到触发轮次仍失败时改用异源模型重新采样',
  },
  {
    key: 'skill_inject',
    label: '技能注入',
    desc: '命中关键词后把对应 SKILL.md 追加到 user 消息尾部',
  },
  {
    key: 'structured_prompt',
    label: '结构化提示',
    desc: '首轮追加格式指令，减少空答',
  },
  {
    key: 'auto_preset',
    label: '按模型自动套用',
    desc: '请求发出前按模型找内置预设，命中就把正文原样前插到 system 提示词；名字对不上就不套',
  },
];

/** 可编辑数字字段：输入态用字符串镜像（允许清空/半输入），保存时统一校验并夹取，杜绝 NaN */
type NumKey = 'skill_inject_limit' | 'fallback_trigger_attempt' | 'retry_limit';

const NUM_FIELDS: Array<{ key: NumKey; label: string; min: number; max: number }> = [
  { key: 'skill_inject_limit', label: '技能注入上限（字符）', min: 16, max: 20000 },
  { key: 'fallback_trigger_attempt', label: '异源触发轮次', min: 1, max: 32 },
  { key: 'retry_limit', label: '重试上限', min: 1, max: 32 },
];

function splitList(value: string): string[] {
  return value
    .split(/[,，\s]+/)
    .map((item) => item.trim())
    .filter(Boolean);
}

/** 换行分隔的词表（保留含逗号/空格的短语原文，拒绝话术词表用） */
function splitLines(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter(Boolean);
}

/**
 * 找出词表中重复的 key（大小写不敏感）。
 * 返回值是「重复的原文 → 出现下标列表」，首个出现项不算冲突目标，
 * 用于行内提示「该原文与第 N 行重复」。
 */
function findDuplicates(items: string[]): Map<string, number[]> {
  const seen = new Map<string, number[]>();
  items.forEach((item, index) => {
    const key = item.trim().toLowerCase();
    if (!key) return;
    const list = seen.get(key);
    if (list) list.push(index);
    else seen.set(key, [index]);
  });
  // 只保留真正出现 ≥2 次的
  for (const [key, list] of seen) {
    if (list.length < 2) seen.delete(key);
  }
  return seen;
}

/** 降档目标候选（clamp_reasoning 只压 high/medium/xhigh/max 档） */
const REASONING_FLOORS: Array<{ value: string; label: string }> = [
  { value: 'low', label: 'low（默认）' },
  { value: 'medium', label: 'medium' },
  { value: 'high', label: 'high' },
  { value: 'minimal', label: 'minimal' },
];

/** 后端 `normalize` 对两张词表的保留上限（超出静默截断，前端据此提前警示） */
const RULE_TABLE_CAP = 200;
/** 后端 `normalize` 对异源模型列表的保留上限（同上） */
const FALLBACK_MODELS_CAP = 16;
/** 后端 `normalize` 对若干字符串列表（剥离前缀/拒绝话术/失败关键词）的保留上限 */
const LIST_CAP = 64;
/** 后端 `normalize` 对结构化模板的保留上限 */
const TEMPLATES_CAP = 32;

/**
 * 列表字段超限提示（`null` = 未超限）。
 *
 * 与后端 `normalize` 的 `take(N)` 一一对齐：超限部分保存时被丢弃，
 * 必须在输入框当场告知（静默截断是本模块反复踩的坑）。
 */
export function capError(
  items: readonly unknown[] | undefined,
  cap: number,
  label?: string,
): string | undefined {
  const n = items?.length ?? 0;
  if (n <= cap) return undefined;
  return label
    ? `${label}：已填 ${n} 项，上限 ${cap} 项，保存只保留前 ${cap} 项`
    : `已填 ${n} 项，上限 ${cap} 项：保存只保留前 ${cap} 项`;
}

/**
 * 配置体检告警 code → 修复入口锚点（`data-purge-fix` 属性值）。
 * 与后端 `config_warning_items` 的 code 注释保持同步。
 */
const FIX_TARGETS: Record<string, string> = {
  fallback_empty: 'fallback-models',
  fallback_after_retry: 'fallback-attempts',
  fallback_duplicate: 'fallback-models',
  skill_dir_empty: 'skill-dir',
  skill_keyword_short: 'skill-snapshot',
  chained_rewrite: 'dead-zone-table',
  dead_zone_all_disabled: 'dead-zone-table',
  synonym_all_disabled: 'synonym-table',
  structured_empty: 'templates',
  preset_missing: 'auto-preset-id',
};

/** 滚动到修复入口并聚焦（输入框 focus；卡片/表格只滚动高亮）。 */
function focusFixTarget(anchor: string): void {
  const el = document.querySelector(`[data-purge-fix="${anchor}"]`);
  if (!el) return;
  el.scrollIntoView({ behavior: 'smooth', block: 'center' });
  el.classList.add('purge-fix-flash');
  window.setTimeout(() => el.classList.remove('purge-fix-flash'), 1800);
  if (el instanceof HTMLElement) el.focus({ preventScroll: true });
}

/**
 * 预览输出 diff：行级 LCS 对齐 + 行内词级高亮（未变行原样）。
 *
 * 返回带标记的行序列，渲染层把 `-`/`+` 染红绿、`~` 拆前后缀染色。
 * 规模控制：行 DP O(行数²)，预览样例几十行内；行内词级对「成对出现的
 * 改写行」做词 LCS（词数几十内）。超过 400 行整段退化为：
 * 旧文本整红 + 新文本整绿（不追精确 diff，防卡顿）。
 */
export type PreviewDiffRow =
  | { kind: 'ctx'; text: string }
  | { kind: 'del'; text: string }
  | { kind: 'add'; text: string }
  | { kind: 'mod'; del: string; add: string };

/** 词级切分：CJK 单字成词 + 连续 ASCII 字母数字/符号段成词。 */
function splitTokens(line: string): string[] {
  const tokens: string[] = [];
  let buf = '';
  const isCjk = (ch: string): boolean => /[\u3000-\u9fff\uff00-\uffef]/.test(ch);
  for (const ch of line) {
    if (isCjk(ch)) {
      if (buf) { tokens.push(buf); buf = ''; }
      tokens.push(ch);
    } else if (/\s/.test(ch)) {
      if (buf) { tokens.push(buf); buf = ''; }
      tokens.push(ch);
    } else {
      buf += ch;
    }
  }
  if (buf) tokens.push(buf);
  return tokens;
}

/** 行级 LCS（O(n·m)，双序列回溯）。空串安全。 */
function lcsRows(a: string[], b: string[]): Array<{ take: 'a' | 'b' | 'both' }> {
  const n = a.length;
  const m = b.length;
  // DP 表压两行
  let prev = new Array<number>(m + 1).fill(0);
  let cur = new Array<number>(m + 1).fill(0);
  // 回溯需要的全表——行数小（≤400）可以接受 O(n·m) 数字矩阵
  const table: number[][] = [];
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      cur[j] = a[i] === b[j] ? prev[j + 1] + 1 : Math.max(prev[j], cur[j + 1]);
    }
    table.unshift(cur.slice());
    const swap = prev; prev = cur; cur = swap;
  }
  // table[i][j] = a[i..] 与 b[j..] 的 LCS 长度
  const out: Array<{ take: 'a' | 'b' | 'both' }> = [];
  // 注意 table 存的行序：unshift 使 table[i] 对应 a 从 i 起
  let i = 0; let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) { out.push({ take: 'both' }); i += 1; j += 1; }
    else if ((table[i + 1]?.[j] ?? 0) >= (table[i]?.[j + 1] ?? 0)) { out.push({ take: 'a' }); i += 1; }
    else { out.push({ take: 'b' }); j += 1; }
  }
  while (i < n) { out.push({ take: 'a' }); i += 1; }
  while (j < m) { out.push({ take: 'b' }); j += 1; }
  return out;
}

/** 主入口：old → new 的行级 diff；同位增删行折叠为 `mod`（行内词级染色由渲染层做）。 */
export function diffLines(oldText: string, newText: string): PreviewDiffRow[] {
  const a = oldText.split('\n');
  const b = newText.split('\n');
  if (a.length * b.length > 400 * 400) {
    // 超大样例：整体红绿，不做精确 diff
    return [
      ...a.map((text): PreviewDiffRow => ({ kind: 'del', text })),
      ...b.map((text): PreviewDiffRow => ({ kind: 'add', text })),
    ];
  }
  const steps = lcsRows(a, b);
  const out: PreviewDiffRow[] = [];
  let ai = 0; let bi = 0;
  let pendingDel: string[] = [];
  let pendingAdd: string[] = [];
  const flush = (): void => {
    // 配对折叠：取 min(len) 对成 mod 行（渲染层做行内词级染色），余量整行 del/add
    const pairs = Math.min(pendingDel.length, pendingAdd.length);
    for (let k = 0; k < pairs; k += 1) {
      out.push({ kind: 'mod', del: pendingDel[k], add: pendingAdd[k] });
    }
    for (let k = pairs; k < pendingDel.length; k += 1) out.push({ kind: 'del', text: pendingDel[k] });
    for (let k = pairs; k < pendingAdd.length; k += 1) out.push({ kind: 'add', text: pendingAdd[k] });
    pendingDel = []; pendingAdd = [];
  };
  for (const step of steps) {
    if (step.take === 'both') { flush(); out.push({ kind: 'ctx', text: a[ai] }); ai += 1; bi += 1; }
    else if (step.take === 'a') { pendingDel.push(a[ai]); ai += 1; }
    else { pendingAdd.push(b[bi]); bi += 1; }
  }
  flush();
  return out;
}

/**
 * `mod` 行内部的词级 diff（渲染用）：返回 `{ keep1, del, keep2, add, keep3 }`
 * ——keep 部分原样渲染，del/add 染色。首尾公共前后缀先剥离；中段用
 * 贪心双指针求公共子序列做 keep2，其余归 del / add（词数几十内，足够快）。
 */
export function diffTokensInLine(
  oldLine: string,
  newLine: string,
): { keep1: string; del: string; keep2: string; add: string; keep3: string } {
  if (oldLine === newLine) return { keep1: oldLine, del: '', keep2: '', add: '', keep3: '' };
  const a = splitTokens(oldLine);
  const b = splitTokens(newLine);
  let p = 0;
  while (p < a.length && p < b.length && a[p] === b[p]) p += 1;
  let s = 0;
  while (
    s < a.length - p && s < b.length - p &&
    a[a.length - 1 - s] === b[b.length - 1 - s]
  ) s += 1;
  const keep1 = a.slice(0, p).join('');
  const keep3 = a.slice(a.length - s).join('');
  const midA = a.slice(p, a.length - s);
  const midB = b.slice(p, b.length - s);
  if (midA.length === 0 || midB.length === 0) {
    return { keep1, del: midA.join(''), keep2: '', add: midB.join(''), keep3 };
  }
  let del = '';
  let add = '';
  let keep2 = '';
  let i = 0; let j = 0;
  while (i < midA.length && j < midB.length) {
    if (midA[i] === midB[j]) { keep2 += midA[i]; i += 1; j += 1; }
    else if (midB.slice(j + 1).includes(midA[i])) { add += midB[j]; j += 1; }
    else { del += midA[i]; i += 1; }
  }
  while (i < midA.length) { del += midA[i]; i += 1; }
  while (j < midB.length) { add += midB[j]; j += 1; }
  return { keep1, del, keep2, add, keep3 };
}

/**
 * 预览输入解析：支持两种形态。
 *
 * 1. 裸文本（现状）：整段视作单条 user 消息。
 * 2. 角色前缀行：以 `system:` / `user:` / `assistant:`（大小写不限，冒号后
 *    至少一个空白）开头的行开启一条消息；后续无前缀行并入当前消息。
 *    粘贴整段对话即可进 dry-run——归一化作用于多条 system，死区词/同义词
 *    同时改 system+user（与线上链路口径一致）。
 *
 * 不含 `tool` 角色：改写链只处理 system/user，开放只会误导。
 */
export function parsePreviewMessages(
  text: string,
): Array<{ role: 'system' | 'user' | 'assistant'; content: string }> {
  const out: Array<{ role: 'system' | 'user' | 'assistant'; content: string }> = [];
  let current: { role: 'system' | 'user' | 'assistant'; lines: string[] } | null = null;
  const ROLE_RE = /^(system|user|assistant):\s+(.*)$/i;
  const flush = (): void => {
    if (current) {
      // 尾部空行只可能是「结尾空白/残留 CR」——剔除；内容中间的空行保留。
      while (current.lines.length > 0 && current.lines[current.lines.length - 1].trim() === '') {
        current.lines.pop();
      }
      const content = current.lines.join('\n');
      if (content.trim() !== '') out.push({ role: current.role, content });
    }
    current = null;
  };
  for (const rawLine of text.split(/\r?\n/)) {
    const m = rawLine.match(ROLE_RE);
    if (m) {
      flush();
      current = {
        role: m[1].toLowerCase() as 'system' | 'user' | 'assistant',
        lines: [m[2]],
      };
    } else if (current) {
      current.lines.push(rawLine);
    } else {
      // 无前缀引导且尚未开消息：裸文本形态，整段累积为 user
      current = { role: 'user', lines: [rawLine] };
    }
  }
  flush();
  return out;
}

/**
 * 词表 CSV 解析（RFC 4180 最小实现：引号包裹/双引号转义/换行内嵌/CRLF/BOM）。
 *
 * 只接受两列；首行表头（`from,to` 或中文列名变体）可选；空行跳过；
 * 任何行不是两列或 `from` 为空 → 整单拒绝（`null`），不做半截导入。
 * 与 `lib/format.ts::toCsv` 对偶：本函数的输出可直接 `toCsv` 回写。
 */
export function parseRulesCsv(text: string): { from: string; to: string }[] | null {
  const rows: string[][] = [];
  let field = '';
  let row: string[] = [];
  let quoted = false;
  const pushField = (): void => {
    row.push(field);
    field = '';
  };
  const pushRow = (): void => {
    pushField();
    rows.push(row);
    row = [];
  };
  const src = text.replace(/^\uFEFF/, '');
  for (let i = 0; i < src.length; i += 1) {
    const ch = src[i];
    if (quoted) {
      if (ch === '"') {
        if (src[i + 1] === '"') {
          field += '"';
          i += 1;
        } else {
          quoted = false;
        }
      } else {
        field += ch;
      }
    } else if (ch === '"') {
      quoted = true;
    } else if (ch === ',') {
      pushField();
    } else if (ch === '\n' || ch === '\r') {
      if (ch === '\r' && src[i + 1] === '\n') i += 1;
      pushRow();
    } else {
      field += ch;
    }
  }
  if (field !== '' || row.length > 0) pushRow();
  const body = rows.filter((r) => r.length > 0 && r.some((cell) => cell.trim() !== ''));
  if (body.length === 0) return [];
  // 首行是表头（两列且都命中已知表头词）→ 丢弃。
  const HEAD_WORDS = new Set(['from', 'to', '原文', '改写为', '敏感词', '同义词']);
  const first = body[0].map((cell) => cell.trim().toLowerCase());
  if (first.length === 2 && first.every((cell) => HEAD_WORDS.has(cell))) {
    body.shift();
  }
  const out: { from: string; to: string }[] = [];
  for (const cells of body) {
    if (cells.length !== 2) return null;
    const from = cells[0].trim();
    const to = cells[1].trim();
    if (!from) return null;
    out.push({ from, to });
  }
  return out;
}

function formatUptime(total: number, units: { sec: string; min: string; hour: string; day: string }): string {
  const seconds = Math.max(0, Math.floor(total));
  if (seconds < 60) return `${seconds} ${units.sec}`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} ${units.min}`;
  if (seconds < 86400) {
    return `${Math.floor(seconds / 3600)} ${units.hour} ${Math.floor((seconds % 3600) / 60)} ${units.min}`;
  }
  return `${Math.floor(seconds / 86400)} ${units.day}`;
}

// ── 预览样例历史（localStorage，最近 8 条；微调词表时反复对照用）──────────

const PREVIEW_HISTORY_KEY = 'aigx.purge.previewHistory';
const PREVIEW_HISTORY_MAX = 8;
/** 单条上限：超出不入库（防整段协议粘贴把 localStorage 撑爆，5MB 限额） */
const PREVIEW_HISTORY_ITEM_MAX = 4096;

/** 读历史：坏 JSON / 非字符串数组 → 空表。 */
export function readPreviewHistory(get: (k: string) => string | null): string[] {
  try {
    const raw = JSON.parse(get(PREVIEW_HISTORY_KEY) ?? '[]');
    if (!Array.isArray(raw)) return [];
    return raw.filter((x): x is string => typeof x === 'string').slice(0, PREVIEW_HISTORY_MAX);
  } catch {
    return [];
  }
}

/** 写历史：新样例去重置顶（去掉旧同值），截断到 8 条。 */
export function writePreviewHistory(
  items: string[],
  sample: string,
  set: (k: string, v: string) => void,
): string[] {
  const next = [
    sample,
    ...items.filter((x) => x !== sample),
  ].filter((x) => x.trim() !== '').slice(0, PREVIEW_HISTORY_MAX);
  try {
    set(PREVIEW_HISTORY_KEY, JSON.stringify(next));
  } catch {
    // localStorage 满/禁用：静默，预览本体不受影响
  }
  return next;
}

/** 历史条目的展示摘要：首行截 46 字符（角色前缀形态保留前缀可读性）。 */
export function previewHistoryLabel(sample: string): string {
  const first = sample.split(/\r?\n/, 1)[0] ?? '';
  const prefix = /^(system|user|assistant):\s/i.test(first) ? '~ ' : '';
  const text = first.replace(/^(system|user|assistant):\s+/i, (m) => m);
  return prefix + (text.length > 46 ? `${text.slice(0, 46)}…` : text);
}

/**
 * 模板 JSON 导入解析：接受单对象 `{name, text}`、数组 `[{...}]` 或配置级
 * `{structured_templates: [...]}`（本页「导出配置」的产物反过来可导）。
 *
 * 校验：name/text 必须字符串；name 去空白后非空、≤64 字符；text ≤128KB；
 * name 相同（大小写不敏感）→后者跳过（同名 upsert 由调用方做 UI 提示）。
 * 返回 `{ templates, skipped }` 或错误串。
 */
export function parseTemplateJson(
  raw: string,
): { templates: Array<{ name: string; text: string }>; skipped: number } | { error: string } {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return { error: 'not-json' };
  }
  // 配置级形态取内嵌数组
  const holder = value as { structured_templates?: unknown };
  if (holder && typeof holder === 'object' && Array.isArray(holder.structured_templates)) {
    value = holder.structured_templates;
  }
  const list = Array.isArray(value) ? value : [value];
  const out: Array<{ name: string; text: string }> = [];
  let skipped = 0;
  const seen = new Set<string>();
  for (const item of list) {
    if (!item || typeof item !== 'object') { skipped += 1; continue; }
    const rec = item as { name?: unknown; text?: unknown };
    const name = typeof rec.name === 'string' ? rec.name.trim() : '';
    const text = typeof rec.text === 'string' ? rec.text : '';
    if (!name || name.length > 64 || text.length > 128 * 1024) { skipped += 1; continue; }
    const key = name.toLowerCase();
    if (seen.has(key)) { skipped += 1; continue; }
    seen.add(key);
    out.push({ name, text });
  }
  if (out.length === 0) return { error: 'no-valid' };
  return { templates: out, skipped };
}

/** 数字字段字符串镜像初值（从服务端配置派生） */
function numRawOf(s: PurgeSettings): Record<NumKey, string> {
  return {
    skill_inject_limit: String(s.skill_inject_limit),
    fallback_trigger_attempt: String(s.fallback_trigger_attempt),
    retry_limit: String(s.retry_limit),
  };
}

/** 按 NumKey 写回数字字段（显式分支，避免计算键的宽化类型） */
function withNum(settings: PurgeSettings, key: NumKey, value: number): PurgeSettings {
  const next = { ...settings };
  if (key === 'skill_inject_limit') next.skill_inject_limit = value;
  else if (key === 'fallback_trigger_attempt') next.fallback_trigger_attempt = value;
  else next.retry_limit = value;
  return next;
}

/**
 * 词表单行（memo）：长词表下编辑一行只重渲染该行——行内两个 Input 是全页最贵的子树，
 * 而父组件每次按键都会新建 draft（未命中行的 rule 引用不变）。
 * 标签按值传递（字符串）而非对象，memo 的浅比较才有意义。
 */
const RuleRow = memo(function RuleRow({
  rule,
  index,
  fromLabel,
  toLabel,
  removeLabel,
  enableLabel,
  dupHint,
  onPatch,
  onRemove,
}: {
  rule: PurgeRewriteRule;
  index: number;
  fromLabel: string;
  toLabel: string;
  removeLabel: string;
  /** 单条启停的无障碍标签 */
  enableLabel: string;
  /** 与其他行原文重复时的行内提示（后端会静默截断重复规则，前端显式提示） */
  dupHint?: string;
  onPatch: (index: number, part: Partial<PurgeRewriteRule>) => void;
  onRemove: (index: number) => void;
}): JSX.Element {
  const enabled = rule.enabled ?? true;
  return (
    <div className={`purge-rule-row${enabled ? '' : ' is-disabled'}`}>
      <div>
        <Input
          placeholder={fromLabel}
          aria-label={fromLabel}
          value={rule.from}
          onChange={(event) => onPatch(index, { from: event.target.value })}
        />
        {dupHint ? <span className="purge-dup-hint">{dupHint}</span> : null}
      </div>
      <span className="purge-arrow" aria-hidden="true">→</span>
      <Input
        placeholder={toLabel}
        aria-label={toLabel}
        value={rule.to}
        onChange={(event) => onPatch(index, { to: event.target.value })}
      />
      <label className="purge-rule-toggle" title={enableLabel}>
        <input
          type="checkbox"
          checked={enabled}
          aria-label={enableLabel}
          onChange={(event) => onPatch(index, { enabled: event.target.checked })}
        />
      </label>
      <Button variant="outline" size="sm" onClick={() => onRemove(index)}>
        {removeLabel}
      </Button>
    </div>
  );
});

/**
 * 统计卡组（memo）：入参只有服务端快照派生值 + 窗口摘要，编辑表单文本时不重画。
 *
 * `suffix`：窗口标记（如「近 1 小时」）拼在各卡标题后；`atLeast`
 * 为 true 时值前缀「≥」（事件环溢出后窗口值为下界估计）。
 */
const StatsGrid = memo(function StatsGrid({
  stats,
  retryRate,
  suffix,
  atLeast,
  t,
}: {
  stats: PurgeStats;
  retryRate: string;
  suffix?: string;
  atLeast?: boolean;
  t: TFunction;
}): JSX.Element {
  const mk = (base: string): string => (suffix ? `${base} · ${suffix}` : base);
  const v = (n: number): string => (atLeast ? `≥ ${n}` : String(n));
  return (
    <div className="purge-stats" role="group" aria-label={t('统计')}>
      <StatCard title={mk(t('响应总数'))} value={v(stats.responses)} />
      <StatCard title={mk(t('重试率'))} value={`${atLeast ? '≥ ' : ''}${retryRate}%`} />
      <StatCard title={mk(t('重试次数'))} value={v(stats.retries)} />
      <StatCard title={mk(t('提前收效'))} value={v(stats.early_success)} />
      <StatCard title={mk(t('技能命中'))} value={v(stats.skill_hits)} />
      <StatCard title={mk(t('拒绝样本'))} value={v(stats.refusals)} />
      <StatCard title={mk(t('同义词命中'))} value={v(stats.synonym_hits)} />
      <StatCard title={mk(t('自动套用'))} value={v(stats.auto_preset_hits ?? 0)} />
    </div>
  );
});

/**
 * 失败分类条（memo）：把 refusal/empty/filter/other 四个终态失败计数
 * 从参数网格里的「N / N / N / N」文本升级为带语义色的横条，一眼看出失败构成。
 */
const FailureBar = memo(function FailureBar({
  refusal,
  empty,
  filter,
  other,
  t,
}: {
  refusal: number;
  empty: number;
  filter: number;
  other: number;
  t: TFunction;
}): JSX.Element {
  const items = [
    { label: t('拒绝'), value: refusal, tone: 'danger' },
    { label: t('空答'), value: empty, tone: 'warning' },
    { label: t('过滤'), value: filter, tone: 'accent' },
    { label: t('其它'), value: other, tone: 'muted' },
  ];
  return (
    <div className="purge-failure-bar" role="group" aria-label={t('失败分类')}>
      {items.map((item) => (
        <div key={item.label} className={`purge-failure-cell tone-${item.tone}`}>
          <span className="purge-failure-value">{item.value}</span>
          <span className="purge-failure-label">{item.label}</span>
        </div>
      ))}
    </div>
  );
});

/**
 * 开关行（memo）：`flags` 由父组件 useMemo 按 8 个原语派生，数组引用只在开关本身变化时变，
 * 故编辑文本/词表不会连带重渲染这 8 行。
 */
const SwitchRows = memo(function SwitchRows({
  flags,
  disabled,
  onToggle,
  t,
}: {
  flags: boolean[];
  disabled: boolean;
  onToggle: (key: BoolKey, next: boolean) => void;
  t: TFunction;
}): JSX.Element {
  return (
    <div className="purge-rows">
      {SWITCHES.map((item, index) => (
        <div key={item.key} className="purge-switch-row">
          <div className="purge-switch-text">
            <div className="purge-row-title">{t(item.label)}</div>
            <div className="purge-row-desc">{t(item.desc)}</div>
          </div>
          <Switch
            checked={flags[index]}
            onChange={(next) => onToggle(item.key, next)}
            disabled={disabled}
            ariaLabel={t(item.label)}
          />
        </div>
      ))}
    </div>
  );
});

/**
 * 只读参数网格（memo）：全部原语入参（含 runtime.inflight 即时并发数），文本编辑不牵连。
 */
const ParamGrid = memo(function ParamGrid({
  retryLimit,
  failureRefusal,
  failureEmpty,
  failureFilter,
  failureOther,
  fallbackModels,
  fallbackTriggerAttempt,
  skillInjectLimit,
  inflight,
  upstream,
  t,
}: {
  retryLimit: number;
  failureRefusal: number;
  failureEmpty: number;
  failureFilter: number;
  failureOther: number;
  fallbackModels: string;
  fallbackTriggerAttempt: number;
  skillInjectLimit: number;
  inflight: number | undefined;
  upstream: string;
  t: TFunction;
}): JSX.Element {
  return (
    <div className="purge-param-grid">
      <div className="purge-param">
        <span>{t('重试上限')}</span>
        <strong>{retryLimit}</strong>
      </div>
      <div className="purge-param">
        <span>{t('失败分类（拒绝/空答/过滤/其它）')}</span>
        <strong>
          {failureRefusal} / {failureEmpty} / {failureFilter} / {failureOther}
        </strong>
      </div>
      <div className="purge-param">
        <span>{t('异源模型')}</span>
        <strong>{fallbackModels || '—'}</strong>
      </div>
      <div className="purge-param">
        <span>{t('异源触发轮次')}</span>
        <strong>{fallbackTriggerAttempt}</strong>
      </div>
      <div className="purge-param">
        <span>{t('技能注入上限')}</span>
        <strong>{skillInjectLimit}</strong>
      </div>
      <div className="purge-param">
        <span>{t('并发')}</span>
        <strong>{typeof inflight === 'number' ? inflight : '—'}</strong>
      </div>
      <div className="purge-param">
        <span>{t('上游')}</span>
        <strong>{upstream || '—'}</strong>
      </div>
    </div>
  );
});

/**
 * 预设落成模板前是否需要用户确认覆盖。
 *
 * 只有「同名模板已存在 **且** 正文不同」才需要确认：
 * - 不存在同名 → 纯新增，无覆盖风险，直接落。
 * - 同名且正文完全一致 → 重复点击同一预设，静默幂等（不弹确认）。
 */
export function presetApplyNeedsConfirm(
  templates: Array<{ name: string; text: string }> | undefined,
  name: string,
  text: string,
): boolean {
  const hit = (templates ?? []).find((tpl) => tpl.name === name);
  return hit !== undefined && hit.text !== text;
}

/**
 * 列表条目 + `model_match`。
 *
 * `model_match` 是后端「模型→预设」匹配结果（exact / vendor / mismatch / unknown），
 * 与 Rust 侧 `PresetMatch` 的 snake_case 序列化一致；旧版后端不返回该字段，
 * 此时按「未匹配」处理、不渲染徽标——与 config_warnings / skills 的向前兼容写法同口径。
 * 声明在本页而非 types 里，是因为 wire 侧尚未把该字段并入 PurgePresetBrief。
 */
type PresetBriefWithMatch = PurgePresetBrief & {
  model_match?: 'exact' | 'vendor' | 'mismatch' | 'unknown' | null;
};

/**
 * 破甲预设库：内置 prompt 集合的浏览与落盘。
 *
 * 三条动作各自独立：
 * - 设为结构化模板：把正文写进父组件的草稿（不直接 PUT，需手动保存）；
 * - 复制全文：只进剪贴板；
 * - 写入技能目录：后端直接写盘，成功后刷新技能快照。
 *
 * 搜索防抖 250ms：预设库有 50+ 条，每次按键都打后端没必要。
 */
const PresetsSection = memo(function PresetsSection({
  t,
  addToast,
  onApplyTemplate,
  onSkillWritten,
  currentTemplates,
}: {
  t: TFunction;
  addToast: (message: string, type?: 'success' | 'error' | 'warning') => void;
  onApplyTemplate: (name: string, text: string) => void;
  onSkillWritten: () => void;
  /** 草稿里的现有模板（覆盖同名时先确认） */
  currentTemplates?: Array<{ name: string; text: string }>;
}): JSX.Element {
  const [q, setQ] = useState('');
  const [debouncedQ, setDebouncedQ] = useState('');
  const [vendor, setVendor] = useState('');
  const [list, setList] = useState<PurgePresetList | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loading, setLoading] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<PurgePresetDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  /** 待确认的覆盖（同名模板正文不同；null = 无待确认） */
  const [confirmOverwrite, setConfirmOverwrite] = useState<
    { id: string; name: string; text: string } | null
  >(null);
  /**
   * 当前默认模型（后端 settings 的 default_model）：预设列表按它做模型匹配。
   * 取数口径与「系统设置 → 运维 → 客户端兼容」一致，走 api.getLimits() 的 defaultModel。
   */
  const [defaultModel, setDefaultModel] = useState('');
  /** 默认模型取数是否已完成（含失败）：列表首屏就带上 model，而不是先发一次不带的再补发 */
  const [modelReady, setModelReady] = useState(false);

  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedQ(q.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [q]);

  // 默认模型只在进页面时取一次：它归系统设置管，本页只读，不随预设操作变化。
  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const res = await api.getLimits();
        if (!alive) return;
        const d = res?.data as Record<string, unknown> | undefined;
        setDefaultModel(String(d?.defaultModel ?? '').trim());
      } catch {
        // 取不到就不带 model 参数，列表退回「不按模型过滤」的原口径
      } finally {
        if (alive) setModelReady(true);
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  // 列表：搜索词 / 厂商 / 默认模型变化时重拉（不传 limit，用后端默认 50）
  useEffect(() => {
    if (!modelReady) return;
    let alive = true;
    setLoading(true);
    void (async () => {
      try {
        const res = await api.listPurgePresets({
          q: debouncedQ || undefined,
          vendor: vendor || undefined,
          // 只在有值时带 model：默认模型为空串（未配置）时不传，后端按未过滤处理
          model: defaultModel || undefined,
        });
        if (!alive) return;
        if (!res.success || !res.data) throw new Error('list failed');
        setList(res.data);
      } catch {
        if (alive) addToast(t('预设列表加载失败'), 'error');
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [addToast, debouncedQ, defaultModel, modelReady, t, vendor]);

  /** 加载更多：按当前已加载条数作为 offset 拉下一页并追加（total 以服务端为准） */
  const loadMorePresets = useCallback(async (): Promise<void> => {
    const current = list;
    if (!current || loadingMore) return;
    setLoadingMore(true);
    try {
      const res = await api.listPurgePresets({
        q: debouncedQ || undefined,
        vendor: vendor || undefined,
        model: defaultModel || undefined,
        offset: current.items.length,
      });
      if (!res.success || !res.data) throw new Error('load-more failed');
      const merged = [...current.items, ...res.data.items];
      setList({ ...res.data, items: merged });
    } catch {
      addToast(t('预设列表加载失败'), 'error');
    } finally {
      setLoadingMore(false);
    }
  }, [addToast, debouncedQ, defaultModel, list, loadingMore, t, vendor]);

  // 详情：选中项变化时拉全文
  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    let alive = true;
    setDetailLoading(true);
    void (async () => {
      try {
        const res = await api.getPurgePreset(selectedId);
        if (!alive) return;
        if (!res.success || !res.data) throw new Error('detail failed');
        setDetail(res.data);
      } catch {
        if (alive) {
          setDetail(null);
          addToast(t('预设正文加载失败'), 'error');
        }
      } finally {
        if (alive) setDetailLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [addToast, selectedId, t]);

  const items: PresetBriefWithMatch[] = list?.items ?? [];

  /** 拉全文（供复制用；详情已在时直接用缓存） */
  const fetchBody = useCallback(
    async (id: string): Promise<PurgePresetDetail | null> => {
      if (detail && detail.id === id) return detail;
      const res = await api.getPurgePreset(id);
      if (!res.success || !res.data) throw new Error('detail failed');
      return res.data;
    },
    [detail],
  );

  const handleApplyTemplate = useCallback(
    async (id: string): Promise<void> => {
      setBusy(`template:${id}`);
      try {
        const full = await fetchBody(id);
        if (!full) throw new Error('detail failed');
        // 同名且正文不同 → 覆盖前确认（就地二段按钮，不引弹窗栈）
        if (presetApplyNeedsConfirm(currentTemplates, full.title, full.body)) {
          setConfirmOverwrite({ id, name: full.title, text: full.body });
          return;
        }
        onApplyTemplate(full.title, full.body);
        addToast(t('已写入模板草稿，点底部「保存」后生效'), 'success');
        // 写入后立即把模板编辑区带到眼前并闪一下（词表很长时落点在页面底部）
        focusFixTarget('templates');
      } catch {
        addToast(t('应用预设失败'), 'error');
      } finally {
        setBusy(null);
      }
    },
    [addToast, currentTemplates, fetchBody, onApplyTemplate, t],
  );

  /** 用户在覆盖确认里点「覆盖」：真正落草稿 */
  const confirmOverwriteAndApply = useCallback((): void => {
    const pending = confirmOverwrite;
    if (!pending) return;
    setConfirmOverwrite(null);
    onApplyTemplate(pending.name, pending.text);
    addToast(t('已覆盖同名模板草稿，点底部「保存」后生效'), 'success');
    focusFixTarget('templates');
  }, [addToast, confirmOverwrite, onApplyTemplate, t]);

  const handleCopy = useCallback(
    async (id: string): Promise<void> => {
      setBusy(`copy:${id}`);
      try {
        const full = await fetchBody(id);
        if (!full) throw new Error('detail failed');
        await navigator.clipboard.writeText(full.body);
        addToast(t('全文已复制到剪贴板'), 'success');
      } catch {
        addToast(t('复制失败（浏览器可能禁用了剪贴板权限）'), 'error');
      } finally {
        setBusy(null);
      }
    },
    [addToast, fetchBody, t],
  );

  const handleApplySkill = useCallback(
    async (id: string): Promise<void> => {
      setBusy(`skill:${id}`);
      try {
        const res = await api.applyPurgePreset(id, 'skill_file');
        if (!res.success || !res.data) throw new Error('apply failed');
        addToast(
          t('已写入技能目录：{{path}}', { path: res.data.applied.path ?? '' }),
          'success',
        );
        onSkillWritten();
      } catch {
        addToast(t('写入技能目录失败'), 'error');
      } finally {
        setBusy(null);
      }
    },
    [addToast, onSkillWritten, t],
  );

  return (
    <Card title={t('破甲预设库')}>
      {/* 授权边界必须常驻可见：正文是第三方公开 prompt 原文，这里只搬不改 */}
      <p className="purge-preset-notice">
        {t(
          '内置 {{count}} 条第三方公开 prompt 原文，仅供授权的防御性安全评估使用。内容原样搬运、不改写；合规责任在使用方。',
          { count: list?.total ?? 0 },
        )}
      </p>
      <div className="purge-preset-toolbar">
        <Input
          placeholder={t('搜索标题 / 厂商 / 模型 / 标签')}
          aria-label={t('搜索预设')}
          value={q}
          onChange={(event) => setQ(event.target.value)}
        />
        <span className="purge-preset-total">
          {t('共 {{total}} 条', { total: list?.total ?? 0 })}
        </span>
      </div>
      <div className="purge-preset-chips" role="group" aria-label={t('按厂商过滤')}>
        <button
          type="button"
          className={`purge-preset-chip${vendor === '' ? ' is-active' : ''}`}
          onClick={() => setVendor('')}
        >
          {t('全部')}
        </button>
        {(list?.vendors ?? []).map((item) => (
          <button
            key={item.vendor}
            type="button"
            className={`purge-preset-chip${vendor === item.vendor ? ' is-active' : ''}`}
            onClick={() => setVendor(item.vendor)}
          >
            {item.vendor}
            <span className="purge-preset-chip-count">{item.count}</span>
          </button>
        ))}
      </div>

      {loading || !modelReady ? (
        <SkeletonList rows={4} />
      ) : items.length === 0 ? (
        <p className="purge-empty">{t('没有匹配的预设')}</p>
      ) : (
        <ul className="purge-preset-list">
          {items.map((item) => {
            const active = selectedId === item.id;
            return (
              <li key={item.id}>
                <button
                  type="button"
                  className={`purge-preset-item${active ? ' is-active' : ''}`}
                  data-testid="purge-preset-item"
                  aria-pressed={active}
                  onClick={() => setSelectedId(active ? null : item.id)}
                >
                  <span className="purge-preset-item-main">
                    <span className="purge-preset-item-title">
                      <strong>{item.title}</strong>
                      {item.model_match === 'exact' ? (
                        <span className="purge-preset-match">{t('默认模型')}</span>
                      ) : null}
                    </span>
                    <span className="purge-preset-item-meta">
                      {item.vendor} · {item.model}
                    </span>
                  </span>
                  <span className="purge-preset-item-side">
                    <span>{(item.bytes / 1024).toFixed(1)} KB</span>
                    <code title={item.sha256}>{item.sha256.slice(0, 8)}</code>
                  </span>
                </button>
                {active ? (
                  <div className="purge-preset-detail">
                    <div className="purge-actions">
                      <Button
                        variant="outline"
                        size="sm"
                        data-testid="purge-preset-apply-template"
                        disabled={busy !== null}
                        onClick={() => void handleApplyTemplate(item.id)}
                      >
                        {busy === `template:${item.id}` ? t('处理中…') : t('设为结构化模板')}
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busy !== null}
                        onClick={() => void handleCopy(item.id)}
                      >
                        {busy === `copy:${item.id}` ? t('处理中…') : t('复制全文')}
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        data-testid="purge-preset-apply-skill"
                        disabled={busy !== null}
                        onClick={() => void handleApplySkill(item.id)}
                      >
                        {busy === `skill:${item.id}` ? t('处理中…') : t('写入技能目录')}
                      </Button>
                    </div>
                    {/* 覆盖同名模板的就地二段确认（不引弹窗栈） */}
                    {confirmOverwrite && confirmOverwrite.id === item.id ? (
                      <div className="purge-warning-box" role="alert" data-testid="purge-preset-overwrite-confirm">
                        <p className="purge-warning-row">
                          <span>
                            {t('草稿里已有同名模板「{{name}}」且内容不同，继续将覆盖它。', {
                              name: confirmOverwrite.name,
                            })}
                          </span>
                          <Button variant="outline" size="sm" onClick={confirmOverwriteAndApply}>
                            {t('覆盖')}
                          </Button>
                          <Button variant="outline" size="sm" onClick={() => setConfirmOverwrite(null)}>
                            {t('取消')}
                          </Button>
                        </p>
                      </div>
                    ) : null}
                    {detailLoading ? (
                      <SkeletonList rows={6} />
                    ) : (
                      <pre className="purge-preset-preview" data-testid="purge-preset-preview">
                        {detail && detail.id === item.id ? detail.body : item.preview}
                      </pre>
                    )}
                  </div>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      {/* 加载更多：服务端 total 大于已加载条数时显示（默认每页 50 条） */}
      {modelReady && !loading && items.length > 0 && (list?.total ?? 0) > items.length ? (
        <div className="purge-actions">
          <Button variant="outline" size="sm" disabled={loadingMore} onClick={() => void loadMorePresets()}>
            {loadingMore
              ? t('加载中…')
              : t('加载更多（还有 {{n}} 条）', { n: (list?.total ?? 0) - items.length })}
          </Button>
        </div>
      ) : null}
    </Card>
  );
});

export default function PurgeConsole(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [data, setData] = useState<PurgeData | null>(null);
  const [draft, setDraft] = useState<PurgeSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  /**
   * 未保存修改标记：任何编辑置真，加载/保存成功置假。
   * 不用 `JSON.stringify(draft) !== JSON.stringify(data.settings)` 对比——长词表/长后缀下
   * 那是每按键一次的 O(全量) 序列化；草稿变化点全在本组件内，布尔标记语义等价且零分配。
   * 开关是「点一下立即持久化」，故不入 dirty。
   */
  const [dirty, setDirty] = useState(false);
  const [numRaw, setNumRaw] = useState<Record<NumKey, string>>({
    skill_inject_limit: '',
    fallback_trigger_attempt: '',
    retry_limit: '',
  });
  const [numErrors, setNumErrors] = useState<Partial<Record<NumKey, string>>>({});
  // 改写预览（dry-run）：样例输入 + 改写结果（不写统计）
  const [previewInput, setPreviewInput] = useState('');
  const [previewOut, setPreviewOut] = useState<string | null>(null);
  /** 改写前基线（与 previewOut 同构：单条纯文本 / 多条 `[role]` 分段），diff 用 */
  const [previewBase, setPreviewBase] = useState<string | null>(null);
  /** 预览样例历史（localStorage；成功跑过一次的输入，快速回填对照） */
  const [previewHistory, setPreviewHistory] = useState<string[]>(() =>
    readPreviewHistory((k) => window.localStorage.getItem(k)),
  );
  /** 「按已保存配置预览」：对比「线上现在会怎样」vs「保存后会怎样」 */
  const [previewSavedOnly, setPreviewSavedOnly] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [previewMeta, setPreviewMeta] = useState<{ changed: boolean; skill_hit: boolean } | null>(null);
  /**
   * 预览命中明细（后端 `AppliedDetail`，snake_case 序列化）。旧版后端不返回
   * `detail` 字段 → 保持 null，明细区不渲染（向前兼容）。
   */
  const [previewDetail, setPreviewDetail] = useState<PurgeAppliedDetail | null>(null);
  // 配置体检告警（结构化优先；旧后端只有字符串 → code 为空只读展示） + 技能快照
  const [configWarnings, setConfigWarnings] = useState<string[]>([]);
  const [configWarningItems, setConfigWarningItems] = useState<
    Array<{ code: string; message: string }>
  >([]);
  const [skills, setSkills] = useState<NonNullable<PurgeData['skills']>>([]);
  // 统计口径切换（全程/近 1 小时/近 24 小时）；提前返回之前声明（rules-of-hooks）
  const [statsWindow, setStatsWindow] = useState<'all' | '1h' | '24h' | 'last'>('all');
  // 导入配置文件选择
  const importInputRef = useRef<HTMLInputElement | null>(null);
  // 词表 CSV 导入（死区词/同义词各一个 input，value 清空允许连续导同名文件）
  const deadZoneCsvRef = useRef<HTMLInputElement | null>(null);
  const synonymCsvRef = useRef<HTMLInputElement | null>(null);
  // 模板 JSON 导入（预设粘贴 / 配置级 JSON 复用）
  const templateJsonRef = useRef<HTMLInputElement | null>(null);
  // 技能包导入（ZIP/TAR/TGZ → 技能目录）
  const [skillPkgName, setSkillPkgName] = useState('');
  const [skillPkgOverwrite, setSkillPkgOverwrite] = useState(false);
  const [skillPkgImporting, setSkillPkgImporting] = useState(false);
  const skillPkgInputRef = useRef<HTMLInputElement | null>(null);

  // PUT 互斥：开关乐观保存与底部保存共用一把锁，防止并发 PUT 互相覆盖
  const savingRef = useRef(false);
  // 服务端快照 / 草稿的非渲染引用：开关保存要以「最近一次服务端配置」为基（不夹带未保存编辑），
  // 同时让 toggle 保持稳定引用（Switch 行 memo 才有意义），故不闭包捕获 state。
  const dataRef = useRef<PurgeData | null>(null);
  const draftRef = useRef<PurgeSettings | null>(null);
  useEffect(() => {
    dataRef.current = data;
    draftRef.current = draft;
  }, [data, draft]);
  // 词表增删后的焦点回归：'add' → 最后一行首个输入；数字 → 该下标行（删除后补位行）
  const rulesRef = useRef<HTMLDivElement | null>(null);
  const focusRule = useRef<number | 'add' | null>(null);
  // 同义词词表增删后的焦点回归（与死区词表独立，互不干扰）
  const synRulesRef = useRef<HTMLDivElement | null>(null);
  const focusSynRule = useRef<number | 'add' | null>(null);

  // 规则重复检测（大小写不敏感，按 trim 后原文）：
  // 死区词表 / 同义词表 / 模板名各自独立检测；重复项保存前给行内提示（不阻断保存，
  // 与后端 normalize 的静默去重互补——后端截断静默丢弃，前端显式提示）。
  const dupRules = useMemo(
    () => findDuplicates(draft?.rewrite_rules.map((r) => r.from) ?? []),
    [draft?.rewrite_rules],
  );
  const dupSynonyms = useMemo(
    () => findDuplicates(draft?.synonym_pairs.map((r) => r.from) ?? []),
    [draft?.synonym_pairs],
  );
  const dupTemplates = useMemo(
    () => findDuplicates(draft?.structured_templates?.map((tpl) => tpl.name) ?? []),
    [draft?.structured_templates],
  );

  // 未保存离开保护：关闭/刷新页签前弹浏览器原生确认（dirty 时才挂监听）
  useEffect(() => {
    if (!dirty) return;
    const handler = (event: BeforeUnloadEvent): void => {
      event.preventDefault();
      // Chrome 需要returnValue 才会弹确认框；文案由浏览器统一提供，不可自定义
      event.returnValue = '';
    };
    window.addEventListener('beforeunload', handler);
    return () => window.removeEventListener('beforeunload', handler);
  }, [dirty]);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const res = await api.getPurge();
      if (!res.success || !res.data) throw new Error('load failed');
      setData(res.data);
      setDraft(res.data.settings);
      setNumRaw(numRawOf(res.data.settings));
      setNumErrors({});
      setConfigWarnings(res.data.config_warnings ?? []);
      setConfigWarningItems(res.data.config_warning_items ?? []);
      setSkills(res.data.skills ?? []);
      setDirty(false);
    } catch {
      addToast(t('加载失败'), 'error');
    } finally {
      setLoading(false);
    }
  }, [addToast, t]);

  useEffect(() => {
    void load();
  }, [load]);

  /** 底部保存：校验数字 → 夹取到合法区间 → PUT 整份草稿 */
  const save = useCallback(async (): Promise<void> => {
    const snapshot = dataRef.current;
    const current = draftRef.current;
    if (!current || !snapshot || savingRef.current) return;
    const invalid = NUM_FIELDS.filter((f) => {
      const raw = numRaw[f.key].trim();
      return raw === '' || !Number.isFinite(Number(raw));
    });
    if (invalid.length > 0) {
      setNumErrors(Object.fromEntries(invalid.map((f) => [f.key, t('请输入有效数字')])));
      addToast(t('存在无效输入，请检查标红的字段'), 'warning');
      return;
    }
    // 模板重名阻断保存：active_template 按名匹配，重名会让「当前启用模板」悬空命中错行
    if (dupTemplates.size > 0) {
      addToast(t('存在重复的模板名称，请先修改再保存'), 'warning');
      return;
    }
    let next = current;
    const rawNext = { ...numRaw };
    for (const f of NUM_FIELDS) {
      const clamped = Math.min(f.max, Math.max(f.min, Math.trunc(Number(numRaw[f.key]))));
      next = withNum(next, f.key, clamped);
      rawNext[f.key] = String(clamped);
    }
    savingRef.current = true;
    setSaving(true);
    setNumRaw(rawNext);
    try {
      const res = await api.savePurge(next);
      if (!res.success || !res.data) throw new Error('save failed');
      setData(res.data);
      setDraft(res.data.settings);
      setNumRaw(numRawOf(res.data.settings));
      setNumErrors({});
      setDirty(false);
      addToast(t('已保存'), 'success');
    } catch {
      addToast(t('保存失败'), 'error');
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  }, [addToast, dupTemplates.size, numRaw, t]);

  /**
   * 开关：乐观翻本键，失败只回滚该键。
   * PUT 以服务端最近一次已知配置为基（只翻这一个键），不夹带草稿里未保存的文本编辑；
   * 成功后同步 data（服务端真相）并把该键对齐服务端归一化结果，draft 的其它未保存编辑原样保留。
   * 引用稳定（依赖 refs 而非 state），使 Switch 行可被 memo 跳过重渲染。
   */
  const toggle = useCallback((key: BoolKey, next: boolean): void => {
    const snapshot = dataRef.current;
    const current = draftRef.current;
    if (!snapshot || !current || savingRef.current) return;
    const prev = Boolean(current[key]);
    if (prev === next) return;
    savingRef.current = true;
    setSaving(true);
    setDraft({ ...current, [key]: next } as PurgeSettings);
    void (async () => {
      try {
        const res = await api.savePurge({ ...snapshot.settings, [key]: next } as PurgeSettings);
        if (!res.success || !res.data) throw new Error('toggle failed');
        const fresh = res.data;
        setData(fresh);
        setDraft((d) => (d ? ({ ...d, [key]: Boolean(fresh.settings[key]) } as PurgeSettings) : d));
      } catch {
        setDraft((d) => (d ? ({ ...d, [key]: prev } as PurgeSettings) : d));
        addToast(t('保存失败'), 'error');
      } finally {
        savingRef.current = false;
        setSaving(false);
      }
    })();
  }, [addToast, t]);

  const patch = useCallback((part: Partial<PurgeSettings>): void => {
    setDirty(true);
    setDraft((d) => (d ? { ...d, ...part } : d));
  }, []);

  const patchRule = useCallback((index: number, part: Partial<PurgeRewriteRule>): void => {
    setDirty(true);
    setDraft((d) =>
      d
        ? {
            ...d,
            rewrite_rules: d.rewrite_rules.map((rule, i) => (i === index ? { ...rule, ...part } : rule)),
          }
        : d,
    );
  }, []);

  const addRule = useCallback((): void => {
    setDirty(true);
    focusRule.current = 'add';
    setDraft((d) => (d ? { ...d, rewrite_rules: [...d.rewrite_rules, { from: '', to: '' }] } : d));
  }, []);

  const removeRule = useCallback((index: number): void => {
    setDirty(true);
    focusRule.current = index;
    setDraft((d) => (d ? { ...d, rewrite_rules: d.rewrite_rules.filter((_, i) => i !== index) } : d));
  }, []);

  const patchSynonymRule = useCallback((index: number, part: Partial<PurgeRewriteRule>): void => {
    setDirty(true);
    setDraft((d) =>
      d
        ? {
            ...d,
            synonym_pairs: d.synonym_pairs.map((rule, i) => (i === index ? { ...rule, ...part } : rule)),
          }
        : d,
    );
  }, []);

  const addSynonymRule = useCallback((): void => {
    setDirty(true);
    focusSynRule.current = 'add';
    setDraft((d) => (d ? { ...d, synonym_pairs: [...d.synonym_pairs, { from: '', to: '' }] } : d));
  }, []);

  const removeSynonymRule = useCallback((index: number): void => {
    setDirty(true);
    focusSynRule.current = index;
    setDraft((d) => (d ? { ...d, synonym_pairs: d.synonym_pairs.filter((_, i) => i !== index) } : d));
  }, []);

  /** 结构化模板编辑：空模板表按空数组处理（旧后端不返回该字段） */
  const patchTemplate = useCallback((index: number, part: Partial<PurgeTemplate>): void => {
    setDirty(true);
    setDraft((d) =>
      d
        ? {
            ...d,
            structured_templates: (d.structured_templates ?? []).map((tpl, i) =>
              i === index ? { ...tpl, ...part } : tpl,
            ),
          }
        : d,
    );
  }, []);

  /**
   * 预设落盘后的技能快照刷新。
   *
   * 只重拉 `skills`，**不动 draft**——预设页与表单是两套状态，整页 `load()` 会把
   * 用户未保存的编辑抹掉。
   */
  const refreshSkills = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getPurge();
      if (res.success && res.data) setSkills(res.data.skills ?? []);
    } catch {
      // 刷新失败不影响写入结果，静默即可
    }
  }, []);

  /** 技能包导入（ZIP/TAR/TGZ → 技能目录）；成功后只刷新技能快照，不动草稿 */
  const handleImportSkill = useCallback(
    async (event: ChangeEvent<HTMLInputElement>): Promise<void> => {
      const file = event.target.files?.[0];
      event.target.value = '';
      if (!file) return;
      setSkillPkgImporting(true);
      try {
        const res = await api.importPurgeSkills(file, {
          name: skillPkgName.trim() || undefined,
          overwrite: skillPkgOverwrite,
        });
        if (!res.success || !res.data) throw new Error(t('导入技能包失败'));
        addToast(
          t('已导入技能包：{{dir}}（{{files}} 个文件）', {
            dir: res.data.dir,
            files: res.data.files,
          }),
          'success',
        );
        await refreshSkills();
      } catch (err) {
        addToast(err instanceof Error ? err.message : t('导入技能包失败'), 'error');
      } finally {
        setSkillPkgImporting(false);
      }
    },
    [addToast, refreshSkills, skillPkgName, skillPkgOverwrite, t],
  );

  /** 预设 → 结构化模板草稿（同名 upsert；不直接 PUT，用户手动保存） */
  const applyPresetTemplate = useCallback((name: string, text: string): void => {
    setDirty(true);
    setDraft((d) => {
      if (!d) return d;
      const templates = [...(d.structured_templates ?? [])];
      const index = templates.findIndex((tpl) => tpl.name === name);
      if (index >= 0) templates[index] = { name, text };
      else templates.push({ name, text });
      return { ...d, structured_templates: templates };
    });
  }, []);

  const addTemplate = useCallback((): void => {
    setDirty(true);
    setDraft((d) =>
      d ? { ...d, structured_templates: [...(d.structured_templates ?? []), { name: '', text: '' }] } : d,
    );
  }, []);

  const removeTemplate = useCallback((index: number): void => {
    setDirty(true);
    setDraft((d) => {
      if (!d) return d;
      const templates = (d.structured_templates ?? []).filter((_, i) => i !== index);
      // 删除的正是当前启用模板 → 清空选择（回退 structured_suffix），避免悬空引用
      const removed = (d.structured_templates ?? [])[index];
      const active = d.active_template && removed && d.active_template === removed.name ? '' : d.active_template;
      return { ...d, structured_templates: templates, active_template: active ?? '' };
    });
  }, []);

  /** 导入模板 JSON：同名 upsert（已有同名被覆盖计数 skipped-over），新名追加 */
  const importTemplateJson = useCallback(
    async (file: File | null): Promise<void> => {
      if (!file) return;
      const raw = await file.text().catch(() => null);
      if (raw === null) {
        addToast(t('导入失败：无法读取文件'), 'error');
        return;
      }
      const parsed = parseTemplateJson(raw);
      if ('error' in parsed) {
        addToast(
          parsed.error === 'not-json'
            ? t('导入失败：不是有效的 JSON')
            : t('导入失败：没有可用的模板（需 {name, text} 字段）'),
          'error',
        );
        return;
      }
      setDirty(true);
      setDraft((d) => {
        if (!d) return d;
        const existing = [...(d.structured_templates ?? [])];
        const keys = new Set(existing.map((tpl) => tpl.name.trim().toLowerCase()));
        let replaced = 0;
        let appended = 0;
        for (const tpl of parsed.templates) {
          const key = tpl.name.toLowerCase();
          const idx = existing.findIndex((e) => e.name.trim().toLowerCase() === key);
          if (idx >= 0) {
            existing[idx] = tpl;
            replaced += 1;
          } else {
            existing.push(tpl);
            keys.add(key);
            appended += 1;
          }
        }
        addToast(
          t('模板已导入：新增 {{added}} / 覆盖同名 {{replaced}}（保存后生效）', {
            added: appended,
            replaced,
          }),
          'success',
        );
        return { ...d, structured_templates: existing };
      });
      focusFixTarget('templates');
    },
    [addToast, t],
  );

  /**
   * 改写预览：以当前草稿为配置试跑样例输入（dry-run，不写统计、不落库）。
   *
   * 输入支持角色前缀行（`system:`/`user:`/`assistant:` 开头开新消息，
   * 见 `parsePreviewMessages`）；裸文本仍视作单条 user。
   * 多消息时输出整段对话（每条前带 `[role]` 标记），单条时保持原文
   * 不加标记（老习惯：复制粘贴直接可用）。
   */
  const runPreview = useCallback(async (): Promise<void> => {
    const current = draftRef.current;
    if (!current || previewing) return;
    // 「按已保存配置预览」：草稿有未保存编辑时，用它区分「线上现在会怎样」
    // 与「保存后会怎样」——两者输出不同正是排障要看的对比。
    const saved = dataRef.current?.settings;
    const usingSaved = previewSavedOnly && saved;
    const settings = usingSaved ? saved : current;
    const parsed = parsePreviewMessages(previewInput);
    if (parsed.length === 0) {
      addToast(t('请输入预览样例'), 'warning');
      return;
    }
    setPreviewing(true);
    try {
      const res = await api.previewPurge({
        settings,
        messages: parsed,
      });
      if (!res.success || !res.data) throw new Error('preview failed');
      const back = res.data.messages;
      const fmt = (msgs: Array<{ role: string; content: string | null }>): string =>
        msgs.length > 1
          ? msgs
              .map((m) => `[${m.role}]\n${m.content ?? ''}`)
              .join('\n\n')
          : (msgs[0]?.content ?? '');
      // diff 基线：输入原样（角色标记与输出同构）；辅助消息（消息数变化时
      // 逐条配对不可靠）退化为整红整绿。
      setPreviewBase(fmt(parsed.map((m) => ({ role: m.role, content: m.content }))));
      setPreviewOut(fmt(back));
      setPreviewMeta(res.data.applied);
      setPreviewDetail(res.data.detail ?? null);
      // 试跑成功才入历史（失败/校验不过的输入不记）；超长样例不入库
      if (previewInput.length <= PREVIEW_HISTORY_ITEM_MAX) {
        setPreviewHistory((prev) =>
          writePreviewHistory(prev, previewInput, (k, v) => window.localStorage.setItem(k, v)),
        );
      }
    } catch {
      addToast(t('预览失败'), 'error');
    } finally {
      setPreviewing(false);
    }
  }, [addToast, previewInput, previewing, previewSavedOnly, t]);

  // ── 词表 CSV 导入导出（死区词/同义词单表，进草稿不直接保存）──────────────
  //
  // 两列 `from,to`（表头可选）；RFC 4180 转义复用 toCsv / downloadText。

  const exportRulesCsv = useCallback(
    (kind: 'dead_zone' | 'synonym'): void => {
      const draftNow = draftRef.current;
      if (!draftNow) return;
      const rules = kind === 'dead_zone' ? draftNow.rewrite_rules : draftNow.synonym_pairs;
      if (rules.length === 0) {
        addToast(t('词表为空，无可导出内容'), 'warning');
        return;
      }
      const filename = `aigx-purge-${kind}-${new Date().toISOString().slice(0, 10)}.csv`;
      downloadText(filename, toCsv(rules.map((r) => ({ from: r.from, to: r.to }))) + '\n');
      addToast(t('词表已导出为 CSV'), 'success');
    },
    [addToast, t],
  );

  /**
   * 导入 CSV → 追加进草稿（与「添加规则」同口径：不整表替换，重复项靠行内提示）。
   * 导入前对现有草稿做 from 冲突预检：全部重复 → 警告口径（可能导错文件/表）；
   * 部分重复 → 成功口径附重复数；行内 `——dupHint` 仍逐行标注。
   */
  const importRulesCsv = useCallback(
    async (kind: 'dead_zone' | 'synonym', file: File | null): Promise<void> => {
      if (!file) return;
      try {
        const parsed = parseRulesCsv(await file.text());
        if (parsed === null) {
          addToast(t('导入失败：必须是两列（原文,改写为）的 CSV'), 'error');
          return;
        }
        if (parsed.length === 0) {
          addToast(t('导入失败：文件里没有规则行'), 'error');
          return;
        }
        const existing = draftRef.current;
        const existingFroms = new Set(
          (kind === 'dead_zone' ? existing?.rewrite_rules ?? [] : existing?.synonym_pairs ?? [])
            .map((r) => r.from.trim().toLowerCase())
            .filter(Boolean),
        );
        const incomingFroms = parsed.map((r) => r.from.trim().toLowerCase());
        const dupCount = incomingFroms.filter((f) => existingFroms.has(f)).length;
        if (dupCount === parsed.length) {
          // 一条都不新：大概率导错了表或旧文件——直接不追加，给出口
          addToast(t('导入的 {{count}} 条规则的原文全部与当前词表重复，未追加（可能导错了文件？）', { count: parsed.length }), 'warning');
          return;
        }
        setDirty(true);
        setDraft((d) =>
          d
            ? {
                ...d,
                rewrite_rules:
                  kind === 'dead_zone' ? [...d.rewrite_rules, ...parsed] : d.rewrite_rules,
                synonym_pairs:
                  kind === 'synonym' ? [...d.synonym_pairs, ...parsed] : d.synonym_pairs,
              }
            : d,
        );
        const base =
          kind === 'dead_zone'
            ? t('已追加 {{count}} 条死区词到草稿，检查无误后点「保存」生效', { count: parsed.length })
            : t('已追加 {{count}} 条同义词到草稿，检查无误后点「保存」生效', { count: parsed.length });
        // 导入后总数超过后端保留上限 → 明确告知会被截断（剩余行白填）
        const mergedLen =
          (kind === 'dead_zone' ? existing?.rewrite_rules.length ?? 0 : existing?.synonym_pairs.length ?? 0) +
          parsed.length;
        const capWarn =
          mergedLen > RULE_TABLE_CAP
            ? ` · ${t('导入后共 {{n}} 条，超过上限 {{cap}} 条：保存只保留前 {{cap}} 条', {
                n: mergedLen,
                cap: RULE_TABLE_CAP,
              })}`
            : '';
        addToast(
          dupCount > 0 || capWarn
            ? `${base}${dupCount > 0 ? ` · ${t('其中 {{n}} 条与现有词表原文重复（保存时后端只保留最早一条）', { n: dupCount })}` : ''}${capWarn}`
            : base,
          dupCount > 0 || capWarn ? 'warning' : 'success',
        );
      } catch {
        addToast(t('导入失败：无法读取文件'), 'error');
      }
    },
    [addToast, t],
  );

  /** 导出配置：当前服务端快照 → JSON 文件（含全部词表/模板/开关） */
  const exportConfig = useCallback((): void => {
    const snapshot = dataRef.current;
    if (!snapshot) return;
    const filename = `aigx-purge-config-${new Date().toISOString().slice(0, 10)}.json`;
    downloadText(filename, JSON.stringify({ version: 1, settings: snapshot.settings }, null, 2) + '\n');
    addToast(t('配置已导出'), 'success');
  }, [addToast, t]);

  /** 导入配置：读取 JSON → 校验结构 → 填入草稿（不直接保存，确认后再点保存） */
  const importConfig = useCallback(async (file: File | null): Promise<void> => {
    if (!file) return;
    try {
      const text = await file.text();
      const parsed = JSON.parse(text) as { version?: number; settings?: PurgeSettings };
      const settings = parsed?.settings;
      if (!settings || typeof settings !== 'object' || !Array.isArray(settings.rewrite_rules)) {
        throw new Error('invalid shape');
      }
      setDraft(settings);
      setNumRaw(numRawOf(settings));
      setNumErrors({});
      setDirty(true);
      setPreviewOut(null);
      setPreviewBase(null);
      setPreviewMeta(null);
      setPreviewDetail(null);
      addToast(t('配置已导入草稿，检查无误后点「保存」生效'), 'success');
    } catch {
      addToast(t('导入失败：不是有效的破甲配置文件'), 'error');
    }
  }, [addToast, t]);

  // 词表增删后把焦点落回列表内，避免按钮随行卸载后焦点掉到 body
  useEffect(() => {
    if (focusRule.current === null || !draft) return;
    const target = focusRule.current;
    focusRule.current = null;
    const rows = rulesRef.current?.querySelectorAll<HTMLElement>('.purge-rule-row');
    if (!rows || rows.length === 0) return;
    const idx = target === 'add' ? rows.length - 1 : Math.min(target, rows.length - 1);
    rows[idx]?.querySelector<HTMLInputElement>('input')?.focus();
  }, [draft]);

  // 同义词词表增删后焦点回归（与死区词表独立）
  useEffect(() => {
    if (focusSynRule.current === null || !draft) return;
    const target = focusSynRule.current;
    focusSynRule.current = null;
    const rows = synRulesRef.current?.querySelectorAll<HTMLElement>('.purge-rule-row');
    if (!rows || rows.length === 0) return;
    const idx = target === 'add' ? rows.length - 1 : Math.min(target, rows.length - 1);
    rows[idx]?.querySelector<HTMLInputElement>('input')?.focus();
  }, [draft]);

  /** 数字输入：字符串镜像实时更新；能解析出有限数才提交进 draft，杜绝 NaN/空值进配置 */
  const handleNumChange = useCallback((key: NumKey, raw: string): void => {
    setDirty(true);
    setNumRaw((prev) => ({ ...prev, [key]: raw }));
    const n = Number(raw);
    if (raw.trim() !== '' && Number.isFinite(n)) {
      setNumErrors((prev) => (prev[key] ? { ...prev, [key]: undefined } : prev));
      const truncated = Math.trunc(n);
      setDraft((d) => (d ? withNum(d, key, truncated) : d));
    }
  }, []);

  // 开关行入参：按 8 个原语派生，数组引用只在开关本身变化时改变——
  // 文本/词表编辑不会让 SwitchRows 命中重渲染（memo 才有意义）。
  const flags = useMemo(
    () =>
      SWITCHES.map((item) => Boolean(draft?.[item.key])),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 逐键列依赖：整体 draft 每次编辑都换引用
    [
      draft?.enabled,
      draft?.system_normalize,
      draft?.dead_zone_rewrite,
      draft?.synonym_enabled,
      draft?.reduce_reasoning,
      draft?.cross_source_fallback,
      draft?.skill_inject,
      draft?.structured_prompt,
    ],
  );

  if (loading) {
    return (
      <div className="purge-console">
        <div className="page-header">
          <h1>{t('破甲控制台')}</h1>
        </div>
        <SkeletonCards count={7} />
        <Card title={t('开关')}>
          <SkeletonList rows={8} />
        </Card>
        <Card title={t('只读参数')}>
          <div className="purge-skeleton-params" role="status" aria-busy="true">
            {Array.from({ length: 8 }, (_, i) => (
              <Skeleton key={i} height={12} />
            ))}
          </div>
        </Card>
      </div>
    );
  }

  if (!data || !draft) {
    return (
      <div className="purge-console">
        <div className="page-header">
          <h1>{t('破甲控制台')}</h1>
        </div>
        <Card>
          <p className="purge-empty">{t('网络异常或服务不可用，请稍后重试')}</p>
          <Button variant="primary" onClick={() => void load()}>
            {t('重试')}
          </Button>
        </Card>
      </div>
    );
  }

  // 旧后端无 stats_1h/24h → 窗口选项禁用（只有全程可看）。
  const windowed =
    statsWindow === '1h'
      ? data.stats_1h
      : statsWindow === '24h'
        ? data.stats_24h
        : statsWindow === 'last'
          ? (data.stats_last_run ?? undefined)
          : undefined;
  const stats = windowed ?? data.stats;
  const retryRate = stats.responses > 0 ? ((stats.retries / stats.responses) * 100).toFixed(2) : '0.00';
  const windowLabel =
    statsWindow === '1h'
      ? t('近 1 小时')
      : statsWindow === '24h'
        ? t('近 24 小时')
        : statsWindow === 'last'
          ? t('上次运行')
          : undefined;
  const uptime = formatUptime(data.runtime.uptime_secs, {
    sec: t('秒'),
    min: t('分钟'),
    hour: t('小时'),
    day: t('天'),
  });

  return (
    <div className="purge-console" aria-busy={saving || undefined}>
      <div className="page-header">
        <h1>{t('破甲控制台')}</h1>
        <div className="purge-meta">
          PID {data.runtime.pid} · {uptime} · {t('上游')}{' '}
          {data.runtime.upstream || '—'}{' '}
          {/* 刷新统计：load 会重置草稿，有未保存编辑时禁用避免吞掉改动 */}
          <Button
            variant="outline"
            size="sm"
            disabled={saving || dirty || loading}
            title={dirty ? t('有未保存的修改，保存或重新加载后才能刷新') : undefined}
            onClick={() => void load()}
          >
            {t('刷新')}
          </Button>
        </div>
        {/* 总开关关闭提示：放在页面标题下方独立成行（请求不会被改写这件事必须一眼看到，
            只靠开关行的颜色区分太弱） */}
        {!draft.enabled ? (
          <p className="purge-off-notice" role="status">
            {t('破甲控制台当前关闭，请求不会被改写。')}
          </p>
        ) : null}
      </div>

      {/* 未保存编辑警示条：离开保护之外的页内显性提示，提供「放弃修改」出口 */}
      {dirty && (
        <div className="purge-dirty-bar" role="status">
          <span>{t('有未保存的修改')}</span>
          <Button
            variant="outline"
            size="sm"
            disabled={saving}
            onClick={() => {
              const snapshot = dataRef.current;
              if (!snapshot) return;
              setDraft(snapshot.settings);
              setNumRaw(numRawOf(snapshot.settings));
              setNumErrors({});
              setDirty(false);
              setPreviewOut(null);
              setPreviewBase(null);
              setPreviewMeta(null);
              setPreviewDetail(null);
            }}
          >
            {t('放弃修改')}
          </Button>
        </div>
      )}

      {/* 统计口径切换（旧后端无窗口数据 → 窗口档禁用并提示） */}
      <div className="purge-window-picker" role="group" aria-label={t('统计时间窗')}>
        {([
          { key: 'all', label: t('全程'), unavailable: false },
          { key: '1h', label: t('近 1 小时'), unavailable: !data.stats_1h },
          { key: '24h', label: t('近 24 小时'), unavailable: !data.stats_24h },
          { key: 'last', label: t('上次运行'), unavailable: !data.stats_last_run },
        ] as const).map((opt) => (
          <button
            key={opt.key}
            type="button"
            className={`purge-preset-chip${statsWindow === opt.key ? ' is-active' : ''}`}
            disabled={opt.unavailable}
            aria-pressed={statsWindow === opt.key}
            onClick={() => setStatsWindow(opt.key)}
          >
            {opt.label}
          </button>
        ))}
      </div>

      <StatsGrid
        stats={stats}
        retryRate={retryRate}
        suffix={windowLabel}
        // 只有窗口档（事件环）会带 truncated；「上次运行」是精确快照
        atLeast={
          statsWindow === '1h' || statsWindow === '24h'
            ? (windowed as PurgeWindowedStats | undefined)?.truncated
            : undefined
        }
        t={t}
      />

      <FailureBar
        refusal={stats.failure_refusal}
        empty={stats.failure_empty}
        filter={stats.failure_filter}
        other={stats.failure_other}
        t={t}
      />

      {/* 配置体检告警：开了但不生效 / 配置自相矛盾（静态可判定项）。
          结构化告警带 code → 「去修复」滚动并聚焦对应控件；旧后端纯文本。
          全部通过时给一行绿字：让运维确认「体检确实跑过且当前无问题」，
          而不是面对一片空白猜后端到底检查了什么。 */}
      {configWarningItems.length > 0 || configWarnings.length > 0 ? (
        <div className="purge-warning-box" role="alert">
          {(configWarningItems.length > 0
            ? configWarningItems.map((w) => ({ code: w.code, message: w.message }))
            : configWarnings.map((message) => ({ code: '', message }))
          ).map(({ code, message }, index) => (
            <p key={index} className="purge-warning-row">
              <span>{message}</span>
              {code && FIX_TARGETS[code] ? (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => focusFixTarget(FIX_TARGETS[code])}
                >
                  {t('去修复')}
                </Button>
              ) : null}
            </p>
          ))}
        </div>
      ) : draft.enabled ? (
        <p className="purge-check-ok" role="status">
          {t('配置体检通过：当前没有「开了但不生效」或自相矛盾的项目。')}
        </p>
      ) : null}

      <Card
        title={t('开关')}
        actions={<span className="purge-hint">{t('点一下立即生效，重启后保留')}</span>}
      >
        <SwitchRows flags={flags} disabled={saving} onToggle={toggle} t={t} />
      </Card>

      <Card
        title={t('只读参数')}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={exportConfig}>
              {t('导出配置')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => importInputRef.current?.click()}
            >
              {t('导入配置')}
            </Button>
            <input
              ref={importInputRef}
              type="file"
              accept=".json,application/json"
              style={{ display: 'none' }}
              onChange={(event) => {
                void importConfig(event.target.files?.[0] ?? null);
                // 允许连续导入同名文件
                event.target.value = '';
              }}
            />
          </>
        }
      >
        {/* 只读区一律读服务端快照（data.settings），不读草稿——未保存的编辑
            不应提前出现在「只读」区（与失败分类、在途数等运行时字段同口径）。 */}
        <ParamGrid
          retryLimit={data.settings.retry_limit}
          failureRefusal={stats.failure_refusal}
          failureEmpty={stats.failure_empty}
          failureFilter={stats.failure_filter}
          failureOther={stats.failure_other}
          fallbackModels={data.settings.fallback_models.join(', ')}
          fallbackTriggerAttempt={data.settings.fallback_trigger_attempt}
          skillInjectLimit={data.settings.skill_inject_limit}
          inflight={data.runtime.inflight}
          upstream={data.runtime.upstream}
          t={t}
        />
      </Card>

      <Card
        title={t('死区词改写词表')}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={addRule}>
              {t('添加规则')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => exportRulesCsv('dead_zone')}>
              {t('导出 CSV')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => deadZoneCsvRef.current?.click()}>
              {t('导入 CSV')}
            </Button>
            <input
              ref={deadZoneCsvRef}
              type="file"
              accept=".csv,text/csv"
              style={{ display: 'none' }}
              onChange={(event) => {
                void importRulesCsv('dead_zone', event.target.files?.[0] ?? null);
                event.target.value = '';
              }}
            />
          </>
        }
      >
        {draft.rewrite_rules.length > RULE_TABLE_CAP ? (
          <p className="purge-cap-hint" role="alert" data-testid="purge-rules-over-cap">
            {t(
              '当前 {{n}} 条，超过上限 {{cap}} 条：保存时只保留前 {{cap}} 条，其余会被丢弃。',
              { n: draft.rewrite_rules.length, cap: RULE_TABLE_CAP },
            )}
          </p>
        ) : null}
        {draft.rewrite_rules.length === 0 ? (
          <p className="purge-empty" data-purge-fix="dead-zone-table">{t('暂无改写规则，点击右上角「添加规则」新建一条')}</p>
        ) : (
          <div className="purge-rules" ref={rulesRef} data-purge-fix="dead-zone-table">
            {draft.rewrite_rules.map((rule, index) => (
              <RuleRow
                key={index}
                rule={rule}
                index={index}
                fromLabel={t('原文')}
                toLabel={t('改写为')}
                removeLabel={t('删除')}
                enableLabel={t('启用该条改写规则')}
                dupHint={
                  dupRules.get(rule.from.trim().toLowerCase())
                    ? t('与其他行的原文重复：改写按表序执行，只有最靠前的那条会真正生效')
                    : undefined
                }
                onPatch={patchRule}
                onRemove={removeRule}
              />
            ))}
          </div>
        )}
      </Card>

      <Card
        title={t('同义词表')}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={addSynonymRule}>
              {t('添加规则')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => exportRulesCsv('synonym')}>
              {t('导出 CSV')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => synonymCsvRef.current?.click()}>
              {t('导入 CSV')}
            </Button>
            <input
              ref={synonymCsvRef}
              type="file"
              accept=".csv,text/csv"
              style={{ display: 'none' }}
              onChange={(event) => {
                void importRulesCsv('synonym', event.target.files?.[0] ?? null);
                event.target.value = '';
              }}
            />
          </>
        }
      >
        {draft.synonym_pairs.length > RULE_TABLE_CAP ? (
          <p className="purge-cap-hint" role="alert" data-testid="purge-synonyms-over-cap">
            {t(
              '当前 {{n}} 条，超过上限 {{cap}} 条：保存时只保留前 {{cap}} 条，其余会被丢弃。',
              { n: draft.synonym_pairs.length, cap: RULE_TABLE_CAP },
            )}
          </p>
        ) : null}
        {draft.synonym_pairs.length === 0 ? (
          <p className="purge-empty">{t('暂无同义词，点击右上角「添加规则」新建一条')}</p>
        ) : (
          <div className="purge-rules" ref={synRulesRef} data-purge-fix="synonym-table">
            {draft.synonym_pairs.map((rule, index) => (
              <RuleRow
                key={index}
                rule={rule}
                index={index}
                fromLabel={t('敏感词')}
                toLabel={t('同义词')}
                removeLabel={t('删除')}
                enableLabel={t('启用该条改写规则')}
                dupHint={
                  dupSynonyms.get(rule.from.trim().toLowerCase())
                    ? t('与其他行的敏感词重复：改写按表序执行，只有最靠前的那条会真正生效')
                    : undefined
                }
                onPatch={patchSynonymRule}
                onRemove={removeSynonymRule}
              />
            ))}
          </div>
        )}
      </Card>

      <Card title={t('技能与结构化提示')}>
        <div className="purge-form-grid">
          <Input
            label={t('技能目录')}
            hint={t('扫描该目录下的 SKILL.md，按 frontmatter 关键词命中')}
            value={draft.skill_dir}
            onChange={(event) => patch({ skill_dir: event.target.value })}
            data-purge-fix="skill-dir"
          />
          <Input
            label={t('固定套用预设 id')}
            hint={t('留空 = 按模型自动匹配；名字对不上就不套，预设正文原样前插到 system 提示词')}
            value={draft.auto_preset_id ?? ''}
            onChange={(event) => patch({ auto_preset_id: event.target.value })}
            data-purge-fix="auto-preset-id"
          />
          <Input
            label={t('技能注入上限（字符）')}
            type="number"
            min={16}
            max={20000}
            value={numRaw.skill_inject_limit}
            error={numErrors.skill_inject_limit}
            onChange={(event) => handleNumChange('skill_inject_limit', event.target.value)}
          />
          <Input
            label={t('异源模型（逗号分隔）')}
            value={draft.fallback_models.join(', ')}
            onChange={(event) => patch({ fallback_models: splitList(event.target.value) })}
            // 后端 normalize take(16)：超出的模型保存时被丢弃，必须当场提示
            error={
              draft.fallback_models.length > FALLBACK_MODELS_CAP
                ? t('已填 {{n}} 个，上限 {{cap}} 个：保存只保留前 {{cap}} 个', {
                    n: draft.fallback_models.length,
                    cap: FALLBACK_MODELS_CAP,
                  })
                : undefined
            }
            data-purge-fix="fallback-models"
          />
          <Input
            label={t('异源触发轮次')}
            type="number"
            min={1}
            max={32}
            value={numRaw.fallback_trigger_attempt}
            error={numErrors.fallback_trigger_attempt}
            onChange={(event) => handleNumChange('fallback_trigger_attempt', event.target.value)}
            data-purge-fix="fallback-attempts"
          />
          <Input
            label={t('重试上限')}
            type="number"
            min={1}
            max={32}
            value={numRaw.retry_limit}
            error={numErrors.retry_limit}
            onChange={(event) => handleNumChange('retry_limit', event.target.value)}
          />
          <Input
            label={t('system 剥离前缀（逗号分隔）')}
            value={draft.normalize_strip_prefixes.join(', ')}
            onChange={(event) => patch({ normalize_strip_prefixes: splitList(event.target.value) })}
            error={capError(draft.normalize_strip_prefixes, LIST_CAP)}
          />
          <Select
            label={t('降档目标（reasoning_floor）')}
            hint={t('降思考开启后，把 high/medium/xhigh/max 压到该档位')}
            value={draft.reasoning_floor}
            onChange={(event) => patch({ reasoning_floor: event.target.value })}
          >
            {REASONING_FLOORS.map((item) => (
              <option key={item.value} value={item.value}>
                {t(item.label)}
              </option>
            ))}
          </Select>
        </div>
        <Textarea
          label={t('拒绝话术词表（每行一条，空表 = 关闭字面拒绝检测）')}
          rows={4}
          value={(draft.refusal_patterns ?? []).join('\n')}
          onChange={(event) => patch({ refusal_patterns: splitLines(event.target.value) })}
          error={capError(draft.refusal_patterns, LIST_CAP)}
        />
        <div className="purge-form-grid">
          <Textarea
            label={t('「内容过滤」类失败关键词（每行一条）')}
            rows={3}
            value={(draft.failure_filter_keywords ?? []).join('\n')}
            onChange={(event) => patch({ failure_filter_keywords: splitLines(event.target.value) })}
            error={capError(draft.failure_filter_keywords, LIST_CAP)}
          />
          <Textarea
            label={t('「空答」类失败关键词（每行一条）')}
            rows={3}
            value={(draft.failure_empty_keywords ?? []).join('\n')}
            onChange={(event) => patch({ failure_empty_keywords: splitLines(event.target.value) })}
            error={capError(draft.failure_empty_keywords, LIST_CAP)}
          />
        </div>
        <Textarea
          label={t('结构化输出后缀')}
          rows={4}
          value={draft.structured_suffix}
          onChange={(event) => patch({ structured_suffix: event.target.value })}
        />
      </Card>

      {/* 技能包导入：ZIP/TAR/TGZ 安全解压到技能目录（包内须含 SKILL.md） */}
      <Card title={t('导入技能包')}>
        <p style={{ margin: '0 0 10px', fontSize: 13 }}>
          {t(
            '上传 ZIP / TAR / TGZ 技能包，安全解压到上面的「技能目录」。包内须含 SKILL.md；导入后会立即出现在技能快照。',
          )}
        </p>
        <div style={{ display: 'flex', gap: 12, alignItems: 'flex-end', flexWrap: 'wrap' }}>
          <div style={{ flex: '1 1 220px', minWidth: 220 }}>
            <Input
              label={t('目录名（可选）')}
              hint={t('留空则按文件名推导，仅允许字母/数字/连字符')}
              value={skillPkgName}
              placeholder={t('例如 my-skill-pack')}
              onChange={(event) => setSkillPkgName(event.target.value)}
            />
          </div>
          <label style={{ display: 'flex', alignItems: 'center', gap: 6, whiteSpace: 'nowrap' }}>
            <input
              type="checkbox"
              checked={skillPkgOverwrite}
              onChange={(event) => setSkillPkgOverwrite(event.target.checked)}
            />
            {t('覆盖同名目录')}
          </label>
          <Button disabled={skillPkgImporting} onClick={() => skillPkgInputRef.current?.click()}>
            {skillPkgImporting ? t('导入中…') : t('选择文件并导入')}
          </Button>
          <input
            ref={skillPkgInputRef}
            type="file"
            accept=".zip,.tar,.tgz,.tar.gz"
            hidden
            onChange={(event) => void handleImportSkill(event)}
          />
        </div>
      </Card>

      {/* 技能扫描快照：让 skill_inject 配置可验证（目录里到底扫到了什么） */}
      {skills.length > 0 && (
        <Card title={t('技能扫描快照')}>
          <ul className="purge-skill-list" data-purge-fix="skill-snapshot">
            {skills.map((skill) => (
              <li key={skill.name}>
                <strong>{skill.name}</strong>
                <span className="purge-skill-meta">
                  {t('{{count}} 个关键词 · 正文 {{chars}} 字符', {
                    count: skill.keywords.length,
                    chars: skill.body_chars,
                  })}
                </span>
                <span className="purge-skill-keywords">{skill.keywords.slice(0, 6).join(' / ')}</span>
              </li>
            ))}
          </ul>
        </Card>
      )}

      {/* 预设库：内置 prompt 集合浏览 + 一键落盘（放在结构化模板之上，应用后直接在下方看到结果） */}
      <PresetsSection
        t={t}
        addToast={addToast}
        onApplyTemplate={applyPresetTemplate}
        currentTemplates={draft?.structured_templates}
        onSkillWritten={() => void refreshSkills()}
      />

      <Card
        title={t('结构化模板')}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={addTemplate}>
              {t('添加模板')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => templateJsonRef.current?.click()}>
              {t('导入 JSON')}
            </Button>
            <input
              ref={templateJsonRef}
              type="file"
              accept=".json,application/json"
              style={{ display: 'none' }}
              onChange={(event) => {
                void importTemplateJson(event.target.files?.[0] ?? null);
                event.target.value = '';
              }}
            />
          </>
        }
      >
        {capError(draft.structured_templates, TEMPLATES_CAP) ? (
          <p className="purge-cap-hint" role="alert">
            {capError(draft.structured_templates, TEMPLATES_CAP)}
          </p>
        ) : null}
        <Select
          label={t('当前启用模板')}
          hint={t('结构化提示开启时，命中模板则用其文本替代默认后缀；选「使用默认后缀」即回退')}
          value={draft.active_template ?? ''}
          onChange={(event) => patch({ active_template: event.target.value })}
        >
          <option value="">{t('（使用默认后缀）')}</option>
          {(draft.structured_templates ?? []).map((tpl, index) => (
            <option key={`tpl-${index}`} value={tpl.name} disabled={!tpl.name.trim()}>
              {tpl.name.trim() || t('（未命名）')}
            </option>
          ))}
        </Select>
        {(draft.structured_templates ?? []).length === 0 ? (
          <p className="purge-empty" data-purge-fix="templates">{t('暂无模板，点击右上角「添加模板」新建一条')}</p>
        ) : (
          <div className="purge-rules" data-purge-fix="templates">
            {(draft.structured_templates ?? []).map((tpl, index) => (
              <div className="purge-rule-row" key={`tpl-${index}`}>
                <div>
                  <Input
                    placeholder={t('模板名称')}
                    aria-label={t('模板名称')}
                    value={tpl.name}
                    error={
                      dupTemplates.get(tpl.name.trim().toLowerCase())
                        ? t('模板名称重复，保存前请先修改')
                        : undefined
                    }
                    onChange={(event) => patchTemplate(index, { name: event.target.value })}
                  />
                </div>
                <Textarea
                  rows={3}
                  placeholder={t('模板文本')}
                  aria-label={t('模板文本')}
                  value={tpl.text}
                  onChange={(event) => patchTemplate(index, { text: event.target.value })}
                />
                <Button variant="outline" size="sm" onClick={() => removeTemplate(index)}>
                  {t('删除')}
                </Button>
              </div>
            ))}
          </div>
        )}
      </Card>

      <Card title={t('改写预览')}>
        {/* 样例历史：点回填；再次点击同一条 = 无操作（已是输入内容） */}
        {previewHistory.length > 0 && (
          <div className="purge-window-picker" data-testid="purge-preview-history" role="group" aria-label={t('最近样例')}>
            {previewHistory.map((sample, index) => (
              <button
                key={`${index}-${sample.slice(0, 12)}`}
                type="button"
                className={`purge-preset-chip${sample === previewInput ? ' is-active' : ''}`}
                title={sample.slice(0, 200)}
                onClick={() => setPreviewInput(sample)}
              >
                {previewHistoryLabel(sample)}
              </button>
            ))}
          </div>
        )}
        <Textarea
          label={t('样例输入')}
          rows={3}
          placeholder={t('粘贴 user 消息试跑；或用 system:/user:/assistant: 行前缀粘贴整段对话（多消息参与归一化与改写）。dry-run，不发送请求、不写统计')}
          value={previewInput}
          onChange={(event) => setPreviewInput(event.target.value)}
        />
        <div className="purge-actions">
          {/* 预览用草稿里的未保存配置：保存前即可验证词表/模板/技能注入效果 */}
          <Button variant="outline" disabled={previewing || saving} onClick={() => void runPreview()}>
            {previewing ? t('预览中…') : t('预览改写')}
          </Button>
          <label className="purge-rule-toggle" title={t('用已保存的配置试跑（对比未保存草稿的效果）')}>
            <input
              type="checkbox"
              checked={previewSavedOnly}
              disabled={previewing}
              onChange={(event) => setPreviewSavedOnly(event.target.checked)}
            />
            <span className="purge-hint">{t('按已保存配置')}</span>
          </label>
        </div>
        {previewOut !== null ? (
          <>
            {previewMeta ? (
              <div className="purge-preview-meta">
                {previewMeta.changed ? t('已应用改写') : t('无改写命中（原样返回）')}
                {previewMeta.skill_hit ? ` · ${t('技能已注入')}` : ''}
              </div>
            ) : null}
            {/* 逐项命中明细（旧后端无 detail 字段 → 整块不渲染） */}
            {previewDetail ? (
              <div className="purge-preview-detail" data-testid="purge-preview-detail">
                {[
                  previewDetail.normalized ? `${t('归一化')} ${previewDetail.normalized}` : '',
                  previewDetail.dead_zone_rules
                    ? `${t('死区词')} ${previewDetail.dead_zone_rules}/${previewDetail.dead_zone_replacements}`
                    : '',
                  previewDetail.synonym_rules
                    ? `${t('同义词')} ${previewDetail.synonym_rules}/${previewDetail.synonym_replacements}`
                    : '',
                  previewDetail.structured_appended ? t('结构化提示已追加') : '',
                  previewDetail.skill_names && previewDetail.skill_names.length > 0
                    ? (() => {
                        // 技能名 + 命中关键词（skill_keywords 与 names 同序；旧后端无则只报名）
                        const kw = new Map(previewDetail.skill_keywords ?? []);
                        const names = previewDetail.skill_names
                          .map((n) => (kw.has(n) ? `${n}（${t('命中关键词')} "${kw.get(n)}"）` : n))
                          .join(', ');
                        return `${t('命中技能')}：${names}${previewDetail.skill_truncated ? ` (${t('已截断')})` : ''}${previewDetail.skill_dropped ? ` · ${t('{{n}} 个命中技能因长度上限未注入', { n: previewDetail.skill_dropped })}` : ''}`;
                      })()
                    : '',
                ]
                  .filter(Boolean)
                  .map((item) => (
                    <span key={item} className="purge-preview-detail-chip">{item}</span>
                  ))}
              </div>
            ) : null}
            {/* 改写前后有差异 → 行级/词级 diff 染色；无差异（或无基线）→ 原文 */}
            {previewBase !== null && previewBase !== previewOut ? (
              <div className="purge-preview-diff" data-testid="purge-preview-diff" aria-label={t('改写结果')}>
                {diffLines(previewBase, previewOut).map((row, index) => {
                  if (row.kind === 'ctx') {
                    return <div key={index} className="pdl-ctx">{row.text}</div>;
                  }
                  if (row.kind === 'del') {
                    return <div key={index} className="pdl-del">{'- '}{row.text}</div>;
                  }
                  if (row.kind === 'add') {
                    return <div key={index} className="pdl-add">{'+ '}{row.text}</div>;
                  }
                  const w = diffTokensInLine(row.del, row.add);
                  return (
                    <div key={index} className="pdl-mod">
                      <span className="pdl-w-keep">{w.keep1}</span>
                      {w.del ? <span className="pdl-w-del">{w.del}</span> : null}
                      <span className="pdl-w-keep">{w.keep2}</span>
                      {w.add ? <span className="pdl-w-add">{w.add}</span> : null}
                      <span className="pdl-w-keep">{w.keep3}</span>
                    </div>
                  );
                })}
              </div>
            ) : (
              <pre className="purge-preview-out" aria-label={t('改写结果')}>
                {previewOut}
              </pre>
            )}
          </>
        ) : null}
      </Card>

      <div className="purge-actions">
        {/* 保存态播报：按钮文案变化对读屏不可见，用 live region 显式播报 */}
        <span className="sr-only" role="status" aria-live="polite">
          {saving ? t('保存中…') : ''}
        </span>
        <Button
          variant="primary"
          disabled={saving || !dirty}
          title={!dirty && !saving ? t('没有需要保存的修改') : undefined}
          onClick={() => void save()}
        >
          {saving ? t('保存中…') : t('保存')}
        </Button>
      </div>
    </div>
  );
}
