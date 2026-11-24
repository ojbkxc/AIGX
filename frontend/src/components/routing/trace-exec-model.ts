/**
 * 请求追踪详情的执行模型（移植自 A2A `ui-islands/routing-exec-model.ts`）。
 *
 * 回答四个问题，全部是判据、不是渲染：
 *   1. `execCarrier`  —— 这次请求最终由谁承载、为什么是它；
 *   2. `execAccounts` —— 候选清单里每个账号在这一次里的角色（承载 / 尝试失败 / 已跳过 / 未调用）；
 *   3. `execTiming`   —— 逐次尝试的耗时分段（等待响应 → 等待首帧 → 输出），给条形图用；
 *   4. `execScopeLabel` —— 这次请求当时吃的是哪一层配置。
 *
 * ── 与上游的字段差（AIGX 后端当前不下发，统一降级为「未记录」而不是猜）─────
 *   · 上游 `candidateId` / `memberId`：一个账号可参加多个成员模型，靠它们区分；
 *     AIGX 的 attemptDetails 没有这两个字段，退回 `channel_id` / 名称匹配。
 *   · 上游 `attempt.retries` / `attempt.errorPolicy`：同账号内部重试与错误策略描述；
 *     AIGX 未记录，对应行整条不渲染。
 *   · 上游 `row.firstResponseMs` / `completionTokens` / `totalTokens`：首帧与 token 读数；
 *     AIGX 未记录，首帧段与 token 指标显示「—」。
 *   · 上游 `route.configSnapshot`（scope / groupId / groupName / strategy / sticky）：
 *     AIGX 的 trace 行只有 `route.strategy` / `route.sticky`，配置范围恒为「未记录」。
 *   · 上游候选条目的 `providerLabel` 与尝试明细的 `reason`：AIGX 的类型里没有，
 *     经 `candidateProviderLabel` / `attemptReason` 两个窄访问器读，缺就空串。
 * 这些缺口都在交付清单里点名，界面照实显示「未记录」，不填假数据。
 */
import {
  attemptState,
  candidateReason,
  errorText,
  formatDuration,
  isTraceCancelled,
  isTraceRunning,
  isTraceSuccess,
  toMillis,
  traceAttempts,
  traceCandidates,
  tracePhase,
  traceProvider,
  type TraceAttempt,
  type TraceCandidate,
} from './trace-model';
import type { RoutingTraceRow } from '../../types/routing-workspace';

type T = (key: string, vars?: Record<string, string | number>) => string;

const same = (a?: string | null, b?: string | null): boolean =>
  Boolean(a && b && a.toLowerCase() === b.toLowerCase());

/* ── 上游字段的窄访问器 ─────────────────────────────────────
 * A2A 的候选条目带 `providerLabel`、尝试明细带 `reason`；AIGX 的
 * `RoutingQueueEntryItem` / `attemptDetails` 没有这两个字段（后端未下发）。
 * 这里按「有就读、没有就空」读，界面据此显示「未记录」，绝不补默认值。 */
type Loose = Record<string, unknown>;
const loose = (value: unknown): Loose => (value && typeof value === 'object' ? (value as Loose) : {});
const str = (value: unknown): string | undefined =>
  typeof value === 'string' && value.trim() ? value : undefined;
export const candidateProviderLabel = (candidate?: TraceCandidate): string | undefined =>
  str(loose(candidate).providerLabel);
export const attemptReason = (attempt?: TraceAttempt): string | undefined =>
  str(loose(attempt).reason);

export const execCandidateKey = (candidate: TraceCandidate): string => candidate.id;

export function execAttemptName(attempt: TraceAttempt, t: T): string {
  return attempt.channel_name || attempt.channel_id || t('账号未记录');
}

/**
 * 尝试与候选的对应关系。
 *
 * 上游先比 `candidateId`、再比 `memberId`/`provider`/`model`，最后才退回账号 id 或名称；
 * AIGX 没有前两个标识，按「provider 对得上 ∧ 渠道 id 或名称对得上」判定，
 * 任一维度都缺时宁可判不匹配（不把 A 的尝试记到 B 头上）。
 */
