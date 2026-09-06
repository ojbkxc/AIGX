// 网络层类型定义（与 api/network、SystemMonitorPanel、NetworkLayer 对齐）

export interface AccountPoolState {
  totalAccounts: number;
  availableAccounts: number;
  busyAccounts: number;
  errorAccounts: number;
  invalidAccounts: number;
  totalRequests: number;
  failedRequests: number;
}

export interface ConnectionPoolState {
  totalConnections: number;
  activeConnections: number;
  idleConnections: number;
  totalConnectionsCreated: number;
  totalConnectionsClosed: number;
  successfulRequests: number;
  failedRequests: number;
  avgLatencyMs: number;
}

export interface SessionPoolState {
  totalSessions: number;
  activeSessions: number;
  idleSessions: number;
  sessionTtlHours: number;
}

/** 网络层状态信息 */
export interface NetworkStatus {
  enabled: boolean;
  accountPool: AccountPoolState;
  connectionPool: ConnectionPoolState;
  sessionPool: SessionPoolState;
  loadBalanceStrategy: string;
  lastCheckAt: number;
}

/** 网络层状态默认值（后端字段缺失时的容错回退） */
export const defaultNetworkStatus: NetworkStatus = {
  enabled: true,
  accountPool: {
    totalAccounts: 0,
    availableAccounts: 0,
    busyAccounts: 0,
    errorAccounts: 0,
    invalidAccounts: 0,
    totalRequests: 0,
    failedRequests: 0,
  },
  connectionPool: {
    totalConnections: 0,
    activeConnections: 0,
    idleConnections: 0,
    totalConnectionsCreated: 0,
    totalConnectionsClosed: 0,
    successfulRequests: 0,
    failedRequests: 0,
    avgLatencyMs: 0,
  },
  sessionPool: {
    totalSessions: 0,
    activeSessions: 0,
    idleSessions: 0,
    sessionTtlHours: 0,
  },
  loadBalanceStrategy: '',
  lastCheckAt: 0,
};

/** 网络层配置请求 */
export interface NetworkConfigRequest {
  enabled: boolean;
  strategy: string;
}

/** 网络层配置响应 */
export interface NetworkConfigResponse {
  enabled: boolean;
  strategy: string;
  accountPoolMin: number;
  accountPoolMax: number;
  connectionPoolMax: number;
  sessionPoolMax: number;
}

/** 网络层账号配置 */
export interface AccountConfigRequest {
  name: string;
  accountId: string;
  apiToken: string;
  status: string;
  priority: number;
}

/** 渠道信息（网络层视角） */
export interface NetworkChannelInfo {
  id: string;
  name: string;
  provider: string;
  baseUrls: string[];
  apiKey: string;
  isCircuitOpen: boolean;
  healthStatus: 'healthy' | 'warning' | 'error';
  trafficRatio: number;
  priority: number;
  weight: number;
  connections: number;
  latency: number;
  successRate: number;
  lastError: string | null;
  lastChecked: number;
}

/** 分布式节点信息 */
export interface DistributedNode {
  id: string;
  name: string;
  address: string;
  status: 'online' | 'offline' | 'syncing';
  version: string;
  healthScore: number;
  cpuUsage: number;
  memoryUsage: number;
  replicationStatus: Array<{ channelId: string; status: string; latency: number }>;
  lastHeartbeat: number;
  dataCenter: string;
  isLeader: boolean;
}

/** 系统监控指标（SystemMonitorPanel 消费） */
export interface Metrics {
  cpuUsage: number;
  memoryUsage: number;
  diskUsage: number;
  networkTx: number;
  networkRx: number;
  activeConnections: number;
  totalRequests: number;
  failedRequests: number;
  successRate: number;
  avgLatency: number;
  throughput: number;
  errorRate: number;
  uptime: number;
  startTime: number;
  currentLoad?: number;
}

/** 自动扩缩容配置 */
export interface ScalingConfig {
  minNodes: number;
  maxNodes: number;
  nodes: Array<{ id: number; status: string; load: number }>;
  autoScalingEnabled: boolean;
  scalingThreshold: number;
  cooldownPeriod: number;
  currentLoad: number;
  idealLoad: number;
  loadBalanceMode: string;
  lastScaledAt: number;
}

/** 网络层仪表盘数据 */
export interface NetworkDashboardData {
  totalRequests: number;
  activeConnections: number;
  requestSuccessRate: number;
  avgLatency: number;
  nodesOnline: number;
  nodesTotal: number;
  scalingStatus: string;
  recentErrors: string[];
}

/** 系统监控面板指标（从网络层状态聚合换算） */
export interface Metrics {
  cpuUsage: number;
  memoryUsage: number;
  diskUsage: number;
  networkTx: number;
  networkRx: number;
  activeConnections: number;
  throughput: number;
  errorRate: number;
  successRate: number;
  avgLatency: number;
  currentLoad: number;
  uptime: number;
}

/** 后端 /api/network/status 的 snake_case 原始结构 */
export interface NetworkStatusRaw {
  enabled: boolean;
  account_pool: {
    total_accounts: number;
    available_accounts: number;
    busy_accounts: number;
    error_accounts: number;
    invalid_accounts: number;
    total_requests: number;
    failed_requests: number;
  };
  connection_pool: {
    total_connections: number;
    active_connections: number;
    idle_connections: number;
    successful_requests: number;
    failed_requests: number;
    avg_latency_ms: number;
  };
  session_pool: {
    total_sessions: number;
    active_sessions: number;
    idle_sessions: number;
  };
  load_balance_strategy: string;
  last_check_at: number;
}
