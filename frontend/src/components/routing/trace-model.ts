/**
 * 请求追踪 / 执行详情的判据层（移植自 A2A `ui-islands/routing-trace-model.ts`）。
 *
 * 为什么单独一个文件：请求追踪（列表 + 详情）、流向面板、特殊路由「最近命中」三处
 * 都要回答同一批问题 —— 「这次请求算成功还是失败」「它走到了第几次尝试」「耗时怎么读」。
 * 三处各写一份就会得到三套略微不同的判据（A2A 那边就是这么收口的），
 * 这里照同一形状收口，界面只做渲染。
 *
 * 与上游的差异（只有两处，都是宿主能力差）：
 *   · 上游 `traceProvider` 走 `host().wbProviders.labelOf` 把 id 翻成展示名；
 *     AIGX 的 trace 行自带 `providerLabel`，没有就直接显示 id，不做二次查表。
 *   · 上游 `formatTraceTokens` 走 `host().wbUnits.formatTokens`；AIGX 用 `toLocaleString`。
 */
import { useTranslation } from 'react-i18next';
import type { RoutingTraceRow } from '../../types/routing-workspace';

export type TraceState = 'running' | 'success' | 'failed' | 'unknown' | 'pending';

export type TraceAttempt = NonNullable<RoutingTraceRow['attemptDetails']>[number] & {
  index: number;
  /** 旧记录没存明细时按行摘要推断出来的那一条 */
  inferred?: boolean;
};

export type TraceCandidate = NonNullable<NonNullable<RoutingTraceRow['route']>['ordered']>[number] & {
  kind: 'available' | 'skipped';
};

/** 秒/毫秒自适应（<1e11 视为秒，与账号页 / 回放页同一口径）。 */
export function toMillis(v: number | undefined | null): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  return v < 1e11 ? v * 1000 : v;
}

export const requestKey = (row: RoutingTraceRow): string =>
  `${row.id}:${row.ts ?? ''}`;

export const sameRequest = (left: RoutingTraceRow, right: RoutingTraceRow): boolean =>
  left.id === right.id && left.ts === right.ts;

export function requestModel(row: RoutingTraceRow, fallback = ''): string {
  return row.clientModel || row.model || row.upstreamModel || fallback;
}

export const isTraceRunning = (row: RoutingTraceRow): boolean =>
  row.status === 0 && (row.error == null || row.error === '');

export const isTraceSuccess = (row: RoutingTraceRow): boolean =>
  row.status != null && row.status >= 200 && row.status < 300 && (row.error == null || row.error === '');

export const isTraceCancelled = (row: RoutingTraceRow): boolean =>
  (row.status === 499 || row.status === 408) &&
  (row.error === '客户端在响应完成前断开连接' || row.error === '请求已被手动终止');

/** 阶段文案（上游 `tracePhase` 的同口径）。 */
export function tracePhase(phase: string | undefined, t: (key: string) => string): string {
  return phase === 'waiting' ? t('等待响应')
    : phase === 'streaming' ? t('响应中')
      : phase === 'retrying' ? t('重试中')
        : phase === 'queued' ? t('排队中')
          : phase === 'connecting' ? t('连接中')
            : t('进行中');
}

export function requestStateLabel(row: RoutingTraceRow, t: (key: string) => string): string {
  return isTraceRunning(row) ? tracePhase(row.phase, t)
    : isTraceCancelled(row) ? t('已取消')
      : isTraceSuccess(row) ? t('成功')
        : t('失败');
}

export function traceProvider(
  id?: string | null,
  label?: string | null,
  t?: (key: string) => string,
): string {
  return label || id || (t ? t('提供商未记录') : '');
}

/** 逐次尝试：有明细用明细；没有但行上记了承载账号时补一条 inferred。 */
export function traceAttempts(row: RoutingTraceRow): TraceAttempt[] {
  const entries = Array.isArray(row.attemptDetails) ? row.attemptDetails : [];
  if (entries.length) return entries.map((entry, index) => ({ ...entry, index }));
  if (!row.provider && !row.accountId && !row.upstreamModel) return [];
  return [{
    index: Math.max(0, (row.attempts || 1) - 1),
    provider: row.provider,
    channel_name: row.providerLabel,
    model: row.upstreamModel,
    inferred: true,
  }];
}

/**
 * 候选队列快照。
 *
 * 已尝试的账号仍留在列表里（尝试明细单独记录）——否则单候选失败时会出现
 * 「1 个候选」但列表为空的自相矛盾。
 */
