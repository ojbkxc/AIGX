import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api } from '../api';
import { API_BASE, authHeaders } from '../api/client';
import type {
  PurgeAppliedDetail,
  PurgeConfigWarning,
  PurgeData,
  PurgePresetBrief,
  PurgePresetDetail,
  PurgePresetList,
  PurgeRewriteRule,
  PurgeSettings,
  PurgeStats,
  PurgeRuntime,
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
import { SectionGuard } from '../components/SectionBoundary';
import { downloadText, toCsv } from '../lib/format';
import { copyText } from '../lib/clipboard';
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

/**
 * 数字输入失焦时的即时校验与收敛（纯函数）。
 *
 * 现状缺口：输入框只在**保存时**统一 clamp，用户填 999 一路看着 999，点保存后
 * 数值变成 32 却没有任何说明——他会以为值丢了或者被谁改回去了。
 *
 * 返回 `corrected`（收敛后的字符串，``=不改动``）与 `reason`（提示文案 key 的
 * 参数，null=不需要提示）。空串与非数字不做处理：那是保存前的正常中间态，
 * 在失焦时就报错会打断输入。
 */
export function clampNumInput(
  field: { min: number; max: number; label: string },
  raw: string,
): { corrected: string; notice: { kind: 'clamped' | 'rounded'; value: number } | null } {
  const trimmed = raw.trim();
  if (!trimmed) return { corrected: '', notice: null };
  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed)) return { corrected: '', notice: null };
  const truncated = Math.trunc(parsed);
  const clamped = Math.min(field.max, Math.max(field.min, truncated));
  if (clamped !== parsed) {
    // 区分两种原因：整数值被范围收紧 vs 小数被截断（提示词要说准）
    const kind = truncated !== parsed ? 'rounded' : 'clamped';
    return { corrected: String(clamped), notice: { kind, value: clamped } };
  }
  return { corrected: trimmed, notice: null };
}

/**
 * 统计导出后的提示：有下界档时**说清楚哪几档不准**。
 *
 * CSV 里 `lower_bound` 只是一个 0/1 列——运维拿到表不知道它是「下界标记」，
 * 看到「近 1h 拒答 3 次」会当成精确值。事件环被覆盖后那几档只会**少算**，
 * 不会多算，这个方向必须说明。
 */
export function statsExportNote(windows: Array<{ label: string; truncated?: boolean }>): string | null {
  const truncated = windows.filter((w) => w.truncated).map((w) => w.label);
  if (truncated.length === 0) return null;
  return truncated.join(' / ');
}

// 后端契约保证有 `stats`，但**渲染函数体里直接解构它**是脆弱点：一旦这个字段
// 缺失/为 null，异常发生在区块边界**之外**，任何 SectionGuard 都救不了，整页只剩
// 「出错了」（实测）。派生阶段就用零值兜底，让页面能继续用。
const EMPTY_STATS: PurgeStats = {
  responses: 0,
  retries: 0,
  early_success: 0,
  skill_hits: 0,
  refusals: 0,
  failure_refusal: 0,
  failure_empty: 0,
  failure_filter: 0,
  failure_other: 0,
  synonym_hits: 0,
  auto_preset_hits: 0,
};

/**
 * 重试率文案。`responses` 不是有效数字时给 `—`——否则会算出 `0.00`，
 * 把「没数据」显示成「0% 重试」，正好是最容易被误读的那个结论。
 */
export function retryRateText(responses: unknown, retries: unknown): string | null {
  if (typeof responses !== 'number' || !Number.isFinite(responses)) return null;
  if (typeof retries !== 'number' || !Number.isFinite(retries)) return null;
  return responses > 0 ? ((retries / responses) * 100).toFixed(2) : '0.00';
}

/** 缺 `runtime` 时的零值兜底（同 `EMPTY_STATS`：派生阶段解构崩了区块边界接不住）。 */
const EMPTY_RUNTIME: PurgeRuntime = { pid: 0, uptime_secs: 0, upstream: '', inflight: 0 };

/** 缺 `settings` 时的零值兜底（只读参数区读的是服务端快照，不在区块边界内）。 */
const EMPTY_SETTINGS_SNAPSHOT = {
  retry_limit: 0,
  fallback_trigger_attempt: 0,
  skill_inject_limit: 0,
  fallback_models: [] as string[],
};

/** 记录一次词表变更前的快照（供「撤销」用）。 */
export function undoSnapshot(
  kind: 'dead_zone' | 'synonym',
  rules: PurgeRewriteRule[],
  label: string,
): { kind: 'dead_zone' | 'synonym'; rules: PurgeRewriteRule[]; label: string } {
  // 浅拷贝数组即可：规则对象本身在编辑时是整对象替换（patchRule 用 map），
  // 不会就地改字段，所以旧数组里的对象引用是安全的
  return { kind, rules: [...rules], label };
}

/** 把快照应用回草稿的对应表（纯函数，便于测）。 */
export function applyUndo(
  draft: PurgeSettings,
  undo: { kind: 'dead_zone' | 'synonym'; rules: PurgeRewriteRule[] },
): PurgeSettings {
  return undo.kind === 'dead_zone'
    ? { ...draft, rewrite_rules: undo.rules }
    : { ...draft, synonym_pairs: undo.rules };
}

/** 一处超限的描述（保存前汇总用）。 */
export interface CapOverflow {
  key: string;
  label: string;
  n: number;
  cap: number;
}

/**
 * 汇总所有「后端会静默截断」的字段超限。
 *
 * 为什么集中一处：这些上限散在 6 个字段上（两张词表、异源模型、四处字符列表、
 * 模板表），各自只在自己输入框下显示一行红字，**都不阻止保存**——用户照点，
 * 后端 `normalize` 静默截断，事后才发现尾巴没了。集中汇总才能一次问清。
 */
export function collectCapOverflows(settings: {
  rewrite_rules: readonly unknown[];
  synonym_pairs: readonly unknown[];
  fallback_models: readonly unknown[];
  normalize_strip_prefixes: readonly unknown[];
  refusal_patterns?: readonly unknown[];
  failure_filter_keywords?: readonly unknown[];
  failure_empty_keywords?: readonly unknown[];
  structured_templates?: readonly unknown[];
}): CapOverflow[] {
  const checks: Array<[string, string, readonly unknown[] | undefined, number]> = [
    ['rewrite_rules', '死区词改写', settings.rewrite_rules, RULE_TABLE_CAP],
    ['synonym_pairs', '同义词', settings.synonym_pairs, RULE_TABLE_CAP],
    ['fallback_models', '异源模型', settings.fallback_models, FALLBACK_MODELS_CAP],
    ['normalize_strip_prefixes', 'system 剥离前缀', settings.normalize_strip_prefixes, LIST_CAP],
    ['refusal_patterns', '拒绝话术', settings.refusal_patterns ?? [], LIST_CAP],
    ['failure_filter_keywords', '失败关键词', settings.failure_filter_keywords ?? [], LIST_CAP],
    ['failure_empty_keywords', '空答关键词', settings.failure_empty_keywords ?? [], LIST_CAP],
    ['structured_templates', '结构化模板', settings.structured_templates ?? [], TEMPLATES_CAP],
  ];
  return checks
    .filter(([, , items, cap]) => (items?.length ?? 0) > cap)
    .map(([key, label, items, cap]) => ({ key, label, n: items?.length ?? 0, cap }));
}

/**
 * 统计会被后端**静默丢弃**的空原文行。
 *
 * 后端 `normalize` 对两张词表用 `filter_map`：`from` trim 后为空就直接剔除，
 * 不报错也不回传。用户点「添加规则」留了个空行再保存，那行会凭空消失——他会
 * 以为是界面丢了自己的输入。
 *
 * 只算空 `from`：`to` 为空是**有效语义**（= 把这个词从消息里删掉），后端也保留。
 */
export function countEmptyFromRules(settings: {
  rewrite_rules: ReadonlyArray<{ from?: string; to?: string }>;
  synonym_pairs: ReadonlyArray<{ from?: string; to?: string }>;
}): { deadZone: number; synonym: number } {
  const count = (rules: ReadonlyArray<{ from?: string }>): number =>
    rules.filter((rule) => (rule.from ?? '').trim() === '').length;
  return { deadZone: count(settings.rewrite_rules), synonym: count(settings.synonym_pairs) };
}

/**
 * 整张词表 → 纯文本（Tab 分隔、一行一条）。
 *
 * 为什么用 Tab 而不是 CSV：`parseBatchRules` 认 Tab 作为分隔符，所以这份文本能
 * **直接粘进另一张表的「批量粘贴」**，也能直接粘进 Excel 两列。用它做表间搬运，
 * 比「导出文件 → 再导入」少两步。
 */
export function rulesToText(rules: ReadonlyArray<{ from: string; to: string }>): string {
  return rules.map((rule) => `${rule.from}\t${rule.to}`).join('\n');
}

/**
 * 词表按关键词过滤（大小写不敏感，同时匹配原文与改写）。
 *
 * **保留原始下标**：过滤只是「看不看得到」，行内的编辑/删除/启停仍要写到
 * 真实位置。用过滤后的下标去 patch 会把改动写到别的行上——这是列表过滤最
 * 常见的错。
 */
export function filterRules<T extends { from: string; to: string }>(
  rules: readonly T[],
  keyword: string,
): Array<{ rule: T; index: number }> {
  const needle = keyword.trim().toLowerCase();
  const indexed = rules.map((rule, index) => ({ rule, index }));
  if (!needle) return indexed;
  return indexed.filter(
    (item) =>
      item.rule.from.toLowerCase().includes(needle) ||
      item.rule.to.toLowerCase().includes(needle),
  );
}

/**
 * 标记「整理时会被删掉」的行（空原文 / 空改写 / 原文重复）。
 *
 * 为什么要有它：`tidyRules` 一套就删，用户**看不到删之前长什么样**——只能先按
 * 「整理」再后悔。有了标记+筛选，就能在动手前逐条看过再决定。
 *
 * 判据与 `tidyRules` 必须一致（重复按 trim+小写比较、空值按 trim 后为空），
 * 否则「高亮的」与「真被删的」不是同一批，比不标更误导。
 */
export function ruleIssues<T extends { from: string; to: string }>(
  rules: readonly T[],
): Array<{ index: number; reason: 'emptyFrom' | 'emptyTo' | 'duplicate' }> {
  const seen = new Map<string, number>();
  const issues: Array<{ index: number; reason: 'emptyFrom' | 'emptyTo' | 'duplicate' }> = [];
  rules.forEach((rule, index) => {
    const from = rule.from.trim();
    if (!from) {
      issues.push({ index, reason: 'emptyFrom' });
      return;
    }
    if (!rule.to.trim()) {
      issues.push({ index, reason: 'emptyTo' });
      return;
    }
    const key = from.toLowerCase();
    const first = seen.get(key);
    if (first !== undefined) {
      // 只标后面那条：tidyRules 保留最早一条
      issues.push({ index, reason: 'duplicate' });
      return;
    }
    seen.set(key, index);
  });
  return issues;
}

/** 「上次运行」档的说明文案（时间 + 那次进程活了多久）。 */
export function lastRunNote(
  snapshot: { saved_at: number; uptime_secs: number } | null | undefined,
  tr: (key: string, params?: Record<string, string | number>) => string,
): string {
  if (!snapshot || !snapshot.saved_at) {
    // 没有快照时**说明为什么空**，而不是让用户面对一排 0 猜「是没数据还是真没流量」
    return tr('本进程还没关机过，所以没有「上次运行」快照。');
  }
  const when = new Date(snapshot.saved_at * 1000).toLocaleString();
  const secs = snapshot.uptime_secs;
  const hours = Math.floor(secs / 3600);
  const mins = Math.floor((secs % 3600) / 60);
  const span = hours > 0 ? tr('{{h}} 小时 {{m}} 分', { h: hours, m: mins }) : tr('{{m}} 分钟', { m: mins });
  return tr('上次运行快照：{{when}}（那次进程运行了 {{span}}）', { when, span });
}

/**
 * 单行的提示文案（优先级：空原文 > 空改写 > 原文重复）。
 *
 * 为什么空值必须排在重复前面：空 `from` 在重复检测里也会命中（多个空行互为重复），
 * 若按重复报，用户看到的是「与其他行的原文重复」——而真正的问题是**没填**。
 * 报错原因错了，修法也就错了。
 *
 * 空 `to` 的语义要**说准**：保存时后端保留它（= 把这个词从消息里删掉），但「整理」
 * 会把这类行删掉。两件事不同，混着说会让人以为保存也会丢。
 */
export function ruleRowHint(
  rule: { from: string; to: string },
  isDuplicate: boolean,
  tr: (key: string) => string,
): string | undefined {
  if (!rule.from.trim()) return tr('原文未填：保存时会被丢弃');
  if (!rule.to.trim()) {
    return tr('改写为空：保存后该词会从消息里删掉（「整理」会把这类行删除）');
  }
  if (isDuplicate) {
    return tr('与其他行的原文重复：改写按表序执行，只有最靠前的那条会真正生效');
  }
  return undefined;
}

