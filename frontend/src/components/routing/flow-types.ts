/**
 * 请求流向的数据形状（移植自 A2A `ui-islands/routing-flow-types.ts`）。
 *
 * 上游这些类型既是 `/api/routing/flow` 的 wire 形状，也是流向画布的入参；
 * AIGX 后端 `/api/routing/flow` 的字段面已在 `types/routing-workspace.ts`
 * （`RoutingFlowSnapshot` / `RoutingFlowPath`）落过，这里只补「界面侧」的三样：
 *   · `FlowPath`   —— 画布用的路径聚合行（比 wire 多 `lastSeen` 等统计口径）；
 *   · `FlowSelection` —— 画布上的选中对象（客户端 / 模型 / 提供商 / 账号 / 路径 / 请求 / 会话）；
 *   · `ConfigTarget` —— 从流向点进配置时要跳的目标。
 * 形状与上游逐字段同名，方便以后两边对照。
 */
import type { RoutingFlowPath, RoutingTraceRow } from '../../types/routing-workspace';

/** 查询条件（对应 `/api/routing/flow` 的 query）。 */
export type FlowQuery = {
  model?: string;
  minutes: number;
  scope: 'window' | 'running';
  search?: string;
  sessionSearch?: string;
  path?: string;
  session?: string;
  client?: string;
  provider?: string;
  account?: string;
  activity?: 'all' | 'running' | 'idle';
  detail?: 'sessions' | 'requests' | 'models';
  offset?: number;
  limit?: number;
};

/** 画布上的一条路径：客户端 → 模型 → 提供商/账号，带统计口径。 */
export type FlowPath = {
  id: string;
  client: string;
  clientLabel?: string;
  model: string;
  provider: string;
  /** AIGX 的 wire 行带 providerLabel（A2A 的 FlowPath 没有这一格，画布不依赖它） */
  providerLabel?: string;
  accountId: string;
  accountName: string;
  requests: number;
  running: number;
  successful: number;
  failed: number;
  sessions: number;
  stickyHits: number;
  switched: number;
  lastSeen: number;
};

/** wire 的路径行 → 画布路径行（缺字段按 0 / 空串补齐，不猜）。 */
export function toFlowPath(raw: RoutingFlowPath, index: number): FlowPath {
  const client = raw.client ?? '';
  const model = raw.model ?? '';
  const provider = raw.provider ?? '';
  const accountId = raw.accountId ?? '';
  const accountName = raw.accountName ?? '';
  return {
    id: raw.id ?? JSON.stringify([client, model, provider, accountId, accountName]),
    client,
    clientLabel: raw.clientLabel,
    model,
    provider,
    providerLabel: raw.providerLabel,
    accountId,
    accountName,
    requests: raw.requests ?? 0,
    running: raw.running ?? 0,
    successful: raw.successful ?? 0,
    failed: raw.failed ?? 0,
    sessions: raw.sessions ?? 0,
    stickyHits: raw.stickyHits ?? 0,
    switched: raw.switched ?? 0,
    lastSeen: raw.lastSeen ?? index,
  };
}

export type FlowSnapshot = {
  available: boolean;
  paths: FlowPath[];
  pathCount: number;
  truncated: boolean;
  requests: number;
  running: number;
  sessions: number;
  unknownRequests: number;
  now: number;
  since: number;
};

export type FlowSession = {
  id: string;
  model: string;
  requestCount: number;
  running: number;
  lastSeen: number;
  latest: RoutingTraceRow;
};

export type FlowDetails = {
  available: boolean;
  entries: Array<FlowSession | RoutingTraceRow>;
  matched: number;
  unknownRequests?: number;
  now: number;
};

/** `detail=models`：当前时间窗内真的发过请求的模型（筛选下拉用）。 */
export type FlowModels = { available: boolean; models: string[]; now: number };

export type FlowSelection =
  | { kind: 'client' | 'model' | 'provider' | 'account'; id: string }
  | { kind: 'path'; id: string }
  | { kind: 'edge'; model: string; client?: string; accountId?: string; provider?: string }
  | { kind: 'request'; row: RoutingTraceRow }
  | { kind: 'session'; session: FlowSession }
  | null;

export type ConfigTarget = { kind: 'model' | 'provider' | 'account'; id: string };