export function execAttemptMatches(candidate: TraceCandidate, attempt: TraceAttempt): boolean {
  if (candidate.provider && attempt.provider && !same(candidate.provider, attempt.provider)) return false;
  if (candidate.name && attempt.channel_name && candidate.name !== attempt.channel_name) {
    // 名称不同但 id 相同仍算同一个（后端两种写法都出现过）
    return Boolean(attempt.channel_id && candidate.id === attempt.channel_id);
  }
  if (attempt.channel_id) return candidate.id === attempt.channel_id;
  return Boolean(candidate.name && attempt.channel_name && candidate.name === attempt.channel_name);
}

export function execLastAttempt(row: RoutingTraceRow, attempts = traceAttempts(row)): TraceAttempt | undefined {
  const last = attempts[attempts.length - 1];
  return last && (last.inferred || last.index + 1 >= (row.attempts || 1)) ? last : undefined;
}

export type ExecCarrier = {
  label: string;
  name: string;
  reason: string;
  carrying: boolean;
  candidate?: TraceCandidate;
  last?: TraceAttempt;
};

export function execCarrier(row: RoutingTraceRow, t: T): ExecCarrier {
  const attempts = traceAttempts(row);
  const candidates = traceCandidates(row);
  const last = execLastAttempt(row, attempts);
  const pickedId = row.accountId || row.route?.picked;
  const candidate = candidates.find((item) =>
    last ? execAttemptMatches(item, last) : Boolean(pickedId && item.id === pickedId));
  const running = isTraceRunning(row);
  const cancelled = isTraceCancelled(row);
  const success = isTraceSuccess(row);
  const name = row.accountName || last?.channel_name || candidate?.name || pickedId || last?.channel_id;
  const provider = traceProvider(
    row.provider || last?.provider || candidate?.provider,
    row.providerLabel || candidateProviderLabel(candidate),
    (k) => t(k),
  );
  const carrying = Boolean(
    name && (success || (running && (last ? attemptState(row, last) !== 'failed' : row.phase !== 'retrying'))),
  );
  let reason = errorText(attemptReason(last) || candidate?.reason)
    || (last?.stickyHit || row.route?.stickyHit ? t('命中会话粘滞') : t('选路原因未记录'));
  const previous = last && attempts.find((attempt) => attempt.index === last.index - 1);
  if (previous && attemptState(row, previous) === 'failed') {
    reason = `${t('{{account}} 尝试失败后切换到此账号', { account: execAttemptName(previous, t) })}；${reason}`;
  }
  if (cancelled) {
    return {
      label: t('本次结果'), name: t('请求已取消'),
      reason: errorText(row.error) || t('客户端已取消请求'), carrying: false, candidate, last,
    };
  }
  if (!running && !success) {
    return {
      label: t('本次结果'), name: t('请求未成功完成'),
      reason: errorText(row.error || last?.error || last?.notice) || t('失败原因未记录'),
      carrying: false, candidate, last,
    };
  }
  if (!carrying && success) {
    return {
      label: t('最终承载'), name: t('账号未记录'),
      reason: t('请求成功完成，未记录承载账号'), carrying: false, candidate, last,
    };
  }
  if (!carrying) {
    return {
      label: t('当前状态'), name: tracePhase(row.phase, (k) => t(k)),
      reason: last && attemptState(row, last) === 'failed'
        ? errorText(last.error || last.notice) || t('本轮尝试失败，正在退避等待或切换到下一个账号')
        : t('尚未记录承载账号'),
      carrying: false, candidate, last,
    };
  }
  return {
    label: running ? t('当前承载') : t('最终承载'),
    name: `${provider} · ${name}`,
    reason, carrying, candidate, last,
  };
}

export type ExecAccount = {
  key: string;
  name: string;
  provider: string;
  model?: string | null;
  current: boolean;
  kind: 'tried' | 'skipped' | 'idle';
  label: string;
  tone: 'accent' | 'fail' | 'muted';
  reason: string;
};

