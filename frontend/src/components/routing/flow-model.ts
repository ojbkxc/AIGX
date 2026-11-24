/**
 * 请求流向的判据层（移植自 A2A `ui-islands/routing-flow-model.ts`）。
 *
 * ── 只搬「判据」，不搬「配置参与度」──────────────────────────
 * 上游这份模型里有两组东西：
 *   A. 纯函数：客户端显示名 / 会话 id / 单条请求 → 路径行 / 选中匹配 / 汇总；
 *   B. 需要整份 `RoutingWorkspace` 才能算的：提供商分组、账号参与度、账号状态。
 * AIGX 的 `RoutingWorkspaceView` 只有 profile / queue / settings 的**子集**
 * （没有 `accounts` / `providers` / `metrics` / `accountStates`），B 组现在搬过来
 * 只能靠编字段凑，故不搬 —— 画布那边由 canvas 成员按自己的数据源实现。
 * 这里保留 A 组，它们只依赖 trace 行与 FlowPath，纯函数、可单测。
 */
import type { RoutingFlowPath, RoutingTraceRow } from '../../types/routing-workspace';
import { toFlowPath, type FlowPath, type FlowSelection } from './flow-types';
import { isTraceRunning, isTraceSuccess, traceAttempts, toMillis } from './trace-model';

type T = (key: string, vars?: Record<string, string | number>) => string;

/**
 * 客户端显示名。
 *
 * 面板内置的模型调用在库里存的是内部用途标识（`assistant` / `model_test` /
 * `checkin_keepalive` 等），它们不是产品名，直接显示会让人以为是陌生第三方客户端。
 * 这里统一翻成产品里的叫法，与请求日志同一口径。
 */
export function clientLabel(client: string, t: T): string {
  switch (client) {
    case '': return t('未识别客户端');
    case 'other': return t('其他客户端');
    case 'assistant': return t('模型对话');
    case 'assistant_images': return t('助手图片工具');
    case 'assistant_title': return t('助手标题总结');
    case 'model_test': return t('模型测试');
    case 'checkin_keepalive': return t('日活保活');
    default: return client;
  }
}

export const sessionId = (row: RoutingTraceRow): string => {
  const route = row.route as (NonNullable<RoutingTraceRow['route']> & {
    sessionId?: string;
    stickyId?: string;
  }) | null | undefined;
  return route?.sessionId || route?.stickyId || '';
};

export const requestModelId = (row: RoutingTraceRow): string =>
  row.clientModel || row.model || row.upstreamModel || '';

export function selectedRequest(selection: FlowSelection): RoutingTraceRow | null {
  if (!selection) return null;
  if (selection.kind === 'request') return selection.row;
  if (selection.kind === 'session') return selection.session.latest;
  return null;
}

/** 单条请求 → 一条路径行（请求数为 1，其余按该请求的状态计）。 */
export function pathForRequest(row: RoutingTraceRow): FlowPath {
  const client = row.client || '';
  const model = requestModelId(row);
  const provider = row.provider || '';
  const accountId = row.accountId || '';
  const accountName = row.accountName || '';
  return {
    id: JSON.stringify([client, model, provider, accountId, accountId ? '' : accountName]),
    client,
    clientLabel: row.clientLabel,
    model,
    provider,
    accountId,
    accountName,
    requests: 1,
    running: Number(isTraceRunning(row)),
    successful: Number(isTraceSuccess(row)),
    failed: Number(!isTraceRunning(row) && !isTraceSuccess(row)),
    sessions: Number(Boolean(sessionId(row))),
    stickyHits: Number(Boolean(row.route?.stickyHit)),
    switched: Number((row.attempts || 1) > 1),
    lastSeen: toMillis(row.ts) ?? 0,
  };
}

/** wire 路径行 → 画布路径行（列表渲染用）。 */
export function flowPaths(raw: RoutingFlowPath[] | undefined): FlowPath[] {
  return Array.isArray(raw) ? raw.map(toFlowPath) : [];
}

export function matchesSelection(path: FlowPath, selection: FlowSelection): boolean {
  if (!selection) return true;
  if (selection.kind === 'path') return path.id === selection.id;
  if (selection.kind === 'edge') {
    return path.model === selection.model
      && (selection.client == null || path.client === selection.client)
      && (selection.accountId == null || path.accountId === selection.accountId)
      && (selection.provider == null || path.provider === selection.provider);
  }
  if (selection.kind === 'account') return path.accountId === selection.id;
  if (selection.kind === 'client') return path.client === selection.id;
  if (selection.kind === 'model') return path.model.toLowerCase() === selection.id.toLowerCase();
  if (selection.kind === 'provider') return path.provider === selection.id;
  const row = selectedRequest(selection);
  return Boolean(row && path.id === pathForRequest(row).id);
}

export const sumPaths = (
  paths: FlowPath[],
  key: 'requests' | 'running' | 'sessions' | 'stickyHits' | 'successful' | 'failed' | 'switched',
): number => paths.reduce((sum, path) => sum + path[key], 0);

/**
 * 路径上「尝试过哪些提供商」—— 选中某条请求时，画布要把它的尝试链路一并高亮。
 *
 * 上游从 `workspace.accounts` 反查 provider；AIGX 的 trace 行本身带
 * `attemptDetails[].provider`，直接用它，反查表不再需要。
 */
export function attemptedProviders(row: RoutingTraceRow): string[] {
  return [...new Set(traceAttempts(row).map((attempt) => attempt.provider || '').filter(Boolean))];
}