export function traceCandidates(row: RoutingTraceRow): TraceCandidate[] {
  const route = row.route;
  const blocked = Array.isArray(route?.blocked) ? route!.blocked! : [];
  const ordered = Array.isArray(route?.ordered) ? route!.ordered! : [];
  const seen = new Set<string>();
  return [
    ...ordered.map((entry) => ({ ...entry, kind: 'available' as const })),
    ...blocked.map((entry) => ({
      ...entry,
      kind: entry.state === 'tried' ? ('available' as const) : ('skipped' as const),
    })),
  ].filter((entry) => {
    const key = entry.id;
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

export function attemptState(row: RoutingTraceRow, attempt: TraceAttempt): TraceState {
  const entries = traceAttempts(row);
  const lastIndex = Math.max(...entries.map((item) => item.index));
  const last = attempt.index === lastIndex && (attempt.index + 1 >= (row.attempts || 1) || attempt.inferred);
  if (attempt.error || (attempt.status != null && (attempt.status < 200 || attempt.status >= 300))) return 'failed';
  if (last && isTraceRunning(row)) return 'running';
  if (last && !isTraceSuccess(row)) {
    return attempt.finishedAt != null ? 'failed' : 'unknown';
  }
  if (attempt.status != null || attempt.finishedAt != null || (last && isTraceSuccess(row))) return 'success';
  return 'unknown';
}

export function attemptLabel(
  row: RoutingTraceRow,
  attempt: TraceAttempt,
  t: (key: string) => string,
): string {
  const state = attemptState(row, attempt);
  if (state === 'running') return tracePhase(row.phase, t);
  if (state === 'failed') return t('尝试失败');
  if (state === 'success') {
    return attempt.index + 1 >= (row.attempts || 1) ? t('最终承载') : t('上游已响应');
  }
  return t('结果未记录');
}

/** 时长文案：<1s 用毫秒，<60s 用秒，再往上用 m+ss。 */
export function formatDuration(ms: number | undefined | null): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60_000);
  return `${m}m${String(Math.round((ms % 60_000) / 1000)).padStart(2, '0')}s`;
}

export function attemptDuration(
  row: RoutingTraceRow,
  attempt: TraceAttempt,
  t: (key: string) => string,
): string {
  if (attempt.startedAt != null && attempt.finishedAt != null) {
    return formatDuration(Math.max(0, (attempt.finishedAt - attempt.startedAt) * 1000));
  }
  if (attemptState(row, attempt) === 'running') return t('进行中');
  return t('耗时未记录');
}

/** 上游错误常带整段 HTML，摘要只留可读文字；完整原文在请求详情里。 */
export function errorText(value?: string | null, limit = 0): string {
  const plain = (value || '').replace(/<[^>]*>/g, ' ').replace(/\s+/g, ' ').trim();
  return limit > 0 && plain.length > limit ? `${plain.slice(0, limit)}…` : plain;
}

export function requestSummary(row: RoutingTraceRow, t: (key: string, vars?: Record<string, string | number>) => string): string {
  const switches = Math.max(0, (row.attempts || 1) - 1);
  if (isTraceRunning(row)) return t('请求仍在处理中，当前阶段：{{phase}}', { phase: tracePhase(row.phase, (k) => t(k)) });
  if (!isTraceSuccess(row)) return errorText(row.error, 180) || t('请求未成功完成');
  if (switches) {
    return t('经过 {{n}} 次切换后成功，最终承载：{{account}}', {
      n: switches,
      account: row.accountName || traceProvider(row.provider, row.providerLabel, (k) => t(k)),
    });
  }
  if (row.route?.stickyHit) return t('命中会话粘性，沿用原账号完成请求');
  return t('请求成功完成，未发生账号切换');
}

export function candidateReason(
  row: RoutingTraceRow,
  candidate: TraceCandidate,
  t: (key: string) => string,
): string {
  if (candidate.kind === 'skipped') return candidate.reason || t('当时被候选过滤规则跳过');
  if (isTraceRunning(row)) return t('当时可用，当前请求尚未轮到这个账号');
  if (isTraceSuccess(row)) return t('其他账号已完成请求，没有继续调用这个候选');
  return t('候选快照中有此账号，但保存的尝试明细未显示它被调用');
}

export const formatTraceTokens = (value?: number | null): string =>
  value == null ? '—' : value.toLocaleString();

/** 列表 / 详情的状态徽标色（对应 AIGX Badge 的 tone 取值）。 */
export function traceBadgeTone(state: TraceState): 'success' | 'danger' | 'info' | 'neutral' {
  return state === 'success' ? 'success'
    : state === 'failed' ? 'danger'
      : state === 'running' ? 'info'
        : 'neutral';
}

/** 绑好 `t` 的便利包装：界面里直接 `const m = useTraceModel()` 后调 `m.xxx`。 */
export function useTraceModel(): {
  t: (key: string, vars?: Record<string, string | number>) => string;
} {
  const { t } = useTranslation();
  return { t: t as (key: string, vars?: Record<string, string | number>) => string };
}