export function execAccounts(row: RoutingTraceRow, t: T): ExecAccount[] {
  const attempts = traceAttempts(row);
  const candidates = traceCandidates(row);
  const carrier = execCarrier(row, t);
  const fromAttempt = (attempt: TraceAttempt, candidate?: TraceCandidate): ExecAccount => {
    const current = carrier.carrying && (carrier.last
      ? carrier.last.index === attempt.index
      : Boolean(candidate && carrier.candidate && execCandidateKey(carrier.candidate) === execCandidateKey(candidate)));
    const cancelled = isTraceCancelled(row) && carrier.last?.index === attempt.index;
    const state = attemptState(row, attempt);
    return {
      key: candidate ? execCandidateKey(candidate) : `attempt:${attempt.index}`,
      name: candidate?.name || execAttemptName(attempt, t),
      provider: traceProvider(
        candidate?.provider || attempt.provider,
        candidateProviderLabel(candidate),
        (k) => t(k),
      ),
      model: attempt.model,
      current,
      kind: 'tried',
      label: cancelled ? t('已取消')
        : current ? (isTraceRunning(row) ? t('当前承载') : t('最终承载'))
          : state === 'failed' ? t('尝试失败')
            : state === 'success' ? t('上游已响应')
              : state === 'running' ? tracePhase(row.phase, (k) => t(k))
                : t('结果未记录'),
      tone: cancelled ? 'muted' : current ? 'accent' : state === 'failed' ? 'fail' : 'muted',
      reason: errorText(
        cancelled ? row.error
          : state === 'failed' ? attempt.error || attempt.notice
            : attemptReason(attempt) || candidate?.reason || attempt.notice,
      ) || (attempt.status != null ? `HTTP ${attempt.status}` : t('没有保存调用明细')),
    };
  };
  const entries = candidates.map((candidate): ExecAccount => {
    const attempt = [...attempts].reverse().find((item) => execAttemptMatches(candidate, item));
    if (attempt) return fromAttempt(attempt, candidate);
    const current = carrier.carrying && Boolean(carrier.candidate && execCandidateKey(carrier.candidate) === execCandidateKey(candidate));
    return {
      key: execCandidateKey(candidate),
      name: candidate.name || candidate.id || t('账号未记录'),
      provider: traceProvider(candidate.provider, candidateProviderLabel(candidate), (k) => t(k)),
      model: null,
      current,
      kind: current || candidate.state === 'tried' ? 'tried' : candidate.kind === 'skipped' ? 'skipped' : 'idle',
      label: current ? (isTraceRunning(row) ? t('当前承载') : t('最终承载'))
        : candidate.state === 'tried' ? t('已尝试')
          : candidate.kind === 'skipped' ? t('已跳过')
            : t('未调用'),
      tone: current ? 'accent' : 'muted',
      reason: current ? carrier.reason
        : candidate.state === 'tried' ? errorText(candidate.reason) || t('没有保存调用明细')
          : candidate.kind === 'skipped' ? errorText(candidateReason(row, candidate, (k) => t(k)))
            : [errorText(candidate.reason), candidateReason(row, candidate, (k) => t(k))].filter(Boolean).join('；'),
    };
  });
  const hasCurrent = entries.some((entry) => entry.current);
  const currentMatches = (attempt: TraceAttempt): boolean =>
    (row.accountId || row.route?.picked)
      ? attempt.channel_id === (row.accountId || row.route?.picked)
      : Boolean(row.accountName && attempt.channel_name === row.accountName);
  const current = !hasCurrent && (carrier.carrying || (!carrier.last && (row.accountId || row.accountName)))
    && !attempts.some(currentMatches) ? traceAttempts({ ...row, attemptDetails: [] }) : [];
  if (current.length) {
    const attempt = current[0];
    entries.push({
      ...fromAttempt(attempt),
      key: 'current-account',
      current: carrier.carrying,
      label: carrier.carrying ? (isTraceRunning(row) ? t('当前承载') : t('最终承载')) : t('结果未记录'),
      reason: carrier.carrying ? carrier.reason : t('没有保存调用明细'),
    });
  }
  const missing = attempts.filter((attempt) => !candidates.some((candidate) => execAttemptMatches(candidate, attempt)));
  return [...entries, ...missing.map((attempt) => fromAttempt(attempt))];
}

export type ExecSegment = { start: number; end: number; tone: 'wait' | 'frame' | 'stream' | 'fail'; label: string };
export type ExecTiming = {
  attempt: TraceAttempt;
  duration: number | null;
  response: number | null;
  segments: ExecSegment[];
  label: string;
  tone: 'accent' | 'fail' | 'muted';
};

/** 相对行时间戳的毫秒偏移（早于行时间戳或不可读时为 null）。 */
function relative(row: RoutingTraceRow, value?: number | null): number | null {
  const base = toMillis(row.ts);
  if (value == null || base == null || !Number.isFinite(value) || value < base) return null;
  return value - base;
}