/** 交换第 index 与第 index+delta 条规则（越界时原样返回）。 */
export function swapRules<T>(rules: readonly T[], index: number, delta: -1 | 1): T[] {
  const target = index + delta;
  if (index < 0 || index >= rules.length || target < 0 || target >= rules.length) {
    return [...rules];
  }
  const next = [...rules];
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

/**
 * 把指定下标的行停用（而不是删除）。
 *
 * 「异常行」多是空原文/空改写/重复：直接删是不可逆的，而**停用**既让它们不再参与
 * 改写，又保留了用户可能想回填的内容。批量操作要给这个中间档，而不是只有「整理掉」。
 */
export function disableRulesAt<T extends { enabled?: boolean }>(
  rules: readonly T[],
  indices: ReadonlySet<number>,
): T[] {
  return rules.map((rule, index) =>
    indices.has(index) ? { ...rule, enabled: false } : rule,
  );
}

/** 严重级 → 徽标文案（i18n key 与中文同形）。 */
const SEVERITY_LABEL: Record<'error' | 'warn' | 'info', string> = {
  error: '不生效',
  warn: '需确认',
  info: '提示',
};

/**
 * 体检告警按严重级排序（Error → Warn → Info，同级保持后端原序）。
 *
 * 为什么在前端排而不在后端排：后端 `config_warning_items` 的顺序是「检查代码
 * 的执行顺序」，对排查有意义（总开关相关的先出现）；分级只是展示优先级，不该
 * 改动后端产出的语义。前端稳定排序两者兼得。
 *
 * 旧后端不返回 `severity` 时按 `error` 处理——与后端「未登记 code 兜底 Error」
 * 同一立场（宁严不宽）。
 */
export function sortWarningsBySeverity<T extends { severity?: string }>(items: T[]): T[] {
  const rank = (item: T): number => {
    switch (item.severity) {
      case 'warn':
        return 1;
      case 'info':
        return 2;
      default:
        return 0; // error 或未声明
    }
  };
  // Array.prototype.sort 在现代引擎里稳定，同级保持原序
  return [...items].sort((a, b) => rank(a) - rank(b));
}

/** 词表整理结论（`active` = 原表，`kept` = 整理后）。 */
export interface RulesTidyResult {
  kept: PurgeRewriteRule[];
  /** 原文为空被删的条数 */
  droppedEmptyFrom: number;
  /** 改写为空被删的条数 */
  droppedEmptyTo: number;
  /** 原文重复被删的条数（保留最早出现的一条） */
  droppedDuplicate: number;
}

/**
 * 整理词表：删空原文 / 删空改写 / 原文去重（保留最早）。
 *
 * 为什么需要：`from` 为空的行后端 `normalize` 会丢，`to` 为空等于「删掉这个词」
 * ——两者都不会报错，长表里越积越多；重复原文更是保存后只留最早一条，界面
 * 上却还列着。这里做一次显式整理，并**如实报出每类删了几条**（不静默）。
 *
 * 判定口径与后端保持一致：`from` 比较用 trim + 小写（后端 `rule.from.trim()`，
 * 前端 `dupHint` 也用 `toLowerCase()`），避免「界面说不重复、保存后被合并」。
 */
export function tidyRules(rules: PurgeRewriteRule[]): RulesTidyResult {
  const seen = new Set<string>();
  const kept: PurgeRewriteRule[] = [];
  let droppedEmptyFrom = 0;
  let droppedEmptyTo = 0;
  let droppedDuplicate = 0;
  for (const rule of rules) {
    const from = rule.from.trim();
    if (!from) {
      droppedEmptyFrom += 1;
      continue;
    }
    if (!rule.to.trim()) {
      droppedEmptyTo += 1;
      continue;
    }
    const key = from.toLowerCase();
    if (seen.has(key)) {
      droppedDuplicate += 1;
      continue;
    }
    seen.add(key);
    kept.push(rule);
  }
  return { kept, droppedEmptyFrom, droppedEmptyTo, droppedDuplicate };
}

/** 词表整理结论的可读摘要（0 项时返回 null，调用方据此决定是否提示）。 */
export function tidySummary(result: RulesTidyResult): {
  removed: number;
  parts: { emptyFrom: number; emptyTo: number; duplicate: number };
} {
  const removed =
    result.droppedEmptyFrom + result.droppedEmptyTo + result.droppedDuplicate;
  return {
    removed,
    parts: {
      emptyFrom: result.droppedEmptyFrom,
      emptyTo: result.droppedEmptyTo,
      duplicate: result.droppedDuplicate,
    },
  };
}

/** 配置项对比结论的一行：`key` 是 i18n 文案，`params` 插值。 */
export interface SettingsDiffItem {
  key: string;
  params?: Record<string, string | number>;
}

/**
 * 对比「已保存」与「草稿」，列出改了什么（纯函数）。
 *
 * 为什么需要：破甲有 8 个开关 + 两张词表 + 模板/预设/技能，改完十几项之后
 * 「我到底动了哪些」靠记忆；`dirty` 只告诉你有改动，不告诉你是哪几项。
 *
 * 口径刻意在**数据面语义层**而不是逐字段比对：
 * - 两张词表按「条数 + 启用行数」报（改一行的 to 也要能看出来 → 再加 `sha` 式
 *   全量比较不现实，改为「条数或启用数变化」才报）
 * - 模板按名称集合比较（改名/增删都算变化）
 * - `fallback_models` 按数组内容比较
 */
export function diffSettings(
  saved: PurgeSettings,
  draft: PurgeSettings,
): SettingsDiffItem[] {
  const items: SettingsDiffItem[] = [];
  const bools: Array<[keyof PurgeSettings, string]> = [
    ['enabled', '总开关'],
    ['system_normalize', 'system 归一化'],
    ['dead_zone_rewrite', '死区词改写'],
    ['synonym_enabled', '同义词替换'],
    ['reduce_reasoning', '降思考'],
    ['cross_source_fallback', '异源递进'],
    ['skill_inject', '技能注入'],
    ['structured_prompt', '结构化提示'],
    ['auto_preset', '按模型套预设'],
  ];
  for (const [field, label] of bools) {
    if (saved[field] !== draft[field]) {
      items.push({
        key: '{{label}}：{{from}} → {{to}}',
        params: {
          label,
          from: saved[field] ? '开' : '关',
          to: draft[field] ? '开' : '关',
        },
      });
    }
  }

  const nums: Array<[keyof PurgeSettings, string]> = [
    ['retry_limit', '重试上限'],
    ['fallback_trigger_attempt', '异源触发轮次'],
    ['skill_inject_limit', '技能注入上限'],
  ];
  for (const [field, label] of nums) {
    if (saved[field] !== draft[field]) {
      items.push({
        key: '{{label}}：{{from}} → {{to}}',
        params: { label, from: String(saved[field]), to: String(draft[field]) },
      });
    }
  }

  const strs: Array<[keyof PurgeSettings, string]> = [
    ['reasoning_floor', '降档目标'],
    ['skill_dir', '技能目录'],
    ['active_template', '启用模板'],
    ['auto_preset_id', '固定预设 id'],
    ['upstream_label', '上游标签'],
  ];
  for (const [field, label] of strs) {
    if (saved[field] !== draft[field]) {
      items.push({
        key: '{{label}}：{{from}} → {{to}}',
        params: {
          label,
          from: String(saved[field] || '—'),
          to: String(draft[field] || '—'),
        },
      });
    }
  }

  // 词表：条数或启用行数变化才报（逐行 diff 对「改了哪行」帮助有限，噪音更大）
  const tables: Array<[keyof PurgeSettings, string]> = [
    ['rewrite_rules', '死区词表'],
    ['synonym_pairs', '同义词表'],
  ];
  for (const [field, label] of tables) {
    const before = (saved[field] as PurgeRewriteRule[] | undefined) ?? [];
    const after = (draft[field] as PurgeRewriteRule[] | undefined) ?? [];
    const enabledOf = (rules: PurgeRewriteRule[]): number =>
      rules.filter((rule) => rule.enabled !== false).length;
    if (before.length !== after.length || enabledOf(before) !== enabledOf(after)) {
      items.push({
        key: '{{label}}：{{fromN}} 条（{{fromE}} 启用）→ {{toN}} 条（{{toE}} 启用）',
        params: {
          label,
          fromN: before.length,
          fromE: enabledOf(before),
          toN: after.length,
          toE: enabledOf(after),
        },
      });
    }
  }

  // 模板：按名称集合比较（改名 / 增删都算变化）
  const namesOf = (templates: Array<{ name: string }> | undefined): string =>
    (templates ?? [])
      .map((tpl) => tpl.name.trim())
      .filter(Boolean)
      .sort()
      .join('|');
  if (namesOf(saved.structured_templates) !== namesOf(draft.structured_templates)) {
    items.push({
      key: '结构化模板组发生变化（当前 {{n}} 个）',
      params: { n: (draft.structured_templates ?? []).length },
    });
  }

  // 异源模型：逐个比较（用户最关心「换了哪个模型」）
  const beforeModels = (saved.fallback_models ?? []).join(',');
  const afterModels = (draft.fallback_models ?? []).join(',');
  if (beforeModels !== afterModels) {
    items.push({
      key: '异源模型：{{from}} → {{to}}',
      params: {
        from: beforeModels || '—',
        to: afterModels || '—',
      },
    });
  }

  return items;
}

/** 最近用过的预设 id（最多 5 条）。 */
export const PRESET_RECENT_KEY = 'aigx.purge.presetRecent';
export const PRESET_RECENT_MAX = 5;

/**
 * 读「最近用过」的预设 id。
 *
 * 为什么值得存：预设库有 50+ 条，实际在用的就那么几条；每次翻页都在长列表里
 * 重新找。这里只做**标记**（条目上挂「最近用过」），不改筛选也不改分页——
 * 改筛选会与后端分页冲突（后端不认识这份 id 表，`total` 与翻页都会错）。
 */
export function readRecentPresets(get: (k: string) => string | null): string[] {
  try {
    const raw = JSON.parse(get(PRESET_RECENT_KEY) ?? '[]');
    if (!Array.isArray(raw)) return [];
    return raw.filter((x): x is string => typeof x === 'string' && x.length > 0).slice(0, PRESET_RECENT_MAX);
  } catch {
    return [];
  }
}

/** 记一次使用：去重置顶、截到上限、空表时移除键。 */
export function noteRecentPreset(
  prev: string[],
  id: string,
  set: (k: string, v: string) => void,
  remove: (k: string) => void,
): string[] {
  const next = [id, ...prev.filter((x) => x !== id)].slice(0, PRESET_RECENT_MAX);
  try {
    if (next.length === 0) remove(PRESET_RECENT_KEY);
    else set(PRESET_RECENT_KEY, JSON.stringify(next));
  } catch {
    // localStorage 满/禁用：静默，标记只是便利
  }
  return next;
}

/** 命中片段：`before`/`after` 是上下文，`hit` 是原文里那一段（保留原大小写）。 */
export interface BodySnippet {
  before: string;
  hit: string;
  after: string;
}

/**
 * 在正文里定位检索词，取前后各 `radius` 个字符的上下文（纯函数）。
 *
 * 为什么需要：列表标了「正文命中」之后，用户还得展开正文自己找那一处——预设
 * 正文动辄 20KB。这里直接给出命中片段，展开即可看到。
 *
 * 大小写不敏感定位（与后端 `matches` 同口径），但**返回原文切片**：正文大小写
 * 是作者写的，展示时不该被改。
 */
export function findBodySnippet(
  body: string,
  needle: string,
  radius = 60,
): BodySnippet | null {
  const trimmed = needle.trim();
  if (!trimmed || !body) return null;
  const index = body.toLowerCase().indexOf(trimmed.toLowerCase());
  if (index < 0) return null;
  const start = Math.max(0, index - radius);
  const end = Math.min(body.length, index + trimmed.length + radius);
  return {
    before: (start > 0 ? '…' : '') + body.slice(start, index),
    hit: body.slice(index, index + trimmed.length),
    after: body.slice(index + trimmed.length, end) + (end < body.length ? '…' : ''),
  };
}

/**
 * 体检结果导出成纯文本（贴工单/群聊时用）。
 *
 * 按严重级分组输出，**同级保持原序**；无告警时给一行「已通过」——空串会让人
 * 以为复制失败，而「没有告警」本身就是结论。
 */
export function warningReportText(
  items: Array<{ message: string; severity?: string }>,
  translate: (key: string) => string,
): string {
  if (items.length === 0) return translate('配置体检通过：当前没有「开了但不生效」或自相矛盾的项目。');
  const groups: Array<{ severity: string; label: string }> = [
    { severity: 'error', label: '不生效' },
    { severity: 'warn', label: '需确认' },
    { severity: 'info', label: '提示' },
  ];
  const lines: string[] = [];
  for (const group of groups) {
    const hits = items.filter((item) => (item.severity ?? 'error') === group.severity);
    if (hits.length === 0) continue;
    lines.push(`## ${translate(group.label)}`);
    for (const hit of hits) lines.push(`- ${hit.message}`);
  }
  return lines.join('\n');
}

/**
 * 破甲现状快照（一次复制全部）：交接/报障时要的是「全景」，不是三个片段。
 *
 * 结构固定为四段（运行 → 生效项 → 体检 → 统计），每段都带标题，粘到工单里
 * 对方能按段读。**不做任何计算**：只用传进来的快照值，避免「复制出来的数和
 * 页面上显示的不一致」。
 */
export function snapshotReportText(input: {
  runtime: { pid: number; uptime_secs: number; upstream: string };
  activeItems: string[];
  warnings: Array<{ message: string; severity?: string }>;
  stats: Record<string, number>;
  baselineAt?: number;
  translate: (key: string) => string;
}): string {
  const { runtime, activeItems, warnings, stats, baselineAt, translate: tr } = input;
  const lines: string[] = [];

  lines.push(`## ${tr('运行')}`);
  lines.push(`- PID ${runtime.pid} · ${tr('已运行')} ${runtime.uptime_secs}s · ${tr('上游')} ${runtime.upstream || '—'}`);
  if (baselineAt) {
    lines.push(`- ${tr('统计基线')} ${new Date(baselineAt * 1000).toLocaleString()}`);
  }

  lines.push(`## ${tr('当前生效项')}`);
  if (activeItems.length === 0) lines.push(`- ${tr('（无）')}`);
  for (const item of activeItems) lines.push(`- ${item}`);

  lines.push(`## ${tr('配置体检')}`);
  if (warnings.length === 0) {
    lines.push(`- ${tr('通过：没有「开了但不生效」或自相矛盾的项目')}`);
  } else {
    for (const warning of warnings) {
      const level = warning.severity ?? 'error';
      lines.push(`- [${level}] ${warning.message}`);
    }
  }

  lines.push(`## ${tr('统计')}`);
  // 键序按后端 JSON 顺序（Object.entries 保序），不排序以免和页面卡片顺序对不上
  const entries = Object.entries(stats);
  if (entries.length === 0) lines.push(`- ${tr('（无）')}`);
  for (const [key, value] of entries) lines.push(`- ${key}: ${value}`);

  return lines.join('\n');
}

/** 批量粘贴解析结果：`rules` 可直接追加，`skipped` 是没能解析的行数。 */
export interface BatchParseResult {
  rules: PurgeRewriteRule[];
  skipped: number;
}

/**
 * 批量粘贴解析：一行一条 `原文 <分隔符> 改写`。
 *
 * 为什么要它：真实词表动辄几十条，界面上只能一条条点「添加规则」手填——这是
 * 破甲页最耗时的操作，而用户手上通常已经有 Excel / 文档里的两列表。
 *
 * 认的分隔符按优先级：**Tab**（从 Excel 直接粘贴就是这个）、`->`、`=>`、`→`、
 * 全角箭头 `⇒`。**不认逗号**：改写内容本身常含中英文逗号（「好的，」「, then」），
 * 拿它当分隔符会把一条规则劈成两半，比不识别更坏。
 *
 * 空行跳过（不计入 skipped）；分不出两段的行计入 skipped，让调用方如实回报。
 * 与现有规则 `from` 重复（trim + 忽略大小写）的行**丢弃**——重复项在词表里
 * 没有额外作用，却会让「整理」时又要删一遍。
 */
export function parseBatchRules(
  text: string,
  existing: PurgeRewriteRule[] = [],
): BatchParseResult {
  const SEPARATORS = ['\t', '->', '=>', '→', '⇒'];
  const seen = new Set(existing.map((rule) => rule.from.trim().toLowerCase()));
  const rules: PurgeRewriteRule[] = [];
  let skipped = 0;

  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line) continue;
    // 表头行（「原文 / 改写」这类）没有分隔符，会自然落进 skipped——不特殊处理，
    // 免得把真实规则「坏->坏」误判成表头
    let from = '';
    let to = '';
    const separator = SEPARATORS.find((sep) => line.includes(sep));
    if (separator) {
      const at = line.indexOf(separator);
      from = line.slice(0, at).trim();
      to = line.slice(at + separator.length).trim();
    }
    if (!from || !to) {
      skipped += 1;
      continue;
    }
    const key = from.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    rules.push({ from, to });
  }

  return { rules, skipped };
}

/** 两份配置是否等价（只比数据面关心的字段，忽略纯展示差异）。 */
export function settingsEqual(a: PurgeSettings, b: PurgeSettings): boolean {
  return JSON.stringify(sortSettings(a)) === JSON.stringify(sortSettings(b));
}

