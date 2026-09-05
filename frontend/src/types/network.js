// 类型定义文件 - 网络层

// 网络层状态信息
export const NetworkStatus = {
  // 网络层是否启用（始终为 true：数据面即网络层）
  enabled: true,
  // 账号池状态
  accountPool: {
    totalAccounts: 0,
    availableAccounts: 0,
    busyAccounts: 0,
    errorAccounts: 0,
    invalidAccounts: 0,
    totalRequests: 0,
    failedRequests: 0,
  },
  // 连接池状态（渠道连接 + 健康状态聚合）
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
  // 会话池状态（上游亲和会话 + 限流状态聚合）
  sessionPool: {
    totalSessions: 0,
    activeSessions: 0,
    idleSessions: 0,
    sessionTtlHours: 0,
  },
  // 负载均衡策略（渠道优先级 + 权重 + 断路器叠加）
  loadBalanceStrategy: '',
  // 最后检查时间（unix 秒）
  lastCheckAt: 0,
};

// 网络层配置请求
export const NetworkConfigRequest = {
  // 是否启用网络层
  enabled: true,
  // 负载均衡策略（暂存，后续接入渠道调度权重时使用）
  strategy: 'priority+weighted+circuit',
};

// 网络层配置响应
export const NetworkConfigResponse = {
  enabled: true,
  strategy: '',
  accountPoolMin: 2,
  accountPoolMax: 10,
  connectionPoolMax: 10,
  sessionPoolMax: 50,
};

// 网络层账号配置
export const AccountConfigRequest = {
  name: '',
  accountId: '',
  apiToken: '',
  status: 'active',
  priority: 1,
};

// 渠道信息（前端表示）
export const ChannelInfo = {
  id: '',
  name: '',
  provider: '', // 'openai', 'anthropic', 'gemini', etc.
  baseUrls: [],
  apiKey: '',
  isCircuitOpen: false, // 断路器是否打开
  healthStatus: 'healthy', // 'healthy', 'warning', 'error'
  trafficRatio: 0.5,
  priority: 1,
  weight: 10,
  connections: 0,
  latency: 0,
  successRate: 0.99,
  lastError: null,
  lastChecked: 0,
};

// 分布式节点信息
export const DistributedNode = {
  id: '',
  name: '',
  address: '',
  status: 'online', // 'online', 'offline', 'syncing'
  version: '1.0.0',
  healthScore: 100,
  cpuUsage: 30,
  memoryUsage: 50,
  replicationStatus: [
    { channelId: 'channel1', status: 'synced', latency: 5 },
    { channelId: 'channel2', status: 'syncing', latency: 120 },
  ],
  lastHeartbeat: 0,
  dataCenter: '',
  isLeader: false,
};

// 监控指标
export const Metrics = {
  cpuUsage: 30,
  memoryUsage: 50,
  diskUsage: 70,
  networkTx: 0,
  networkRx: 0,
  activeConnections: 0,
  totalRequests: 0,
  failedRequests: 0,
  successRate: 0.99,
  avgLatency: 20,
  throughput: 0,
  errorRate: 0.01,
  uptime: 0,
  startTime: 0,
};

// 扩容配置
export const ScalingConfig = {
  minNodes: 1,
  maxNodes: 10,
  nodes: [
    { id: 1, status: 'ready', load: 0 },
    { id: 2, status: 'ready', load: 0 },
  ],
  autoScalingEnabled: true,
  scalingThreshold: 80,
  cooldownPeriod: 300, // 秒
  currentLoad: 50,
  idealLoad: 100,
  loadBalanceMode: 'latency', // 'latency', 'random', 'least_loaded'
  lastScaledAt: 0,
};

// 仪表盘数据
export const DashboardData = {
  totalRequests: 0,
  activeConnections: 0,
  requestSuccessRate: 0.99,
  avgLatency: 20,
  nodesOnline: 0,
  nodesTotal: 0,
  scalingStatus: 'normal', // 'normal', 'scale_up', 'scale_down', 'warning'
  recentErrors: [],
};