export function execTiming(row: RoutingTraceRow, now: number, t: T) {
  const attempts = traceAttempts(row);
  const last = execLastAttempt(row, attempts);
  const running = isTraceRunning(row);
  const total = running ? Math.max(0, now - (toMillis(row.ts) ?? now)) : row.durationMs ?? null;
  /** 首帧读数：AIGX 后端未下发，恒为 null（对应段不画） */
  const firstFrame: number | null = null;
  const response = last ? relative(row, toMillis(last.finishedAt)) : null;
  const timings: ExecTiming[] = attempts.map((attempt, index) => {
    const started = relative(row, toMillis(attempt.startedAt));
    const responded = relative(row, toMillis(attempt.finishedAt));
    const state = attemptState(row, attempt);
    const cancelled = isTraceCancelled(row) && attempt.index === last?.index;
    const finished = relative(row, toMillis(attempt.finishedAt))
      ?? (state === 'running' && attempt.index === last?.index ? total : null);
    const nextStart = relative(row, toMillis(attempts[index + 1]?.startedAt));
    const end = finished ?? responded;
    const frame = firstFrame != null && started != null && firstFrame >= started
      && (finished == null || firstFrame <= finished) && (nextStart == null || firstFrame < nextStart) ? firstFrame : null;
    const segments: ExecSegment[] = [];
    const add = (start: number, stop: number, tone: ExecSegment['tone'], label: string): void => {
      if (stop > start) segments.push({ start, end: stop, tone, label: `${label} ${formatDuration(stop - start)}` });
    };
    // 缺少起止时间的旧尝试不补画；响应头到达与上游首帧是两个独立读数。
    if (started != null && end != null && end >= started) {
      const boundary = Math.min(end, frame ?? end, responded != null && responded >= started ? responded : end);
      add(started, boundary, state === 'failed' && frame == null && !cancelled ? 'fail' : 'wait', t('等待响应'));
      if (frame != null && frame <= end) {
        add(boundary, frame, 'frame', t('等待首帧'));
        add(frame, end, state === 'failed' && !cancelled ? 'fail' : 'stream', t('输出'));
      } else {
        add(boundary, end, state === 'failed' && !cancelled ? 'fail' : 'frame', t('响应到达后'));
      }
    }
    return {
      attempt,
      duration: started != null && finished != null && finished >= started ? finished - started : null,
      response: responded,
      segments,
      label: cancelled ? t('已取消')
        : state === 'running' ? tracePhase(row.phase, (k) => t(k))
          : state === 'failed' ? [attempt.status != null ? String(attempt.status) : '', t('失败')].filter(Boolean).join(' · ')
            : state === 'success' ? t('成功')
              : t('结果未记录'),
      tone: cancelled ? 'muted' : state === 'failed' ? 'fail' : state === 'running' ? 'accent' : 'muted',
    };
  });
  const scale = Math.max(total ?? 0, ...timings.flatMap((timing) => timing.segments.map((segment) => segment.end)), 1);
  return { total, firstFrame, response, scale, timings };
}

/**
 * 这次请求当时吃的是哪一层配置。
 *
 * AIGX 的 trace 行只记 `route.strategy` / `route.sticky`，没有 A2A 的 `configSnapshot`
 * （scope / groupId / groupName）—— 所以这里恒为「未记录」，绝不按当前配置猜当时值。
 */
export function execScopeLabel(_row: RoutingTraceRow, t: T): string {
  return t('未记录');
}

/** 选路方式文案（取值与文案用 AIGX 自己的枚举：weighted / round_robin / ranked）。 */
export function execStrategyLabel(strategy: string | undefined, t: T): string {
  if (!strategy) return t('未记录');
  switch (strategy) {
    case 'weighted': return t('加权随机');
    case 'round_robin': return t('组内轮询');
    case 'ranked': return t('多因子调度');
    default: return strategy;
  }
}

/** 会话粘滞文案（AIGX 的 session 是「长会话」，与 A2A 的「会话内」不是同一时长，不搬它的词）。 */
export function execStickyLabel(sticky: string | undefined, t: T): string {
  if (!sticky) return t('未记录');
  switch (sticky) {
    case 'auto': return t('自动');
    case 'session': return t('长会话');
    case 'turn': return t('每轮');
    case 'off': return t('关闭');
    default: return sticky;
  }
}