/** 稳定化：键排序后再序列化，避免键序不同被误判为「有差异」。 */
function sortSettings(value: PurgeSettings): unknown {
  const walk = (input: unknown): unknown => {
    if (Array.isArray(input)) return input.map(walk);
    if (input && typeof input === 'object') {
      return Object.fromEntries(
        Object.entries(input as Record<string, unknown>)
          .sort(([x], [y]) => x.localeCompare(y))
          .map(([k, v]) => [k, walk(v)]),
      );
    }
    return input;
  };
  return walk(value);
}

/** 未保存草稿的本地备份（防误刷新/误关页丢改动）。 */
export const DRAFT_BACKUP_KEY = 'aigx.purge.draftBackup';

/** 备份存活上限：超过一天基本是上次遗留，恢复它比丢掉更危险。 */
export const DRAFT_BACKUP_TTL_MS = 24 * 60 * 60 * 1000;

export interface DraftBackup {
  settings: PurgeSettings;
  savedAt: number;
}

/**
 * 读草稿备份。**过期/坏值一律当作没有**。
 *
 * 为什么需要它：破甲页的改动只存在内存里，点「保存」才落库——用户编辑十分钟、
 * 顺手刷新看统计，草稿全丢且没有任何提示。这类损失看不见（没有报错），最难
 * 事后追。
 */
export function readDraftBackup(
  get: (k: string) => string | null,
  now: number = Date.now(),
): DraftBackup | null {
  try {
    const raw = get(DRAFT_BACKUP_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<DraftBackup>;
    if (!parsed || typeof parsed !== 'object') return null;
    if (typeof parsed.savedAt !== 'number' || !parsed.settings) return null;
    if (now - parsed.savedAt > DRAFT_BACKUP_TTL_MS) return null;
    return { settings: parsed.settings as PurgeSettings, savedAt: parsed.savedAt };
  } catch {
    return null;
  }
}

/** 写草稿备份；空草稿（没有实际改动）时移除键，不留无意义条目。 */
export function writeDraftBackup(
  settings: PurgeSettings | null,
  set: (k: string, v: string) => void,
  remove: (k: string) => void,
  now: number = Date.now(),
): void {
  try {
    if (!settings) {
      remove(DRAFT_BACKUP_KEY);
      return;
    }
    set(DRAFT_BACKUP_KEY, JSON.stringify({ settings, savedAt: now }));
  } catch {
    // localStorage 满/禁用：静默。备份是便利，不能反过来打断编辑
  }
}

/** 生效项摘要导出成纯文本（贴给同事/贴进工单时用，不带 UI 装饰）。 */
export function activeSummaryText(
  items: ActiveItem[],
  translate: (key: string, params?: Record<string, string | number>) => string,
): string {
  return items.map((item) => `- ${translate(item.key, item.params)}`).join('\n');
}

/** 生效项摘要的一条：`key` 是 i18n 文案（含 `{{n}}` 等占位），`params` 是插值，
 *  `anchor` 是 `data-purge-fix` 锚点（点了滚到对应配置区）。 */
export interface ActiveItem {
  key: string;
  params?: Record<string, string | number>;
  /** 点了跳到哪：复用它旁边体检告警的锚点值 */
  anchor?: string;
}

/**
 * 「当前生效项」摘要（纯函数，便于单测）。
 *
 * 为什么需要：破甲有 8 个开关 + 两张词表 + 模板/预设/技能四块配置，配到
 * 十几项之后没人能一眼说出「现在到底哪些治理在生效」——只能逐个开关去数。
 * 这里按**数据面的真实语义**汇总（词表只算启用行、异源要看有没有模型），
 * 总开关关闭时明确说「全关」而不是列出零散项。
 */
export function buildActiveSummary(settings: PurgeSettings): ActiveItem[] {
  if (!settings.enabled) return [{ key: '总开关关闭：数据面不做任何治理' }];
  const items: ActiveItem[] = [];
  if (settings.system_normalize) items.push({ key: 'system 归一化', anchor: 'switches' });
  const deadZone = (settings.rewrite_rules ?? []).filter((rule) => rule.enabled !== false);
  if (settings.dead_zone_rewrite) {
    items.push(
      deadZone.length > 0
        ? { key: '死区词改写（{{n}} 条启用）', params: { n: deadZone.length }, anchor: 'dead-zone-table' }
        : { key: '死区词改写（开关开着但无启用行）', anchor: 'dead-zone-table' },
    );
  }
  const synonyms = (settings.synonym_pairs ?? []).filter((rule) => rule.enabled !== false);
  if (settings.synonym_enabled) {
    items.push(
      synonyms.length > 0
        ? { key: '同义词替换（{{n}} 条启用）', params: { n: synonyms.length }, anchor: 'synonym-table' }
        : { key: '同义词替换（开关开着但无启用行）', anchor: 'synonym-table' },
    );
  }
  if (settings.reduce_reasoning) {
    items.push({
      key: '降思考档位 → {{floor}}',
      params: { floor: settings.reasoning_floor || '—' },
      anchor: 'switches',
    });
  }
  if (settings.cross_source_fallback) {
    const models = settings.fallback_models ?? [];
    items.push(
      models.length > 0
        ? {
            key: '异源递进（{{n}} 个模型，第 {{attempt}} 轮起）',
            params: { n: models.length, attempt: settings.fallback_trigger_attempt },
            anchor: 'fallback-models',
          }
        : { key: '异源递进（开关开着但无候选模型）', anchor: 'fallback-models' },
    );
  }
  if (settings.skill_inject) {
    items.push({
      key: '技能注入（上限 {{limit}} 字符）',
      params: { limit: settings.skill_inject_limit },
      anchor: 'skill-dir',
    });
  }
  if (settings.structured_prompt) {
    const template = settings.active_template?.trim();
    items.push(
      template
        ? { key: '结构化提示（模板「{{name}}」）', params: { name: template }, anchor: 'templates' }
        : { key: '结构化提示（默认后缀）', anchor: 'templates' },
    );
  }
  if (settings.auto_preset) {
    const pinned = settings.auto_preset_id?.trim();
    items.push(
      pinned
        ? { key: '按模型套预设（固定「{{id}}」）', params: { id: pinned }, anchor: 'auto-preset-id' }
        : { key: '按模型套预设（自动匹配）', anchor: 'auto-preset-id' },
    );
  }
  return items;
}

/** 导入校验结论：`fatal` 非空 = 拒绝导入；`warnings` = 可导入但有风险。 */export interface ImportVerdict {
  fatal: string[];
  warnings: string[];
}

/** 必须是「数组，且每个元素都是对象」的字段（渲染期会 `.map` 或取属性）。 */
const ARRAY_FIELDS = [
  'rewrite_rules',
  'synonym_pairs',
  'fallback_models',
  'normalize_strip_prefixes',
  'structured_templates',
  'refusal_patterns',
  'failure_filter_keywords',
  'failure_empty_keywords',
] as const;

/** 必须是「数组元素都带 string 型 from/to」的字段（词表两表）。 */
const RULE_TABLE_FIELDS = ['rewrite_rules', 'synonym_pairs'] as const;

/**
 * 导入配置的结构校验（纯函数，便于单测）。
 *
 * 两级判定是刻意的：
 * - **fatal**：结构坏了就渲染不出来（`synonym_pairs` 不是数组 → `.map` 抛错
 *   整页白屏；规则行 from 是数字 → React 渲染对象报错）。这类必须拒绝导入。
 * - **warnings**：能渲染但语义可疑（数字字段越界/是字符串 → 保存时被 clamp，
 *   用户看不到差异；模板缺名 → 无法被 `active_template` 选中）。提示但仍导入，
 *   因为「先看一眼再修」比「拒之门外」更符合导出-手改-再导入的工作流。
 *
 * 缺省字段一律不算问题：旧后端不返回 `structured_templates` 等，导出的旧文件
 * 本来就该能导回来。
 */
export function validateImportedSettings(raw: unknown): ImportVerdict {
  const fatal: string[] = [];
  const warnings: string[] = [];
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    return { fatal: ['配置根节点不是对象'], warnings };
  }
  const settings = raw as Record<string, unknown>;

  for (const field of ARRAY_FIELDS) {
    const value = settings[field];
    if (value === undefined) continue;
    if (!Array.isArray(value)) {
      fatal.push(`${field} 不是数组`);
      continue;
    }
    if (value.some((item) => !item || typeof item !== 'object' || Array.isArray(item))) {
      fatal.push(`${field} 里存在非对象条目`);
    }
  }
  for (const field of RULE_TABLE_FIELDS) {
    const value = settings[field];
    if (!Array.isArray(value)) continue;
    const bad = value.findIndex((rule) => {
      if (!rule || typeof rule !== 'object') return false; // 上一轮已按 fatal 处理
      const row = rule as Record<string, unknown>;
      return typeof row.from !== 'string' || typeof row.to !== 'string';
    });
    if (bad >= 0) fatal.push(`${field} 第 ${bad + 1} 条的原文/改写不是字符串`);
  }

  // 数字字段：越界或非数字都会被保存时静默 clamp，导入时就要说清楚
  for (const field of NUM_FIELDS) {
    const value = settings[field.key];
    if (value === undefined) continue;
    if (typeof value !== 'number' || !Number.isFinite(value)) {
      warnings.push(`${field.label} 不是有效数字，保存时会回落默认值`);
    } else if (value < field.min || value > field.max) {
      warnings.push(
        `${field.label} 为 ${value}，超出 ${field.min}–${field.max}，保存时会被夹到边界`,
      );
    }
  }

  // 模板缺名：能渲染但选不中（active_template 按名匹配）
  if (Array.isArray(settings.structured_templates)) {
    const unnamed = settings.structured_templates.filter(
      (tpl) => !tpl || typeof tpl !== 'object' || typeof (tpl as { name?: unknown }).name !== 'string'
        || !(tpl as { name: string }).name.trim(),
    ).length;
    if (unnamed > 0) warnings.push(`${unnamed} 个结构化模板没有名称，无法被「当前启用模板」选中`);
  }

  // 布尔字段非布尔：会被当 falsy，开关静默关闭
  const boolKeys = [
    'enabled',
    'system_normalize',
    'dead_zone_rewrite',
    'synonym_enabled',
    'reduce_reasoning',
    'cross_source_fallback',
    'skill_inject',
    'structured_prompt',
    'auto_preset',
  ] as const;
  const badBool = boolKeys.filter(
    (key) => settings[key] !== undefined && typeof settings[key] !== 'boolean',
  );
  if (badBool.length > 0) {
    warnings.push(`这些开关不是布尔值，导入后会被当作关闭：${badBool.join('、')}`);
  }

  return { fatal, warnings };
}

function splitList(value: string): string[] {  return value
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
 * 关键词 → 预览样例。技能命中是**子串**匹配（`keyword_matches` 走
 * `contains`），所以样例只需把这个词原样包进一句自然语言里，既能验证
 * 「这个词命中」，也看得出注入块插在消息尾部的位置。
 */
export function skillKeywordSample(keyword: string): string {
  const trimmed = keyword.trim();
  return trimmed ? `请帮我处理一下 ${trimmed} 相关的问题` : '';
}

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

/** 预设库筛选（搜索词 + 厂商）的持久化键。 */
const PRESET_FILTER_KEY = 'aigx.purge.presetFilter';
/** 搜索词上限：超出不入库（防超长串撑爆 localStorage） */
const PRESET_FILTER_Q_MAX = 200;

/** 预设库筛选条件（空串 = 不筛）。 */
export interface PresetFilter {
  q: string;
  vendor: string;
}

/**
 * 读回上次的预设库筛选。
 *
 * 为什么值得存：预设库有几十条，运维常按某厂商（或某个词）反复查；
 * 刷新一次就归零，等于每次都要重打一遍。坏 JSON / 类型不符 → 空筛选
 * （宁可从头筛，也不要拿脏值去请求后端）。
 */
export function readPresetFilter(get: (k: string) => string | null): PresetFilter {
  try {
    const raw = JSON.parse(get(PRESET_FILTER_KEY) ?? '{}') as unknown;
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return { q: '', vendor: '' };
    const record = raw as Record<string, unknown>;
    const q = typeof record.q === 'string' ? record.q.slice(0, PRESET_FILTER_Q_MAX) : '';
    const vendor = typeof record.vendor === 'string' ? record.vendor : '';
    return { q, vendor };
  } catch {
    return { q: '', vendor: '' };
  }
}

/** 写筛选：两项都空时移除键（不留 `{"q":"","vendor":""}` 垃圾）。 */
export function writePresetFilter(
  filter: PresetFilter,
  set: (k: string, v: string) => void,
  remove: (k: string) => void,
): void {
  try {
    if (!filter.q && !filter.vendor) remove(PRESET_FILTER_KEY);
    else
      set(
        PRESET_FILTER_KEY,
        JSON.stringify({ q: filter.q.slice(0, PRESET_FILTER_Q_MAX), vendor: filter.vendor }),
      );
  } catch {
    // localStorage 满/禁用：静默，筛选照常在本会话内工作
  }
}

/** localStorage 读写器：模块级常量（引用恒定，可安全进 useCallback 依赖）。 */
const lsSetStable = (k: string, v: string): void => window.localStorage.setItem(k, v);
const lsRemoveStable = (k: string): void => window.localStorage.removeItem(k);

/** 预览样例历史的导出/导入（团队共享同一批测试用例）。 */
export const PREVIEW_HISTORY_FILE_VERSION = 1;

/** 导出为 JSON 文本（带 version，便于以后兼容演进）。 */
export function exportPreviewHistory(items: string[]): string {
  return `${JSON.stringify({ version: PREVIEW_HISTORY_FILE_VERSION, samples: items }, null, 2)}\n`;
}

/**
 * 解析导入的样例历史文件。
 *
 * 接受三种形态：本页导出的 `{version, samples:[...]}`、裸数组、单个字符串。
 * 校验与写历史同一套口径（非空、单条 ≤4KB、去重保序、截断到 8 条）——
 * 导入路径如果比写入路径宽松，就会出现「导进来 20 条、刷新后只剩 8 条」的
 * 静默截断。无法解析返回 `null`（调用方区分「格式错」与「空文件」）。
 */
export function parsePreviewHistoryJson(text: string): string[] | null {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return null;
  }
  const list = Array.isArray(raw)
    ? raw
    : raw && typeof raw === 'object' && Array.isArray((raw as { samples?: unknown }).samples)
      ? (raw as { samples: unknown[] }).samples
      : typeof raw === 'string'
        ? [raw]
        : null;
  if (list === null) return null;
  const seen = new Set<string>();
  const out: string[] = [];
  for (const item of list) {
    if (typeof item !== 'string') continue;
    const trimmed = item.trim();
    if (!trimmed || trimmed.length > PREVIEW_HISTORY_ITEM_MAX) continue;
    if (seen.has(trimmed)) continue;
    seen.add(trimmed);
    out.push(item);
    if (out.length >= PREVIEW_HISTORY_MAX) break;
  }
  return out;
}

/** 历史条目的展示摘要：首行截 46 字符（角色前缀形态保留前缀可读性）。 */
export function previewHistoryLabel(sample: string): string {
  const first = sample.split(/\r?\n/, 1)[0] ?? '';
  const prefix = /^(system|user|assistant):\s/i.test(first) ? '~ ' : '';
  const text = first.replace(/^(system|user|assistant):\s+/i, (m) => m);
  return prefix + (text.length > 46 ? `${text.slice(0, 46)}…` : text);
}

