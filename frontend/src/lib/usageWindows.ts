// beta.7 对齐：Usage 配额按基础模型合并思考等级。
//
// 上游 `ui-shared/usage-readings.ts::usageModelQuotasOf` 把同一模型在不同思考
// 等级（low / medium / high / xhigh / minimal…）下的配额行合并成一条基础模型
// 读数，保留「最紧的剩余比例」与「有效重置时间」。AIGX 的等价数据结构是
// `AccountBalance.windows`（`AccountUsageWindow[]`，见 types/index.ts），这里
// 用同样的口径实现纯函数，供账号池余额格与余额明细弹层复用。
//
// 设计约束：
//   · 只有「模型标识确实带思考等级后缀」的行才会被合并；普通时间窗口（5h /
//     weekly / monthly / daily）没有等级后缀，原样透传，行为不变。
//   · 合并结果沿用首次出现的顺序，避免列表跳动。

import type { AccountUsageWindow } from '../types';

/** 连字符编号后缀：`gpt-5-high`、`claude_xhigh`…（含 max 兜底） */
const EFFORT_SUFFIX_ID =
  /[-_](?:extra[-_]?low|xlow|minimal|xhigh|medium|high|low|max)$/i;
/** 空格分词后缀：`gpt-5 high`… */
const EFFORT_SUFFIX_WORD =
  /\s+(?:extra[- ]?low|xlow|minimal|xhigh|medium|high|low|max)$/i;
/** 括号后缀：`gpt-5 (high)`、`gpt-5（低）`… */
const EFFORT_PAREN =
  /\s*[（(]\s*(?:extra[- ]?low|xlow|minimal|xhigh|medium|high|low|max)\s*[)）]$/i;

/** 去掉名称末尾的思考等级标记，返回基础名与是否发生过剥离。 */
function stripEffort(raw: string): { base: string; stripped: boolean } {
  const value = raw.trim();
  if (!value) return { base: '', stripped: false };
  let next = value.replace(EFFORT_PAREN, '').trimEnd();
  if (next !== value && next) return { base: next, stripped: true };
  next = value.replace(EFFORT_SUFFIX_WORD, '').trimEnd();
  if (next !== value && next) return { base: next, stripped: true };
  next = value.replace(EFFORT_SUFFIX_ID, '').replace(/[-_\s]+$/, '');
  if (next !== value && next) return { base: next, stripped: true };
  return { base: value, stripped: false };
}

/** 归一化数字读数（非有限数视为缺省）。 */
function numberOrNull(value: number | null | undefined): number | null {
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

/** 有效重置时刻（unix 秒；0 / 负数 / 非数视为无效）。 */
function validReset(value: number | null | undefined): number | null {
  const at = numberOrNull(value);
  return at !== null && at > 0 ? at : null;
}

/** 剩余百分比（0-100，缺省返回 null）。 */
function percentOf(w: AccountUsageWindow): number | null {
  const at = numberOrNull(w.remaining_percent);
  return at === null ? null : Math.max(0, Math.min(100, at));
}

/**
 * 判断 `incoming` 是否比 `existing` 更「紧」，需取代其读数。
 *
 * 口径与上游 `preferIncomingModelQuota` 一致：先比剩余百分比（越小越紧），
 * 再比有无窗口代码，最后比重置时间（越早越紧）。
 */
function isIncomingTighter(existing: AccountUsageWindow, incoming: AccountUsageWindow): boolean {
  const ep = percentOf(existing);
  const ip = percentOf(incoming);
  if (ep === null && ip !== null) return true;
  if (ep !== null && ip === null) return false;
  if (ep !== null && ip !== null) {
    if (ip + 1e-6 < ep) return true;
    if (Math.abs(ip - ep) > 1e-6) return false;
  }
  const ew = Boolean((existing.window ?? '').trim());
  const iw = Boolean((incoming.window ?? '').trim());
  if (!ew && iw) return true;
  if (ew && !iw) return false;
  const er = validReset(existing.resets_at);
  const ir = validReset(incoming.resets_at);
  if (er === null && ir !== null) return true;
  if (er !== null && ir !== null) return ir < er;
  return false;
}

/** 计算一行的合并标识；无法识别模型时返回 null（不参与合并）。 */
function identityOf(w: AccountUsageWindow): { key: string; stripped: boolean } | null {
  const label = stripEffort(w.label ?? w.key ?? '');
  const group = stripEffort(w.group ?? '');
  if (!label.base && !group.base) return null;
  const key = `${group.base.toLowerCase()}::${label.base.toLowerCase()}::${(w.window ?? '').trim().toLowerCase()}`;
  return { key, stripped: label.stripped || group.stripped };
}

/** 合并两行：以更紧的一行为主，缺失的重置时间 / 剩余绝对值从另一行补齐。 */
function mergeTwo(existing: AccountUsageWindow, incoming: AccountUsageWindow): AccountUsageWindow {
  const winner = isIncomingTighter(existing, incoming) ? incoming : existing;
  const loser = winner === existing ? incoming : existing;
  const winnerLabel = stripEffort(winner.label ?? '');
  const winnerGroup = stripEffort(winner.group ?? '');
  return {
    ...winner,
    label: winnerLabel.stripped ? winnerLabel.base : winner.label,
    group: winnerGroup.stripped ? winnerGroup.base : winner.group,
    remaining_percent: percentOf(winner),
    resets_at: validReset(winner.resets_at) ?? validReset(loser.resets_at),
    remaining: numberOrNull(winner.remaining) ?? numberOrNull(loser.remaining),
    limit: numberOrNull(winner.limit) ?? numberOrNull(loser.limit),
    // 明确「不限」只在没有百分比读数时保留；否则以百分比读数（更紧）为准。
    unlimited: winner.unlimited === true && percentOf(winner) === null ? true : winner.unlimited,
  };
}

/**
 * 把同一基础模型的不同思考等级窗口合并为单条读数。
 *
 * 剩余比例取最紧（最小），重置时间取更早的有效值；没有等级后缀的窗口原样透传。
 * 返回新数组，不修改入参。
 */
export function mergeUsageWindows(windows: readonly AccountUsageWindow[]): AccountUsageWindow[] {
  const order: string[] = [];
  const map = new Map<string, AccountUsageWindow>();
  let anonymous = 0;
  for (const w of windows) {
    const identity = identityOf(w);
    if (!identity) {
      // 无任何可识别标识：不合并，按出现顺序原样保留。
      const key = `__row_${anonymous++}`;
      order.push(key);
      map.set(key, w);
      continue;
    }
    const existing = map.get(identity.key);
    if (!existing) {
      map.set(identity.key, identity.stripped
        ? mergeTwo(w, w) // 统一剥掉后缀展示基础名
        : w);
      order.push(identity.key);
      continue;
    }
    map.set(identity.key, mergeTwo(existing, w));
  }
  return order.map((key) => map.get(key) as AccountUsageWindow);
}