/** 历史写回（删除/清空共用）：`items` 为空时直接移除 key，不留空数组。 */
export function persistPreviewHistory(
  items: string[],
  set: (k: string, v: string) => void,
  remove: (k: string) => void,
): void {
  try {
    if (items.length === 0) remove(PREVIEW_HISTORY_KEY);
    else set(PREVIEW_HISTORY_KEY, JSON.stringify(items));
  } catch {
    // localStorage 满/禁用：静默，历史不进不删也不影响预览本体
  }
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

/**
 * 把配置里的数组字段兜成真数组。
 *
 * 为什么必须在**进入点**做（而不是渲染时逐处 `?.`）：
 * 词表/模板/异源模型在 JSX 里到处 `.map()`、`.length`、`.join()` —— 那些求值
 * 都发生在 `PurgeConsole` 自己的渲染作用域，**ErrorBoundary 接不住**（它只捕获
 * 子组件渲染时的异常）。进入点规范化一次，渲染层就永远拿到规范形状。
 */
export function normalizeSettingsArrays(settings: PurgeSettings): PurgeSettings {
  const arr = <T,>(value: unknown): T[] => (Array.isArray(value) ? (value as T[]) : []);
  return {
    ...settings,
    rewrite_rules: arr<PurgeRewriteRule>(settings.rewrite_rules),
    synonym_pairs: arr<PurgeRewriteRule>(settings.synonym_pairs),
    structured_templates: arr<PurgeTemplate>(settings.structured_templates),
    normalize_strip_prefixes: arr<string>(settings.normalize_strip_prefixes),
    fallback_models: arr<string>(settings.fallback_models),
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
  onMove,
  canMoveUp,
  canMoveDown,
  moveUpLabel,
  moveDownLabel,
  onProbe,
  probeLabel,
}: {
  rule: PurgeRewriteRule;
  index: number;
  fromLabel: string;
  toLabel: string;
  removeLabel: string;
  /** 「试」按钮文案（用该条原文做预览样例，验证到底命中没命中） */
  probeLabel: string;
  /** 单条启停的无障碍标签 */
  enableLabel: string;
  /** 与其他行原文重复时的行内提示（后端会静默截断重复规则，前端显式提示） */
  dupHint?: string;
  onPatch: (index: number, part: Partial<PurgeRewriteRule>) => void;
  onRemove: (index: number) => void;
  /** 上移/下移（改写按表序执行，顺序有意义时才需要） */
  onMove?: (index: number, delta: -1 | 1) => void;
  /** 是否为第一行/最后一行（决定移动按钮是否可用） */
  canMoveUp?: boolean;
  canMoveDown?: boolean;
  moveUpLabel?: string;
  moveDownLabel?: string;
  /** 用这一条的原文做预览样例（验证它到底命中没命中） */
  onProbe?: (rule: PurgeRewriteRule) => void;
}): JSX.Element {
  const enabled = rule.enabled ?? true;
  return (
    <div className={`purge-rule-row${enabled ? '' : ' is-disabled'}`} data-rule-index={index}>
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
      {/* 开关 + 试 + 删除包成一组：桌面上占网格最后一列（横排），窄屏时整行横排
          —— 否则单列布局下三个控件各占一行，一条规则要占 7 行高 */}
      <div className="purge-rule-ops">
        {/* 调序：改写按表序执行，顺序决定谁先命中（重复 from 只有最靠前生效） */}
        {onMove ? (
          <>
            <Button
              variant="outline"
              size="sm"
              data-testid={`purge-rule-up-${index}`}
              disabled={canMoveUp === false}
              title={moveUpLabel}
              aria-label={moveUpLabel}
              onClick={() => onMove(index, -1)}
            >
              ↑
            </Button>
            <Button
              variant="outline"
              size="sm"
              data-testid={`purge-rule-down-${index}`}
              disabled={canMoveDown === false}
              title={moveDownLabel}
              aria-label={moveDownLabel}
              onClick={() => onMove(index, 1)}
            >
              ↓
            </Button>
          </>
        ) : null}
        <label className="purge-rule-toggle" title={enableLabel}>
          <input
            type="checkbox"
            checked={enabled}
            aria-label={enableLabel}
            onChange={(event) => onPatch(index, { enabled: event.target.checked })}
          />
        </label>
        {onProbe && rule.from.trim() ? (
          <Button
            variant="outline"
            size="sm"
            title={`用「${rule.from}」做预览样例`}
            onClick={() => onProbe(rule)}
          >
            {probeLabel}
          </Button>
        ) : null}
        <Button variant="outline" size="sm" onClick={() => onRemove(index)}>
          {removeLabel}
        </Button>
      </div>
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
  retryRate: string | null;
  suffix?: string;
  atLeast?: boolean;
  t: TFunction;
}): JSX.Element {
  const mk = (base: string): string => (suffix ? `${base} · ${suffix}` : base);
  // 非数字显示 `—` 而不是 `undefined`/`NaN`：后端字段形状异常时，界面不该把
  // 内部值原样漏给用户（显示 `undefined` 会像页面的 bug，`—` 才是「没数据」）
  const v = (n: number): string => {
    if (typeof n !== 'number' || !Number.isFinite(n)) return '—';
    return atLeast ? `≥ ${n}` : String(n);
  };
  return (
    <div className="purge-stats" role="group" aria-label={t('统计')}>
      <StatCard title={mk(t('响应总数'))} value={v(stats.responses)} />
      <StatCard
        title={mk(t('重试率'))}
        value={retryRate === null ? '—' : `${atLeast ? '≥ ' : ''}${retryRate}%`}
      />
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

/** 破甲页的页内导航锚点（顺序 = 页面从上到下的顺序）。 */
const PAGE_SECTIONS: Array<{ anchor: string; label: string }> = [
  { anchor: 'stats', label: '统计' },
  { anchor: 'switches', label: '开关' },
  { anchor: 'dead-zone-table', label: '死区词表' },
  { anchor: 'synonym-table', label: '同义词表' },
  { anchor: 'skill-dir', label: '技能与模板' },
  { anchor: 'presets', label: '预设库' },
  { anchor: 'preview', label: '改写预览' },
];

/**
 * 页内导航条：破甲页有 8 个区块、十几屏，滚到底想回顶部改一个开关要滑很久。
 *
 * 复用 `focusFixTarget`（同一次滚动 + 闪烁），不引入第二套跳转实现。
 */
const PageNav = memo(function PageNav({ t }: { t: TFunction }): JSX.Element {
  return (
    <nav className="purge-pagenav" aria-label={t('页内导航')} data-testid="purge-pagenav">
      {PAGE_SECTIONS.map((section) => (
        <button
          key={section.anchor}
          type="button"
          className="purge-pagenav-item"
          onClick={() => focusFixTarget(section.anchor)}
        >
          {t(section.label)}
        </button>
      ))}
    </nav>
  );
});

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
  /** 命中来自正文（后端只在「元数据搜不到、正文搜得到」时给 true） */
  body_match?: boolean | null;
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
  // 筛选条件跨刷新保留（运维常按某厂商反复查；刷新归零等于每次重打）
  const initialFilter = readPresetFilter((k) => window.localStorage.getItem(k));
  const [q, setQ] = useState(initialFilter.q);
  const [debouncedQ, setDebouncedQ] = useState(initialFilter.q);
  const [vendor, setVendor] = useState(initialFilter.vendor);
  const [list, setList] = useState<PurgePresetList | null>(null);
  // 「最近用过」的预设 id：只做标记，不参与筛选/分页（后端不认识这份表）
  const [recentIds, setRecentIds] = useState<string[]>(() =>
    readRecentPresets((k) => window.localStorage.getItem(k)),
  );
  const noteRecent = useCallback((id: string): void => {
    setRecentIds((prev) =>
      noteRecentPreset(
        prev,
        id,
        (k, v) => window.localStorage.setItem(k, v),
        (k) => window.localStorage.removeItem(k),
      ),
    );
  }, []);

  // 筛选变化即落盘（写失败静默；本会话内筛选照常工作）
  useEffect(() => {
    writePresetFilter(
      { q, vendor },
      (k, v) => window.localStorage.setItem(k, v),
      (k) => window.localStorage.removeItem(k),
    );
  }, [q, vendor]);
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
        noteRecent(id);
        addToast(t('已写入模板草稿，点底部「保存」后生效'), 'success');
        // 写入后立即把模板编辑区带到眼前并闪一下（词表很长时落点在页面底部）
        focusFixTarget('templates');
      } catch {
        addToast(t('应用预设失败'), 'error');
      } finally {
        setBusy(null);
      }
    },
    [addToast, currentTemplates, fetchBody, noteRecent, onApplyTemplate, t],
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
        // 非安全上下文（内网 http）下 clipboard API 不存在，用 copyText 的降级通道
        const copied = await copyText(full.body);
        if (!copied) throw new Error('copy failed');
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
        noteRecent(id);
        onSkillWritten();
      } catch {
        addToast(t('写入技能目录失败'), 'error');
      } finally {
        setBusy(null);
      }
    },
    [addToast, noteRecent, onSkillWritten, t],
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
          placeholder={t('搜索标题 / 厂商 / 模型 / 标签 / 正文')}
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
                      {item.body_match ? (
                        <span
                          className="purge-preset-match is-body"
                          title={t('标题/厂商/模型/标签里没有这个词，是正文里命中的')}
                        >
                          {t('正文命中')}
                        </span>
                      ) : null}
                      {recentIds.includes(item.id) ? (
                        <span className="purge-preset-match is-recent" title={t('你最近应用过这条预设')}>
                          {t('最近用过')}
                        </span>
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
                      <>
                        {/* 有检索词时先给命中片段：正文动辄 20KB，直接展开等于让人自己找 */}
                        {(() => {
                          const snippet = findBodySnippet(
                            detail && detail.id === item.id ? detail.body : item.preview,
                            debouncedQ,
                          );
                          return snippet ? (
                            <p className="purge-preset-snippet" data-testid="purge-preset-snippet">
                              {snippet.before}
                              <mark>{snippet.hit}</mark>
                              {snippet.after}
                            </p>
                          ) : null;
                        })()}
                        <pre className="purge-preset-preview" data-testid="purge-preset-preview">
                          {detail && detail.id === item.id ? detail.body : item.preview}
                        </pre>
                      </>
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
  /** 预览时套用的模型（非空则一并预演「按模型自动套用」的预设前插） */
  const [previewModel, setPreviewModel] = useState('');
  const savedForDiff = data?.settings;
  /** diff 只看变更行（长样例里上下文占绝大多数，遮住真正的改写点） */
  const [diffChangedOnly, setDiffChangedOnly] = useState(false);

  /** 样例历史导入用的文件选择器 */
  const previewHistoryFileRef = useRef<HTMLInputElement>(null);
  /** 删除一条历史样例（按下标，不改当前输入内容） */
  const removePreviewHistory = useCallback((index: number): void => {
    setPreviewHistory((prev) => {
      const next = prev.filter((_, i) => i !== index);
      persistPreviewHistory(next, lsSetStable, lsRemoveStable);
      return next;
    });
  }, []);
  /** 清空全部历史（localStorage key 一并移除，不留空数组） */
  const clearPreviewHistory = useCallback((): void => {
    persistPreviewHistory([], lsSetStable, lsRemoveStable);
    setPreviewHistory([]);
  }, []);

  /** 导出历史为 JSON（团队共享同一批测试用例） */
  const exportPreviewHistoryFile = useCallback((): void => {
    if (previewHistory.length === 0) {
      addToast(t('没有可导出的样例'), 'warning');
      return;
    }
    downloadText(
      `aigx-purge-samples-${new Date().toISOString().slice(0, 10)}.json`,
      exportPreviewHistory(previewHistory),
    );
    addToast(t('样例历史已导出'), 'success');
  }, [addToast, previewHistory, t]);

  /**
   * 导入样例历史（**合并**而非替换：团队共享时各自的样例都该留下）。
   *
   * 合并后按「新的在前、去重、截到 8 条」收敛——与 `writePreviewHistory`
   * 同一口径，避免「导入 20 条、刷新后只剩 8 条」的静默截断。
   */
  const importPreviewHistoryFile = useCallback(
    async (file: File | null): Promise<void> => {
      if (!file) return;
      try {
        const parsed = parsePreviewHistoryJson(await file.text());
        if (parsed === null) {
          addToast(t('导入失败：不是有效的样例历史文件'), 'error');
          return;
        }
        if (parsed.length === 0) {
          addToast(t('导入失败：文件里没有可用样例'), 'error');
          return;
        }
        setPreviewHistory((prev) => {
          let next = prev;
          // 逐条走 writePreviewHistory：它已实现「去重置顶 + 截断到 8 条」
          for (const sample of parsed) {
            next = writePreviewHistory(next, sample, lsSetStable);
          }
          persistPreviewHistory(next, lsSetStable, lsRemoveStable);
          return next;
        });
        addToast(t('已合并 {{count}} 条样例（上限 8 条，新的在前）', { count: parsed.length }), 'success');
      } catch {
        addToast(t('导入失败：无法读取文件'), 'error');
      }
    },
    [addToast, t],
  );
  const [previewing, setPreviewing] = useState(false);
  const [previewMeta, setPreviewMeta] = useState<{ changed: boolean; skill_hit: boolean } | null>(null);
  /**
   * 预览命中明细（后端 `AppliedDetail`，snake_case 序列化）。旧版后端不返回
   * `detail` 字段 → 保持 null，明细区不渲染（向前兼容）。
   */
  const [previewDetail, setPreviewDetail] = useState<PurgeAppliedDetail | null>(null);
  // 配置体检告警（结构化优先；旧后端只有字符串 → code 为空只读展示） + 技能快照
  const [configWarnings, setConfigWarnings] = useState<string[]>([]);
  /** CSV 导入模式：false = 追加（默认），true = 整表替换 */
  const [csvReplace, setCsvReplace] = useState(false);
  const [configWarningItems, setConfigWarningItems] = useState<PurgeConfigWarning[]>([]);
  const [skills, setSkills] = useState<NonNullable<PurgeData['skills']>>([]);
  /** 技能目录展开后的绝对路径（旧后端不返回 → 空串，不渲染那一行） */
  const [skillDirResolved, setSkillDirResolved] = useState('');
  /** 管理面 403「必须先改密码」：整页给引导，而不是只说「加载失败」 */
  const [needsPasswordChange, setNeedsPasswordChange] = useState(false);
  /** 批量粘贴面板：null=关，否则是哪张词表（就地展开，不引弹窗栈） */
  const [batchPaste, setBatchPaste] = useState<'dead_zone' | 'synonym' | null>(null);
  const [batchText, setBatchText] = useState('');
  /** 上次未保存的草稿备份（与服务器配置不同时才出现） */
  const [draftBackup, setDraftBackup] = useState<DraftBackup | null>(null);
  /**
   * 本会话内「上次保存成功」的时刻。
   *
   * 为什么只记内存、不落 localStorage：它回答的是「我这一次改完存了没有」，
   * 而不是「这个配置历史上被谁改过」——后者后端没存审计，前端编一个时间戳
   * 反而会误导。刷新后从空白开始是对的（那时用户该看的是服务器上的值）。
   */
  const [savedAt, setSavedAt] = useState<number | null>(null);
  /** 与出厂默认的差异（null=没比过；空数组=完全一致） */
  const [defaultsDiff, setDefaultsDiff] = useState<SettingsDiffItem[] | null>(null);
  /** 词表搜索关键词（两张表各一份；只影响显示，不参与保存） */
  const [deadZoneQuery, setDeadZoneQuery] = useState('');
  const [synonymQuery, setSynonymQuery] = useState('');
  /** 「只看会被整理删掉的行」开关（两张表各一份） */
  const [deadZoneIssuesOnly, setDeadZoneIssuesOnly] = useState(false);
  const [synonymIssuesOnly, setSynonymIssuesOnly] = useState(false);
  /**
   * 词表「撤销上一次批量变更」的快照。
   *
   * 只记**一次**：整理/导入/批量粘贴都可能一把改动几十行，用户想退回原来是
   * 逐行删——那种痛感远超「只能退一步」。同一次操作内重复变更会覆盖快照，
   * 语义仍然是「撤销我刚才那一下」。
   */
  const [tableUndo, setTableUndo] = useState<{
    kind: 'dead_zone' | 'synonym';
    rules: PurgeRewriteRule[];
    label: string;
  } | null>(null);
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
  /** dirty / save 的实时快照：Ctrl+S 监听只挂一次，不能依赖会变的闭包 */
  const dirtyRef = useRef(false);
  const saveRef = useRef<() => Promise<void>>(async () => {});
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
  // 「整理会删掉哪些行」——与 tidyRules 同判据，供筛选预览
  const deadZoneIssues = useMemo(
    () => new Set(ruleIssues(draft?.rewrite_rules ?? []).map((item) => item.index)),
    [draft?.rewrite_rules],
  );
  const synonymIssues = useMemo(
    () => new Set(ruleIssues(draft?.synonym_pairs ?? []).map((item) => item.index)),
    [draft?.synonym_pairs],
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

  // Ctrl/Cmd+S 保存（破甲页最常用的收尾动作；不拦浏览器默认会弹「另存为」）
  useEffect(() => {
    const handler = (event: KeyboardEvent): void => {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== 's') return;
      // 未改动时放行默认行为：让用户还能用浏览器自带的页面保存
      if (!dirtyRef.current) return;
      event.preventDefault();
      void saveRef.current();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);


  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const res = await api.getPurge();
      if (!res.success || !res.data) throw new Error('load failed');
      setData(res.data);
      setDraft(normalizeSettingsArrays(res.data.settings));
      setNumRaw(numRawOf(res.data.settings));
      setNumErrors({});
      setConfigWarnings(res.data.config_warnings ?? []);
      setConfigWarningItems(res.data.config_warning_items ?? []);
      setSkills(res.data.skills ?? []);
      setSkillDirResolved(res.data.skill_dir_resolved ?? '');
      // 有未保存备份且与服务器配置不同 → 让用户决定是否恢复（不静默套用，
      // 也不静默丢弃：两种都会让「我上次改的东西哪去了」无从回答）
      const backup = readDraftBackup((k) => window.localStorage.getItem(k));
      if (backup && !settingsEqual(backup.settings, res.data.settings)) {
        setDraftBackup(backup);
      } else {
        setDraftBackup(null);
      }
      setDirty(false);
    } catch (error) {
      // 首次登录未改密时，管理面一律 403 PASSWORD_CHANGE_REQUIRED（不是 401）。
      // 直接报「加载失败」会让人以为接口坏了——这里显式指路。
      const err = error as { status?: number; message?: string } | undefined;
      const mustChange =
        err?.status === 403 &&
        (localStorage.getItem('must_change_password') === '1' ||
          /password/i.test(err?.message ?? ''));
      if (mustChange) {
        setNeedsPasswordChange(true);
        addToast(t('首次登录需先修改密码，之后才能使用破甲控制台'), 'warning');
      } else {
        addToast(t('加载失败'), 'error');
      }
    } finally {
      setLoading(false);
    }
  }, [addToast, t]);

  useEffect(() => {
    void load();
  }, [load]);

  /** 底部保存：校验数字 → 夹取到合法区间 → PUT 整份草稿 */
  /** 编辑中把草稿写进本地备份（dirty 才写；保存成功后清掉） */
  useEffect(() => {
    if (!draft) return;
    if (!dirty) return;
    const timer = window.setTimeout(() => {
      writeDraftBackup(draft, lsSetStable, lsRemoveStable);
    }, 800);
    return () => window.clearTimeout(timer);
  }, [draft, dirty]);

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
    // 超上限**保存前必须确认**：后端 normalize 会静默截断，只靠输入框下的红字提示
    // 用户照样点保存、然后发现尾巴没了。集中汇总所有会被截断的字段，一次问清。
    const overflows = collectCapOverflows(current);
    // 空原文行会被后端静默剔除：一起问，别让用户保存完才发现少了一行
    const empties = countEmptyFromRules(current);
    const emptyParts = [
      empties.deadZone > 0
        ? t('死区词改写 {{n}} 行未填原文', { n: empties.deadZone })
        : '',
      empties.synonym > 0 ? t('同义词 {{n}} 行未填原文', { n: empties.synonym }) : '',
    ].filter(Boolean);
    if (overflows.length > 0 || emptyParts.length > 0) {
      const capDetail = overflows
        .map((item) =>
          t('{{label}} {{n}} 项（将丢弃 {{drop}} 项）', {
            label: t(item.label),
            n: item.n,
            drop: item.n - item.cap,
          }),
        )
        .join('；');
      const detail = [capDetail, ...emptyParts].filter(Boolean).join('；');
      const question =
        overflows.length > 0
          ? t('{{detail}}。保存只保留各字段的上限、并丢弃空原文行，继续？', { detail })
          : t('{{detail}}，保存时这些行会被丢弃。继续？', { detail });
      if (!window.confirm(question)) {
        return;
      }
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
      setDraft(normalizeSettingsArrays(res.data.settings));
      setNumRaw(numRawOf(res.data.settings));
      setNumErrors({});
      setDirty(false);
      // 保存成功后旧的词表撤销快照就过期了：它指向的是**已落库之前**的状态，
      // 再点撤销等于把已保存的结果改回去（还不会自动落库），语义上不是「撤销
      // 我刚才那一下」，而是「悄悄改回去」。宁可让入口消失。
      setTableUndo(null);
      // 落库成功 → 草稿备份没有存在意义了（留着反而下次弹「恢复」误导）
      writeDraftBackup(null, lsSetStable, lsRemoveStable);
      setSavedAt(Date.now());
      addToast(t('已保存'), 'success');
    } catch {
      addToast(t('保存失败'), 'error');
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  }, [addToast, dupTemplates.size, numRaw, t]);

  /**
   * 重置为默认：拉出厂默认配置填进**草稿**（不直接保存）。
   *
   * 为什么走服务端取默认值而不是前端硬编码：默认值是有语义的（总开关默认开、
   * 子开关默认组合、词表初始内容），前端硬编码会与后端 `PurgeSettings::default()`
   * 漂移；服务端还做了 normalize，与「全新安装首次保存」结果一致。
   */
  const resetToDefaults = useCallback(async (): Promise<void> => {
    if (!window.confirm(t('将把出厂默认配置填入草稿（未保存的编辑会丢失）。继续？'))) return;
    try {
      const res = await api.getPurgeDefaults();
      if (!res.success || !res.data) throw new Error('defaults failed');
      setDraft(normalizeSettingsArrays(res.data.settings));
      setNumRaw(numRawOf(res.data.settings));
      setNumErrors({});
      setDirty(true);
      setPreviewOut(null);
      setPreviewBase(null);
      setPreviewMeta(null);
      setPreviewDetail(null);
      addToast(t('已填入出厂默认配置，确认无误后点「保存」生效'), 'success');
    } catch {
      addToast(t('读取出厂默认配置失败'), 'error');
    }
  }, [addToast, t]);

  // 把最新的 dirty / save 交给 Ctrl+S 监听（监听只挂一次，读 ref 拿实时值）
  useEffect(() => {
    dirtyRef.current = dirty;
    saveRef.current = save;
  }, [dirty, save]);

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

  /**
   * 上移/下移一条规则。
   *
   * 为什么要可调序：改写**按表序执行**，重复 `from` 只有最靠前那条生效（`ruleRowHint`
   * 已经这么告诉用户了）。若没有调序入口，用户看到「只有最靠前的生效」却无从把想
   * 要的那条挪上去——提示就成了风凉话。
   */
  const moveRule = useCallback((index: number, delta: -1 | 1): void => {
    setDirty(true);
    setDraft((d) =>
      d
        ? {
            ...d,
            rewrite_rules: swapRules(d.rewrite_rules, index, delta),
          }
        : d,
    );
  }, []);

  const moveSynonymRule = useCallback((index: number, delta: -1 | 1): void => {
    setDirty(true);
    setDraft((d) =>
      d
        ? {
            ...d,
            synonym_pairs: swapRules(d.synonym_pairs, index, delta),
          }
        : d,
    );
  }, []);

  /** 批量停用异常行（不删：删是不可逆的，停用还能回填后再启用） */
  const disableIssues = useCallback(
    (kind: 'dead_zone' | 'synonym'): void => {
      const issues = kind === 'dead_zone' ? deadZoneIssues : synonymIssues;
      if (issues.size === 0) return;
      if (
        !window.confirm(
          t('将停用 {{n}} 条异常行（不删除，之后可再启用）。继续？', { n: issues.size }),
        )
      ) {
        return;
      }
      setDirty(true);
      setDraft((d) => {
        if (!d) return d;
        return kind === 'dead_zone'
          ? { ...d, rewrite_rules: disableRulesAt(d.rewrite_rules, issues) }
          : { ...d, synonym_pairs: disableRulesAt(d.synonym_pairs, issues) };
      });
      addToast(t('已停用 {{n}} 条异常行', { n: issues.size }), 'success');
    },
    [addToast, deadZoneIssues, synonymIssues, t],
  );

  /** 撤销上一次词表批量变更（整理 / 批量粘贴 / 导入 CSV 共用一条撤销） */
  const undoTableChange = useCallback((): void => {
    const snapshot = tableUndo;
    if (!snapshot) return;
    setDirty(true);
    setDraft((d) => (d ? applyUndo(d, snapshot) : d));
    setTableUndo(null);
    addToast(
      t('已撤销「{{label}}」，词表恢复为变更前的 {{n}} 行', {
        label: snapshot.label,
        n: snapshot.rules.length,
      }),
      'success',
    );
  }, [addToast, t, tableUndo]);

  /** 批量粘贴：解析后追加到对应词表（去重、跳过坏行并如实回报） */
  const applyBatchPaste = useCallback((): void => {
    const current = draft;
    if (!batchPaste || !current) return;
    const table: 'rewrite_rules' | 'synonym_pairs' =
      batchPaste === 'dead_zone' ? 'rewrite_rules' : 'synonym_pairs';
    const existing: PurgeRewriteRule[] =
      table === 'rewrite_rules' ? current.rewrite_rules : current.synonym_pairs;
    // 注意：**不要**在 `setDraft(updater)` 里收集计数——React 的 updater 是异步
    // 执行的，toast 会在计数还是 0 的时候弹出去（实测会报「没有可添加」，而规则
    // 其实已经加进去了）。计数必须在这里同步算完。
    const { rules, skipped } = parseBatchRules(batchText, existing);
    if (rules.length === 0) {
      addToast(
        skipped > 0
          ? t('没有解析出规则：{{skipped}} 行格式不对', { skipped })
          : t('没有可添加的规则（可能都与现有行重复）'),
        'error',
      );
      return;
    }
    setDirty(true);
    setTableUndo(
      undoSnapshot(
        batchPaste === 'dead_zone' ? 'dead_zone' : 'synonym',
        existing,
        t('批量粘贴'),
      ),
    );
    setDraft(
      table === 'rewrite_rules'
        ? { ...current, rewrite_rules: [...existing, ...rules] }
        : { ...current, synonym_pairs: [...existing, ...rules] },
    );
    addToast(
      skipped > 0
        ? t('已添加 {{n}} 条，{{skipped}} 行未能解析', { n: rules.length, skipped })
        : t('已添加 {{n}} 条规则', { n: rules.length }),
      'success',
    );
    setBatchPaste(null);
    setBatchText('');
  }, [addToast, batchPaste, batchText, draft, t]);

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
      const res = await api.getPurge({ skills: 'fresh' });
      if (res.success && res.data) {
        setSkills(res.data.skills ?? []);
        setSkillDirResolved(res.data.skill_dir_resolved ?? '');
      }
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
  const runPreview = useCallback(
    async (inputOverride?: string): Promise<void> => {
    const current = draftRef.current;
    if (!current || previewing) return;
    // 「按已保存配置预览」：草稿有未保存编辑时，用它区分「线上现在会怎样」
    // 与「保存后会怎样」——两者输出不同正是排障要看的对比。
    const saved = dataRef.current?.settings;
    const usingSaved = previewSavedOnly && saved;
    const settings = usingSaved ? saved : current;
    // 单条词表规则的「试」按钮传 override：setState 是异步的，直接用 state
    // 会拿到上一次的样例（跑出来的结果对不上点的那一行）。
    const input = inputOverride ?? previewInput;
    const parsed = parsePreviewMessages(input);
    if (parsed.length === 0) {
      addToast(t('请输入预览样例'), 'warning');
      return;
    }
    setPreviewing(true);
    try {
      const res = await api.previewPurge({
        settings,
        messages: parsed,
        // 空串不传：后端按纯治理处理（旧行为）
        model: previewModel.trim() || undefined,
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
      if (input.length <= PREVIEW_HISTORY_ITEM_MAX) {
        setPreviewHistory((prev) =>
          writePreviewHistory(prev, input, lsSetStable),
        );
      }
    } catch {
      // 失败要清掉上一次的结果区：否则界面留着旧 diff，而输入框已经是
      // 新内容——看上去像「这个新输入预览出来是这样」，属误导。
      setPreviewOut(null);
      setPreviewBase(null);
      setPreviewMeta(null);
      setPreviewDetail(null);
      addToast(t('预览失败（上一次结果已清空，避免与新输入混淆）'), 'error');
    } finally {
      setPreviewing(false);
    }
    },
    [addToast, previewInput, previewModel, previewing, previewSavedOnly, t],
  );

  /**
   * 「试」单条词表规则：用它自己的原文做预览样例并立刻跑一次。
   *
   * 为什么直接跑而不是只填样例：用户点「试」就是想立刻看到「这条到底命中没
   * 命中」（表序、停用、链式改写都会影响结果），再让他手动点一次预览是多余
   * 的一步。override 参数是为了避开 setState 异步——见 runPreview 的注释。
   */
  const probeRule = useCallback(
    (rule: PurgeRewriteRule): void => {
      const sample = skillKeywordSample(rule.from);
      if (!sample) return;
      setPreviewInput(sample);
      focusFixTarget('preview');
      void runPreview(sample);
    },
    [runPreview],
  );

  /**
   * 整理词表（删空 from / 删空 to / 原文去重）。
   *
   * 空 `to` 的语义是「删掉这个词」，容易被当成「还没填」留在表里；空 `from`
   * 后端 normalize 直接丢。整理前先报出各类条数让用户确认，整理后如实回报。
   */
  const tidyTable = useCallback(
    (kind: 'dead_zone' | 'synonym'): void => {
      const current = draftRef.current;
      if (!current) return;
      const rules = kind === 'dead_zone' ? current.rewrite_rules : current.synonym_pairs;
      const result = tidyRules(rules);
      const summary = tidySummary(result);
      if (summary.removed === 0) {
        addToast(t('词表已经干净，没有可整理的行'), 'success');
        return;
      }
      const detail = [
        summary.parts.emptyFrom > 0 ? t('原文为空 {{n}} 条', { n: summary.parts.emptyFrom }) : '',
        summary.parts.emptyTo > 0 ? t('改写为空 {{n}} 条', { n: summary.parts.emptyTo }) : '',
        summary.parts.duplicate > 0 ? t('原文重复 {{n}} 条', { n: summary.parts.duplicate }) : '',
      ]
        .filter(Boolean)
        .join('、');
      if (
        !window.confirm(
          t('将删除 {{n}} 行（{{detail}}），剩余 {{kept}} 行。继续？', {
            n: summary.removed,
            detail,
            kept: result.kept.length,
          }),
        )
      ) {
        return;
      }
      setDirty(true);
      setTableUndo(undoSnapshot(kind, rules, t('整理词表')));
      setDraft((d) =>
        d
          ? {
              ...d,
              rewrite_rules: kind === 'dead_zone' ? result.kept : d.rewrite_rules,
              synonym_pairs: kind === 'synonym' ? result.kept : d.synonym_pairs,
            }
          : d,
      );
      addToast(
        t('已整理：删除 {{n}} 行，剩余 {{kept}} 行（{{detail}}）', {
          n: summary.removed,
          kept: result.kept.length,
          detail,
        }),
        'success',
      );
    },
    [addToast, t],
  );

  // ── 词表 CSV 导入导出（死区词/同义词单表，进草稿不直接保存）──────────────
  //
  // 两列 `from,to`（表头可选）；RFC 4180 转义复用 toCsv / downloadText。

  /** 复制整张词表为 Tab 分隔文本（可直接粘进 Excel 或另一张表的「批量粘贴」） */
  const copyRulesTable = useCallback(
    async (kind: 'dead_zone' | 'synonym'): Promise<void> => {
      const draftNow = draftRef.current;
      if (!draftNow) return;
      const rules = kind === 'dead_zone' ? draftNow.rewrite_rules : draftNow.synonym_pairs;
      if (rules.length === 0) {
        addToast(t('词表为空，无可复制内容'), 'warning');
        return;
      }
      const copied = await copyText(rulesToText(rules));
      addToast(
        copied
          ? t('已复制 {{n}} 行（Tab 分隔，可直接粘进 Excel）', { n: rules.length })
          : t('复制失败（浏览器可能禁用了剪贴板权限）'),
        copied ? 'success' : 'error',
      );
    },
    [addToast, t],
  );

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
   * 导出四档统计（宽表 CSV）。
   *
   * 走后端 /api/purge/stats.csv 而不是在前端拼字符串：窗口档依赖服务端
   * 事件环（前端只有当前档的快照），只有后端能一次给全四档。
   */
  /**
   * 与出厂默认比对当前草稿。
   *
   * 场景：接手别人的破甲配置时，「有改动」没有信息量——要知道**偏离默认的是哪
   * 几项**，才能判断哪些是刻意调过的、哪些是忘在那儿的。
   *
   * 用草稿而不是已保存值：用户此刻看到的就是草稿，比的是「眼前这套 vs 默认」。
   */
  const compareDefaults = useCallback(async (): Promise<void> => {
    const current = draftRef.current;
    if (!current) return;
    try {
      const res = await api.getPurgeDefaults();
      if (!res.success || !res.data?.settings) throw new Error('defaults failed');
      const items = diffSettings(res.data.settings, current);
      setDefaultsDiff(items);
    } catch {
      addToast(t('读取出厂默认失败'), 'error');
    }
  }, [addToast, t]);

  /** 复制破甲现状快照（交接/报障：运行 + 生效项 + 体检 + 统计一次带走） */
  const copySnapshot = useCallback(async (): Promise<void> => {
    const current = draftRef.current;
    if (!current || !data) return;
    const warnings =
      configWarningItems.length > 0
        ? sortWarningsBySeverity(
            configWarningItems.map((w) => ({
              message: w.message,
              severity: w.severity ?? 'error',
            })),
          )
        : configWarnings.map((message) => ({ message, severity: 'error' }));
    const text = snapshotReportText({
      runtime: data.runtime,
      activeItems: buildActiveSummary(current).map((item) => t(item.key, item.params)),
      warnings,
      // 用 `data.stats`（全程累计）而不是页面上被窗口切换后的 `stats`：
      // 快照要的是「这段基线以来一共怎样」，跟当前视图口径无关
      stats: data.stats as unknown as Record<string, number>,
      baselineAt: data.stats_since,
      translate: (key) => t(key),
    });
    const copied = await copyText(text);
    addToast(
      copied ? t('已复现现状快照') : t('复制失败（浏览器可能禁用了剪贴板权限）'),
      copied ? 'success' : 'error',
    );
  }, [addToast, configWarningItems, configWarnings, data, t]);

  /** 复制体检结果为纯文本（工单证据：出了什么问题、按级分组） */
  const copyWarningReport = useCallback(async (): Promise<void> => {
    const items =
      configWarningItems.length > 0
        ? sortWarningsBySeverity(
            configWarningItems.map((w) => ({
              message: w.message,
              severity: w.severity ?? 'error',
            })),
          )
        : configWarnings.map((message) => ({ message, severity: 'error' }));
    const copied = await copyText(warningReportText(items, (key) => t(key)));
    addToast(
      copied ? t('已复制体检结果') : t('复制失败（浏览器可能禁用了剪贴板权限）'),
      copied ? 'success' : 'error',
    );
  }, [addToast, configWarningItems, configWarnings, t]);

  /** 复制生效项为纯文本（贴工单/群聊时不用手动抄） */
  const copyActiveSummary = useCallback(async (): Promise<void> => {
    const current = draftRef.current;
    if (!current) return;
    const text = activeSummaryText(buildActiveSummary(current), (key, params) => t(key, params));
    const copied = await copyText(text);
    addToast(
      copied ? t('已复制生效项清单') : t('复制失败（浏览器可能禁用了剪贴板权限）'),
      copied ? 'success' : 'error',
    );
  }, [addToast, t]);

  /**
   * 清零统计：把此刻作为新的观察基线。
   *
   * 为什么需要：累计值是进程级的，改完配置想验证「生效后有没有效果」时，旧历史
   * 混在里面比不出来；等重启清零不现实。只清计数，不动配置、不动「上次运行」快照。
   */
  const resetStats = useCallback(async (): Promise<void> => {
    if (!window.confirm(t('清零后累计统计与时间窗计数都会归零（不影响配置）。继续？'))) return;
    try {
      const res = await api.resetPurgeStats();
      if (!res.success) throw new Error('reset failed');
      addToast(t('统计已清零，之后的数据从零开始累计'), 'success');
      // 重新拉一次整页快照：stats / stats_1h / stats_24h 都要跟着变
      await load();
    } catch {
      addToast(t('统计清零失败'), 'error');
    }
  }, [addToast, load, t]);

  const exportStatsCsv = useCallback(async (): Promise<void> => {
    try {
      const res = await fetch(`${API_BASE}/purge/stats.csv`, { headers: authHeaders() });
      if (!res.ok) throw new Error(`status ${res.status}`);
      const text = await res.text();
      downloadText(
        `aigx-purge-stats-${new Date().toISOString().slice(0, 10)}.csv`,
        text,
      );
      addToast(t('统计已导出为 CSV'), 'success');
      // 有下界档要说清楚哪几档不准：CSV 里 lower_bound 只是 0/1，运维不知道它是
      // 「下界标记」，会把「近 1h 拒答 3 次」当精确值。事件环覆盖后只会少算。
      const lowerBound = statsExportNote([
        { label: t('近 1 小时'), truncated: dataRef.current?.stats_1h?.truncated },
        { label: t('近 24 小时'), truncated: dataRef.current?.stats_24h?.truncated },
      ]);
      if (lowerBound) {
        addToast(
          t('{{windows}} 是下界估计（事件环被覆盖），lower_bound=1 表示该档只会少算', {
            windows: lowerBound,
          }),
          'warning',
        );
      }
    } catch {
      addToast(t('统计导出失败'), 'error');
    }
  }, [addToast, t]);

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
        const currentRules =
          kind === 'dead_zone' ? existing?.rewrite_rules ?? [] : existing?.synonym_pairs ?? [];
        const existingFroms = new Set(
          currentRules.map((r) => r.from.trim().toLowerCase()).filter(Boolean),
        );
        const incomingFroms = parsed.map((r) => r.from.trim().toLowerCase());
        const dupCount = incomingFroms.filter((f) => existingFroms.has(f)).length;

        // 替换模式：整表换成文件内容（导出→Excel 改→导回的标准动作）。
        // 与追加模式的判据不同——「全部重复」在这里恰恰是正常情况，不能拒收。
        if (csvReplace) {
          if (currentRules.length > 0) {
            const ok = window.confirm(
              t('将用文件里的 {{next}} 条替换当前 {{prev}} 条（含未保存的编辑）。继续？', {
                next: parsed.length,
                prev: currentRules.length,
              }),
            );
            if (!ok) return;
          }
          setDirty(true);
          setTableUndo(undoSnapshot(kind, currentRules, t('导入 CSV（替换）')));
          setDraft((d) =>
            d
              ? {
                  ...d,
                  rewrite_rules: kind === 'dead_zone' ? parsed : d.rewrite_rules,
                  synonym_pairs: kind === 'synonym' ? parsed : d.synonym_pairs,
                }
              : d,
          );
          const replacedBase =
            kind === 'dead_zone'
              ? t('已用 {{count}} 条替换死区词表，检查无误后点「保存」生效', {
                  count: parsed.length,
                })
              : t('已用 {{count}} 条替换同义词表，检查无误后点「保存」生效', {
                  count: parsed.length,
                });
          const overCap =
            parsed.length > RULE_TABLE_CAP
              ? ` · ${t('超过上限 {{cap}} 条：保存只保留前 {{cap}} 条', { cap: RULE_TABLE_CAP })}`
              : '';
          addToast(replacedBase + overCap, overCap ? 'warning' : 'success');
          return;
        }

        if (dupCount === parsed.length) {
          // 一条都不新：大概率导错了表或旧文件——直接不追加，给出口
          addToast(t('导入的 {{count}} 条规则的原文全部与当前词表重复，未追加（可能导错了文件？）', { count: parsed.length }), 'warning');
          return;
        }
        setDirty(true);
        setTableUndo(undoSnapshot(kind, currentRules, t('导入 CSV（追加）')));
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
    [addToast, csvReplace, t],
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
      const parsed = JSON.parse(text) as { version?: number; settings?: unknown };
      const verdict = validateImportedSettings(parsed?.settings);
      if (verdict.fatal.length > 0) {
        // 结构坏了导不进来（渲染期会白屏）；逐条点名，不让用户猜哪里错
        addToast(`${t('导入失败：配置结构有问题')} — ${verdict.fatal.join('；')}`, 'error');
        return;
      }
      const settings = parsed.settings as PurgeSettings;
      setDraft(normalizeSettingsArrays(settings));
      setNumRaw(numRawOf(settings));
      setNumErrors({});
      setDirty(true);
      setPreviewOut(null);
      setPreviewBase(null);
      setPreviewMeta(null);
      setPreviewDetail(null);
      if (verdict.warnings.length > 0) {
        // 能渲染但语义可疑：导入照做，把风险一次说全（保存时才 clamp 的话用户看不到）
        addToast(
          `${t('配置已导入草稿，但有 {{n}} 处需要确认', { n: verdict.warnings.length })} — ${verdict.warnings.join('；')}`,
          'warning',
        );
      } else {
        addToast(t('配置已导入草稿，检查无误后点「保存」生效'), 'success');
      }
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

  /**
   * 跳到下一条重复行（循环）。
   *
   * 为什么需要：「只看异常」筛出来之后，重复行可能有几十条，逐条滚动找**当前这条
   * 的下一处**很费劲；重复又恰恰是最需要对照着改的（要看清楚前一条写的是什么）。
   *
   * 过滤中不启用：过滤后列表里没有相邻行，跳过去也看不到上下文。
   */
  const jumpNextDuplicate = useCallback(
    (kind: 'dead_zone' | 'synonym'): void => {
      const rules = draftRef.current
        ? kind === 'dead_zone'
          ? draftRef.current.rewrite_rules
          : draftRef.current.synonym_pairs
        : [];
      const dupIndices = ruleIssues(rules)
        .filter((item) => item.reason === 'duplicate')
        .map((item) => item.index);
      if (dupIndices.length === 0) {
        addToast(t('没有重复行'), 'success');
        return;
      }
      const container = kind === 'dead_zone' ? rulesRef.current : synRulesRef.current;
      const currentIdx = Number(container?.dataset.jumpCursor ?? -1);
      const next =
        dupIndices.find((index) => index > currentIdx) ?? dupIndices[0];
      if (container) container.dataset.jumpCursor = String(next);
      const row = container?.querySelector<HTMLElement>(`[data-rule-index="${next}"]`);
      row?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      row?.querySelector<HTMLInputElement>('input')?.focus();
      row?.classList.add('purge-rule-flash');
      window.setTimeout(() => row?.classList.remove('purge-rule-flash'), 1200);
      addToast(t('第 {{n}} 条（共 {{total}} 处重复）', { n: next + 1, total: dupIndices.length }), 'success');
    },
    [addToast, t],
  );

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

  /**
   * 失焦收敛：把超范围/小数输入就地修正并**说明原因**。
   *
   * 为什么不在 onChange 里收敛：用户打「20」时中途是「2」，边打边被 clamp 会把
   * 输入打断。失焦是「我填完了」的信号，这时修正才不会碍事。
   */
  const handleNumBlur = useCallback(
    (field: { key: NumKey; label: string; min: number; max: number }): void => {
      const raw = numRaw[field.key];
      const { corrected, notice } = clampNumInput(field, raw);
      if (corrected === '') return;
      if (notice) {
        setNumRaw((prev) => ({ ...prev, [field.key]: corrected }));
        setDirty(true);
        setDraft((d) => (d ? withNum(d, field.key, notice.value) : d));
        addToast(
          notice.kind === 'rounded'
            ? t('{{label}} 只接受整数，已取整为 {{value}}', { label: t(field.label), value: notice.value })
            : t('{{label}} 超出 {{min}}–{{max}} 范围，已调整为 {{value}}', {
                label: t(field.label),
                min: field.min,
                max: field.max,
                value: notice.value,
              }),
          'warning',
        );
      }
    },
    [addToast, numRaw, t],
  );

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
          {needsPasswordChange ? (
            <>
              <p className="purge-empty" data-testid="purge-need-password-change">
                {t('首次登录必须修改密码后才能使用管理面（后端返回 403）。请到「个人资料」修改密码，然后回到本页重试。')}
              </p>
              <Button variant="primary" onClick={() => void load()}>
                {t('已改好，重试')}
              </Button>
            </>
          ) : (
            <>
              <p className="purge-empty">{t('网络异常或服务不可用，请稍后重试')}</p>
              <Button variant="primary" onClick={() => void load()}>
                {t('重试')}
              </Button>
            </>
          )}
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
  // 缺 `stats` 时用零值兜底：这里崩的话区块边界也接不住（异常在边界父层）
  const stats = windowed ?? data.stats ?? EMPTY_STATS;
  // 字段形状异常（如后端把 stats 返成字符串）时给 null，卡片显示 `—`
  const retryRate = retryRateText(stats.responses, stats.retries);
  const windowLabel =
    statsWindow === '1h'
      ? t('近 1 小时')
      : statsWindow === '24h'
        ? t('近 24 小时')
        : statsWindow === 'last'
          ? t('上次运行')
          : undefined;
  // `runtime` 也在派生阶段被解构（uptime / header 的 PID）——同样在边界之外
  const runtime = data.runtime ?? EMPTY_RUNTIME;
  // 只读参数区读的是**服务端快照**（不读草稿）；这里崩同样在边界之外。
  // 注意：ErrorBoundary 只能捕获「子组件渲染时」的异常，**捕获不到「父组件构造
  // children 时」的异常** —— `<ParamGrid inflight={data.runtime.inflight} />`
  // 的 prop 求值发生在当前组件的渲染里，包多少层 guard 都没用。
  const savedSettings = data.settings ?? EMPTY_SETTINGS_SNAPSHOT;
  const savedFallbackModels = Array.isArray(savedSettings.fallback_models)
    ? savedSettings.fallback_models
    : [];
  const uptime = formatUptime(runtime.uptime_secs, {
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
          PID {runtime.pid} · {uptime} · {t('上游')}{' '}
          {runtime.upstream || '—'}{' '}
          {/* 存了还是没存：dirty 只说「有未保存修改」，不说「上次存于何时」 */}
          {savedAt ? (
            <span className="purge-saved-at" data-testid="purge-saved-at">
              {t('上次保存：{{time}}', { time: new Date(savedAt).toLocaleTimeString() })} ·{' '}
            </span>
          ) : null}
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
          {/* 现状快照：交接/报障要全景（运行 + 生效项 + 体检 + 统计） */}
          <Button
            variant="outline"
            size="sm"
            data-testid="purge-snapshot-copy"
            onClick={() => void copySnapshot()}
          >
            {t('复制现状快照')}
          </Button>
          {/* Ctrl/Cmd+S 一直可用但界面零提示——能力藏着等于没有。
              放在 header（始终渲染），不要放进「有未保存改动才出现」的警示条 */}
          <kbd className="purge-kbd-hint" data-testid="purge-shortcut-hint">
            {t('Ctrl/⌘ + S 保存')}
          </kbd>
        </div>
        {/* 未保存草稿备份：刷新/关页丢掉改动的唯一补救入口 */}
      {draftBackup ? (
        <div className="purge-warning-box" role="alert" data-testid="purge-draft-backup">
          <p className="purge-warning-row">
            <span>
              {t('检测到未保存的草稿（{{time}}），与当前服务器配置不同。要恢复吗？', {
                time: new Date(draftBackup.savedAt).toLocaleString(),
              })}
            </span>
          </p>
          <div className="purge-actions">
            <Button
              variant="primary"
              size="sm"
              data-testid="purge-draft-backup-restore"
              onClick={() => {
                setDraft(normalizeSettingsArrays(draftBackup.settings));
                setNumRaw(numRawOf(draftBackup.settings));
                setDirty(true);
                setDraftBackup(null);
                addToast(t('已恢复未保存的草稿，记得点保存'), 'success');
              }}
            >
              {t('恢复草稿')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              data-testid="purge-draft-backup-discard"
              onClick={() => {
                writeDraftBackup(null, lsSetStable, lsRemoveStable);
                setDraftBackup(null);
                addToast(t('已丢弃未保存的草稿'));
              }}
            >
              {t('丢弃')}
            </Button>
          </div>
        </div>
      ) : null}
      {/* 总开关关闭提示：放在页面标题下方独立成行（请求不会被改写这件事必须一眼看到，
            只靠开关行的颜色区分太弱） */}
        {!draft.enabled ? (
          <p className="purge-off-notice" role="status" data-testid="purge-off-notice">
            {t('破甲控制台当前关闭，请求不会被改写。')}
            {/* 不禁用下面的开关：常见流程是「先配好、最后打开总开关」。
                但必须说清「现在配了也不会生效」，否则用户会以为已经生效。 */}
            <span className="purge-off-hint">
              {t('下面的开关与词表可以先配好，但只有打开总开关后才会真正生效。')}
            </span>
          </p>
        ) : null}
        {/* 作用域说明：网关只有一份全局配置（`PurgeStore.current`），没有按账号/按
            密钥的隔离。新接手的人最容易误判成「只影响我这个号」，从而以为可以随便
            试——这句话必须在改第一个开关之前就能看到。 */}
        <p className="purge-scope-note" data-testid="purge-scope-note">
          {t(
            '这份配置是全局的：对所有账号、所有上游请求一律生效，没有按用户或按密钥的隔离。改动保存后立即影响线上流量。',
          )}
        </p>
        {/* 与出厂默认的差异：接手别人配置时，「哪几项偏离默认」才是要看的 */}
        {defaultsDiff ? (
          <div className="purge-warning-box" role="status" data-testid="purge-defaults-diff">
            <p className="purge-warning-row">
              <span>
                {defaultsDiff.length === 0
                  ? t('当前配置与出厂默认完全一致。')
                  : t('相对出厂默认改了 {{n}} 项：', { n: defaultsDiff.length })}
              </span>
            </p>
            {defaultsDiff.length > 0 ? (
              <ul className="purge-active-list">
                {defaultsDiff.map((item, index) => (
                  <li key={`${item.key}-${index}`}>{t(item.key, item.params)}</li>
                ))}
              </ul>
            ) : null}
            <div className="purge-actions">
              <Button
                variant="outline"
                size="sm"
                data-testid="purge-defaults-diff-close"
                onClick={() => setDefaultsDiff(null)}
              >
                {t('收起')}
              </Button>
            </div>
          </div>
        ) : null}
      </div>

      {/* 未保存编辑警示条：离开保护之外的页内显性提示，提供「放弃修改」出口 */}
      {dirty && (
        <div className="purge-dirty-bar" role="status">
          <span>{t('有未保存的修改')}</span>
          {/* 改了什么：dirty 只说明「有改动」，这里列出来（换人接手/隔天回来看） */}
          {savedForDiff ? (
            <span className="purge-dirty-changes" data-testid="purge-dirty-changes">
              {diffSettings(savedForDiff, draft)
                .slice(0, 6)
                .map((item) => t(item.key, item.params))
                .join(' · ')}
              {diffSettings(savedForDiff, draft).length > 6
                ? ` · ${t('另有 {{n}} 项', { n: diffSettings(savedForDiff, draft).length - 6 })}`
                : ''}
            </span>
          ) : null}
          <Button
            variant="outline"
            size="sm"
            disabled={saving}
            onClick={() => {
              const snapshot = dataRef.current;
              const current = draftRef.current;
              if (!snapshot) return;
              // 确认里报**具体项数**：只说「要丢弃修改吗」，用户不知道丢多少
              const changed = current ? diffSettings(snapshot.settings, current) : [];
              if (changed.length > 0) {
                const preview = changed
                  .slice(0, 5)
                  .map((item) => t(item.key, item.params))
                  .join('、');
                const more = changed.length > 5 ? ` 等 ${changed.length} 项` : '';
                if (
                  !window.confirm(
                    t('将丢弃 {{n}} 项未保存的修改（{{preview}}{{more}}），恢复为服务器上的配置。继续？', {
                      n: changed.length,
                      preview,
                      more,
                    }),
                  )
                ) {
                  return;
                }
              }
              setDraft(normalizeSettingsArrays(snapshot.settings));
              setNumRaw(numRawOf(snapshot.settings));
              setNumErrors({});
              setDirty(false);
              setTableUndo(null);
              setPreviewOut(null);
              setPreviewBase(null);
              setPreviewMeta(null);
              setPreviewDetail(null);
            }}
          >
            {t('放弃修改')}
          </Button>
          {/* 与出厂默认比：接手别人的配置时，光看「有改动」不够——改了什么才是问题 */}
          <Button
            variant="outline"
            size="sm"
            data-testid="purge-compare-defaults"
            onClick={() => void compareDefaults()}
          >
            {t('对比出厂默认')}
          </Button>
          {/* 撤销上一次词表批量变更：整理/粘贴/导入都可能一把改几十行 */}
          {tableUndo ? (
            <Button
              variant="outline"
              size="sm"
              data-testid="purge-table-undo"
              onClick={undoTableChange}
            >
              {t('撤销「{{label}}」', { label: tableUndo.label })}
            </Button>
          ) : null}
          {/* 重置为默认：只填草稿（用户确认后再点保存），不直接写库 */}
          <Button
            variant="outline"
            size="sm"
            disabled={saving}
            onClick={() => void resetToDefaults()}
          >
            {t('重置为默认')}
          </Button>
        </div>
      )}

      {/* 页内导航：十几个屏的页面，跳转比滚轮快 */}
      <PageNav t={t} />

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
        {/* 导出四档统计（宽表 CSV）：与页面同一份数据，直接落盘进报表 */}
        <Button
          variant="outline"
          size="sm"
          data-testid="purge-stats-export"
          onClick={() => void exportStatsCsv()}
        >
          {t('导出统计 CSV')}
        </Button>
        {/* 清零基线：改完配置想看「生效后有没有效果」，旧历史混在累计值里比不出来 */}
        <Button
          variant="outline"
          size="sm"
          data-testid="purge-stats-reset"
          disabled={saving}
          onClick={() => void resetStats()}
        >
          {t('清零统计')}
        </Button>
      </div>

      <span data-purge-fix="stats" />
      {/* 基线时刻：清零后「全程」不再是「进程启动至今」，写准免得误读 */}
      {data.stats_since ? (
        <p className="purge-hint" data-testid="purge-stats-since">
          {t('统计基线：{{time}}', {
            time: new Date(data.stats_since * 1000).toLocaleString(),
          })}
        </p>
      ) : null}
      {/* 「上次运行」档：不说清是哪一次，一排数字等于没意义 */}
      {statsWindow === 'last' ? (
        <p className="purge-hint" data-testid="purge-last-run-note">
          {lastRunNote(data.stats_last_run, (key, params) => t(key, params))}
        </p>
      ) : null}
      <SectionGuard title={t('统计')}>
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
        {/* 失败分类条与统计卡读的是同一个 `stats`：同一个边界内，否则它会在
            guard 之外解构 null（实测整页崩在这里） */}
        <FailureBar
          refusal={stats.failure_refusal}
          empty={stats.failure_empty}
          filter={stats.failure_filter}
          other={stats.failure_other}
          t={t}
        />
      </SectionGuard>

      {/* 配置体检告警：开了但不生效 / 配置自相矛盾（静态可判定项）。
          结构化告警带 code → 「去修复」滚动并聚焦对应控件；旧后端纯文本。
          全部通过时给一行绿字：让运维确认「体检确实跑过且当前无问题」，
          而不是面对一片空白猜后端到底检查了什么。 */}
      {configWarningItems.length > 0 || configWarnings.length > 0 ? (
        <div className="purge-warning-box" role="alert">
          {(configWarningItems.length > 0
            ? sortWarningsBySeverity(
                configWarningItems.map((w) => ({
                  code: w.code,
                  message: w.message,
                  severity: w.severity ?? 'error',
                })),
              )
            : configWarnings.map((message) => ({
                code: '',
                message,
                severity: 'error' as const,
              }))
          ).map(({ code, message, severity }, index) => (
            <p key={index} className={`purge-warning-row is-${severity}`}>
              <span className="purge-warning-badge">{t(SEVERITY_LABEL[severity])}</span>
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
      {/* 体检结果可复制（工单证据）；无告警时也给出「已通过」结论而不是空串 */}
      {draft.enabled ? (
        <div className="purge-actions">
          <Button
            variant="outline"
            size="sm"
            data-testid="purge-warning-copy"
            onClick={() => void copyWarningReport()}
          >
            {t('复制体检结果')}
          </Button>
        </div>
      ) : null}

      {/* 当前生效项：8 开关 + 两张词表 + 模板/预设/技能，配到十几项后没人能一眼
          说出「现在到底哪些在生效」；这里按数据面真实语义汇总（词表只算启用行）。 */}
      <span data-purge-fix="active-summary" />
      <Card
        title={t('当前生效项')}
        actions={
          <Button
            variant="outline"
            size="sm"
            data-testid="purge-active-copy"
            onClick={() => void copyActiveSummary()}
          >
            {t('复制为文本')}
          </Button>
        }
      >
        <ul className="purge-active-list" data-testid="purge-active-summary">
          {buildActiveSummary(draft).map((item, index) => (
            <li key={`${item.key}-${index}`}>
              {item.anchor ? (
                // 点了跳到对应配置区（复用体检告警的锚点与闪烁提示）
                <button
                  type="button"
                  className="purge-active-jump"
                  title={t('跳到这一项对应的配置')}
                  onClick={() => focusFixTarget(item.anchor as string)}
                >
                  {t(item.key, item.params)}
                </button>
              ) : (
                t(item.key, item.params)
              )}
            </li>
          ))}
        </ul>
      </Card>

      <Card
        title={t('开关')}
        actions={<span className="purge-hint">{t('点一下立即生效，重启后保留')}</span>}
      >
        <span data-purge-fix="switches" />
        <SwitchRows flags={flags} disabled={saving} onToggle={toggle} t={t} />
      </Card>
      <SectionGuard title={t('只读参数')}>
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
          retryLimit={savedSettings.retry_limit}
          failureRefusal={stats.failure_refusal}
          failureEmpty={stats.failure_empty}
          failureFilter={stats.failure_filter}
          failureOther={stats.failure_other}
          fallbackModels={savedFallbackModels.join(', ')}
          fallbackTriggerAttempt={savedSettings.fallback_trigger_attempt}
          skillInjectLimit={savedSettings.skill_inject_limit}
          inflight={runtime.inflight}
          upstream={runtime.upstream}
          t={t}
        />
      </Card>
      </SectionGuard>

      <SectionGuard title={t('死区词改写词表')}>
      <Card
        title={t('死区词改写词表')}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={addRule}>
              {t('添加规则')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              data-testid="purge-batch-paste-open"
              onClick={() => setBatchPaste('dead_zone')}
            >
              {t('批量粘贴')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => tidyTable('dead_zone')}>
              {t('整理')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => exportRulesCsv('dead_zone')}>
              {t('导出 CSV')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              data-testid="purge-copy-rules-dead-zone"
              onClick={() => void copyRulesTable('dead_zone')}
            >
              {t('复制整表')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => deadZoneCsvRef.current?.click()}>
              {t('导入 CSV')}
            </Button>
            <label className="purge-rule-toggle" title={t('勾选后导入会整表替换，而不是追加')}>
              <input
                type="checkbox"
                checked={csvReplace}
                onChange={(event) => setCsvReplace(event.target.checked)}
              />
              <span className="purge-hint">{t('替换')}</span>
            </label>
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
        {/* 词表搜索：50+ 条时找一条要滚很久；过滤只影响显示，编辑仍写真实下标 */}
        {draft.rewrite_rules.length > 3 ? (
          <div className="purge-rule-search">
            <Input
              aria-label={t('搜索死区词表')}
              placeholder={t('搜索原文或改写')}
              value={deadZoneQuery}
              onChange={(event) => setDeadZoneQuery(event.target.value)}
            />
            {deadZoneQuery.trim() ? (
              <span className="purge-hint">
                {t('匹配 {{n}} / {{total}} 条', {
                  n: filterRules(draft.rewrite_rules, deadZoneQuery).length,
                  total: draft.rewrite_rules.length,
                })}
              </span>
            ) : null}
            {/* 「整理」会直接删，先让人看清删哪些再动手 */}
            <label className="purge-issues-only">
              <input
                type="checkbox"
                data-testid="purge-dead-zone-issues-only"
                checked={deadZoneIssuesOnly}
                onChange={(event) => setDeadZoneIssuesOnly(event.target.checked)}
              />
              {t('只看异常 {{n}} 条', { n: deadZoneIssues.size })}
            </label>
            {deadZoneIssues.size > 0 ? (
              <>
                <Button
                  variant="outline"
                  size="sm"
                  data-testid="purge-dead-zone-disable-issues"
                  onClick={() => disableIssues('dead_zone')}
                >
                  {t('批量停用异常行')}
                </Button>
                {deadZoneQuery.trim() || deadZoneIssuesOnly ? null : (
                  <Button
                    variant="outline"
                    size="sm"
                    data-testid="purge-dead-zone-next-dup"
                    onClick={() => jumpNextDuplicate('dead_zone')}
                  >
                    {t('跳到下一个重复')}
                  </Button>
                )}
              </>
            ) : null}
          </div>
        ) : null}
        {batchPaste === 'dead_zone' ? (
          <div className="purge-warning-box" role="group" data-testid="purge-batch-paste-panel">
            <p className="purge-warning-row">
              <span>
                {t('一行一条，原文与改写之间用 Tab 或 -> 分隔（从 Excel 直接粘贴两列即可）')}
              </span>
            </p>
            <textarea
              className="purge-batch-text"
              aria-label={t('批量粘贴内容')}
              rows={6}
              value={batchText}
              onChange={(event) => setBatchText(event.target.value)}
              placeholder={'原文\t改写'}
            />
            <div className="purge-actions">
              <Button variant="primary" size="sm" onClick={applyBatchPaste}>
                {t('解析并添加')}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  setBatchPaste(null);
                  setBatchText('');
                }}
              >
                {t('取消')}
              </Button>
            </div>
          </div>
        ) : null}
        {draft.rewrite_rules.length === 0 ? (
          <p className="purge-empty" data-purge-fix="dead-zone-table">{t('暂无改写规则，点击右上角「添加规则」新建一条')}</p>
        ) : (
          <div className="purge-rules" ref={rulesRef} data-purge-fix="dead-zone-table">
            {filterRules(draft.rewrite_rules, deadZoneQuery)
              .filter(({ index }) => !deadZoneIssuesOnly || deadZoneIssues.has(index))
              .map(({ rule, index }) => (
              <RuleRow
                key={index}
                rule={rule}
                index={index}
                fromLabel={t('原文')}
                toLabel={t('改写为')}
                removeLabel={t('删除')}
                enableLabel={t('启用该条改写规则')}
                probeLabel={t('试')}
                onProbe={probeRule}
                onMove={moveRule}
                canMoveUp={index > 0 && !deadZoneQuery.trim()}
                canMoveDown={index < draft.rewrite_rules.length - 1 && !deadZoneQuery.trim()}
                moveUpLabel={
                  deadZoneQuery.trim()
                    ? t('搜索过滤中不能调序（交换的是原表里的相邻行）——先清空搜索')
                    : t('上移（改写按表序执行，越靠前越先命中）')
                }
                moveDownLabel={
                  deadZoneQuery.trim()
                    ? t('搜索过滤中不能调序（交换的是原表里的相邻行）——先清空搜索')
                    : t('下移')
                }
                dupHint={ruleRowHint(
                  rule,
                  Boolean(dupRules.get(rule.from.trim().toLowerCase())),
                  (key) => t(key),
                )}
                onPatch={patchRule}
                onRemove={removeRule}
              />
            ))}
          </div>
        )}
      </Card>
      </SectionGuard>

      <SectionGuard title={t('同义词表')}>
      <Card
        title={t('同义词表')}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={addSynonymRule}>
              {t('添加规则')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              data-testid="purge-batch-paste-open-synonym"
              onClick={() => setBatchPaste('synonym')}
            >
              {t('批量粘贴')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => tidyTable('synonym')}>
              {t('整理')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => exportRulesCsv('synonym')}>
              {t('导出 CSV')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              data-testid="purge-copy-rules-synonym"
              onClick={() => void copyRulesTable('synonym')}
            >
              {t('复制整表')}
            </Button>
            <Button variant="outline" size="sm" onClick={() => synonymCsvRef.current?.click()}>
              {t('导入 CSV')}
            </Button>
            <label className="purge-rule-toggle" title={t('勾选后导入会整表替换，而不是追加')}>
              <input
                type="checkbox"
                checked={csvReplace}
                onChange={(event) => setCsvReplace(event.target.checked)}
              />
              <span className="purge-hint">{t('替换')}</span>
            </label>
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
        {draft.synonym_pairs.length > 3 ? (
          <div className="purge-rule-search">
            <Input
              aria-label={t('搜索同义词表')}
              placeholder={t('搜索原文或改写')}
              value={synonymQuery}
              onChange={(event) => setSynonymQuery(event.target.value)}
            />
            {synonymQuery.trim() ? (
              <span className="purge-hint">
                {t('匹配 {{n}} / {{total}} 条', {
                  n: filterRules(draft.synonym_pairs, synonymQuery).length,
                  total: draft.synonym_pairs.length,
                })}
              </span>
            ) : null}
            <label className="purge-issues-only">
              <input
                type="checkbox"
                data-testid="purge-synonym-issues-only"
                checked={synonymIssuesOnly}
                onChange={(event) => setSynonymIssuesOnly(event.target.checked)}
              />
              {t('只看异常 {{n}} 条', { n: synonymIssues.size })}
            </label>
            {synonymIssues.size > 0 ? (
              <>
                <Button
                  variant="outline"
                  size="sm"
                  data-testid="purge-synonym-disable-issues"
                  onClick={() => disableIssues('synonym')}
                >
                  {t('批量停用异常行')}
                </Button>
                {synonymQuery.trim() || synonymIssuesOnly ? null : (
                  <Button
                    variant="outline"
                    size="sm"
                    data-testid="purge-synonym-next-dup"
                    onClick={() => jumpNextDuplicate('synonym')}
                  >
                    {t('跳到下一个重复')}
                  </Button>
                )}
              </>
            ) : null}
          </div>
        ) : null}
        {draft.synonym_pairs.length === 0 ? (
          <p className="purge-empty">{t('暂无同义词，点击右上角「添加规则」新建一条')}</p>
        ) : (
          <div className="purge-rules" ref={synRulesRef} data-purge-fix="synonym-table">
            {filterRules(draft.synonym_pairs, synonymQuery)
              .filter(({ index }) => !synonymIssuesOnly || synonymIssues.has(index))
              .map(({ rule, index }) => (
              <RuleRow
                key={index}
                rule={rule}
                index={index}
                fromLabel={t('敏感词')}
                toLabel={t('同义词')}
                removeLabel={t('删除')}
                enableLabel={t('启用该条改写规则')}
                probeLabel={t('试')}
                onProbe={probeRule}
                onMove={moveSynonymRule}
                canMoveUp={index > 0 && !synonymQuery.trim()}
                canMoveDown={index < draft.synonym_pairs.length - 1 && !synonymQuery.trim()}
                moveUpLabel={
                  synonymQuery.trim()
                    ? t('搜索过滤中不能调序（交换的是原表里的相邻行）——先清空搜索')
                    : t('上移（改写按表序执行，越靠前越先命中）')
                }
                moveDownLabel={
                  synonymQuery.trim()
                    ? t('搜索过滤中不能调序（交换的是原表里的相邻行）——先清空搜索')
                    : t('下移')
                }
                dupHint={ruleRowHint(
                  rule,
                  Boolean(dupSynonyms.get(rule.from.trim().toLowerCase())),
                  (key) => t(key),
                )}
                onPatch={patchSynonymRule}
                onRemove={removeSynonymRule}
              />
            ))}
          </div>
        )}
      </Card>
      </SectionGuard>

      <SectionGuard title={t('技能与结构化提示')}>
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
            onBlur={() => handleNumBlur(NUM_FIELDS[0])}
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
            onBlur={() => handleNumBlur(NUM_FIELDS[1])}
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
            onBlur={() => handleNumBlur(NUM_FIELDS[2])}
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
      </SectionGuard>

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

      {/* 技能扫描快照：让 skill_inject 配置可验证（目录里到底扫到了什么）。
          关键词可点：填进预览样例并滚到预览区——直接验证「这个词会不会命中」。
          **空目录也渲染**：以前 `skills.length > 0` 才显示，用户把 SKILL.md 放进
          目录却没看到卡片，会以为「扫描成功但没内容」；现在给空态文案 + 重新扫描。 */}
      <Card
        title={t('技能扫描快照')}
        actions={
          <Button variant="outline" size="sm" onClick={() => void refreshSkills()}>
            {t('重新扫描')}
          </Button>
        }
      >
        {/* 扫的是哪：`~` 的展开依赖服务进程 HOME，用户放错目录时靠这行定位 */}
        {skillDirResolved ? (
          <p className="purge-hint" data-testid="purge-skill-dir-resolved">
            {t('实际扫描目录：{{dir}}', { dir: skillDirResolved })}
          </p>
        ) : null}
        {skills.length === 0 ? (
          <p className="purge-empty" data-purge-fix="skill-snapshot">
            {t('当前技能目录扫不到任何 SKILL.md（技能注入不会生效）。放好文件后点「重新扫描」。')}
          </p>
        ) : (
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
                <span className="purge-skill-keywords">
                  {skill.keywords.slice(0, 6).map((keyword) => (
                    <button
                      key={`${skill.name}-${keyword}`}
                      type="button"
                      className="purge-skill-keyword"
                      title={t('用「{{keyword}}」做预览样例', { keyword })}
                      onClick={() => {
                        setPreviewInput(skillKeywordSample(keyword));
                        focusFixTarget('preview');
                      }}
                    >
                      {keyword}
                    </button>
                  ))}
                  {skill.keywords.length > 6 ? (
                    <span className="purge-skill-more">
                      {t('另有 {{n}} 个', { n: skill.keywords.length - 6 })}
                    </span>
                  ) : null}
                </span>
              </li>
            ))}
          </ul>
        )}
      </Card>

      {/* 预设库：内置 prompt 集合浏览 + 一键落盘（放在结构化模板之上，应用后直接在下方看到结果） */}
      <span data-purge-fix="presets" />
      {/* 区块级隔离：预设库解析的是「内置目录 + 后端匹配」两套数据，出错不该拖垮
          开关和保存（用户可能正急着落库） */}
      <SectionGuard title={t('破甲预设库')}>
        <PresetsSection
        t={t}
        addToast={addToast}
        onApplyTemplate={applyPresetTemplate}
        currentTemplates={draft?.structured_templates}
        onSkillWritten={() => void refreshSkills()}
        />
      </SectionGuard>

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

      <Card
        title={t('改写预览')}
        actions={
          // 预览在最底部、摘要在最上面：改完词表想确认「现在到底哪些在生效」要滚十几屏
          <Button
            variant="outline"
            size="sm"
            data-testid="purge-jump-active-summary"
            onClick={() => focusFixTarget('active-summary')}
          >
            {t('回看当前生效项')}
          </Button>
        }
      >
        <span data-purge-fix="preview" />
        {/* 样例历史：点回填；右侧「删除 ×」单条移除，「清空」整表清掉。
            样例可能含敏感正文，历史必须可删（localStorage 会跨会话留存）。 */}
        {previewHistory.length > 0 && (
          <div className="purge-window-picker" data-testid="purge-preview-history" role="group" aria-label={t('最近样例')}>
            {previewHistory.map((sample, index) => (
              <span key={`${index}-${sample.slice(0, 12)}`} className="purge-history-item">
                <button
                  type="button"
                  className={`purge-preset-chip${sample === previewInput ? ' is-active' : ''}`}
                  title={sample.slice(0, 200)}
                  onClick={() => setPreviewInput(sample)}
                >
                  {previewHistoryLabel(sample)}
                </button>
                <button
                  type="button"
                  className="purge-history-remove"
                  aria-label={t('删除这条样例')}
                  title={t('删除这条样例')}
                  onClick={() => removePreviewHistory(index)}
                >
                  ×
                </button>
              </span>
            ))}
            <button
              type="button"
              className="purge-history-clear"
              onClick={() => clearPreviewHistory()}
            >
              {t('清空历史')}
            </button>
            <button
              type="button"
              className="purge-history-clear"
              onClick={() => exportPreviewHistoryFile()}
            >
              {t('导出样例')}
            </button>
            <button
              type="button"
              className="purge-history-clear"
              onClick={() => previewHistoryFileRef.current?.click()}
            >
              {t('导入样例')}
            </button>
            <input
              ref={previewHistoryFileRef}
              type="file"
              accept=".json,application/json"
              style={{ display: 'none' }}
              onChange={(event) => {
                void importPreviewHistoryFile(event.target.files?.[0] ?? null);
                event.target.value = '';
              }}
            />
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
          {/* 模型：非空则一并预演「按模型自动套用」的预设前插（与数据面同链） */}
          <label className="purge-rule-toggle" title={t('填写模型名可预演「按模型自动套用」的预设前插')}>
            <span className="purge-hint">{t('模型')}</span>
            <input
              type="text"
              className="form-input"
              style={{ width: 180, marginLeft: 6 }}
              placeholder={t('留空则不套预设')}
              value={previewModel}
              disabled={previewing}
              onChange={(event) => setPreviewModel(event.target.value)}
            />
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
                  previewDetail.auto_preset_applied ? t('已套用模型预设') : '',
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
            <label className="purge-rule-toggle" title={t('长样例里上下文行会遮住真正的改写点')}>
              <input
                type="checkbox"
                data-testid="purge-diff-changed-only"
                checked={diffChangedOnly}
                onChange={(event) => setDiffChangedOnly(event.target.checked)}
              />
              <span className="purge-hint">{t('只看变更行')}</span>
            </label>
            {previewBase !== null && previewBase !== previewOut ? (
              <div className="purge-preview-diff" data-testid="purge-preview-diff" aria-label={t('改写结果')}>
                {(() => {
                  const rows = diffLines(previewBase, previewOut);
                  // 「只看变更」：上下文行在长样例里占绝大多数，遮蔽真正的改写点
                  const shown = diffChangedOnly ? rows.filter((r) => r.kind !== 'ctx') : rows;
                  const hidden = rows.length - shown.length;
                  return (
                    <>
                      {hidden > 0 ? (
                        <div className="pdl-hidden-hint">
                          {t('已隐藏 {{n}} 行未变更内容', { n: hidden })}
                        </div>
                      ) : null}
                      {shown.map((row, index) => {
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
                    </>
                  );
                })()}
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
