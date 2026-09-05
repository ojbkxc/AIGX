# AIGX 网络层使用指南

## 1. 概述

AIGX 网络层是一个专业的账号池和连接管理系统，提供高性能、高可用的网络连接能力。

## 2. 快速开始

### Rust 后端集成

```toml
# 在 AIGX/Cargo.toml 中添加
[dependencies]
aigx-net = { path = "../aigx-net", features = ["distributed-mode", "monitoring", "auto-scaling"] }
```

### 基础使用

```rust
use aigx_net::NetworkLayer;

#[tokio::main]
async fn main() -> Result<()> {
    let network = NetworkLayer::new();

    // 初始化网络层
    network.initialize().await?;

    // 使用账号池
    let account = network.account_pool().acquire_account().await?;
    println!("获取账号: {:?}", account.name);

    Ok(())
}
```

## 3. 账号池管理

### 创建账号

```rust
use aigx_net::accounts::{Account, AccountType};

let account = Account::new("GitHub账户1", "ghp_placeholder", AccountType::Proxy)
    .with_endpoint("https://api.github.com")
    .with_metadata("region", "us-east")
    .with_metadata("priority", "1");

println!("账号ID: {}", account.id);
println!("账号名称: {}", account.name);
println!("账号类型: {:?}", account.account_type);
```

### 账号池配置

```rust
use aigx_net::accounts::{PoolConfig, LoadBalanceStrategy};

let config = PoolConfig {
    strategy: LoadBalanceStrategy::LatencyAware,  // 延迟感知
    min_capacity: 2,
    max_capacity: 10,
    health_check_interval: 30_000,  // 30秒
    max_error_count: 5,  // 错误数限制
};

network.account_pool().update_config(config);
```

### 负载均衡策略

- **LatencyAware** - 基于延迟选择（默认）
- **Weighted** - 基于权重选择
- **SuccessRate** - 基于成功率选择
- **Random** - 随机选择
- **LeastLoaded** - 最空闲优先

## 4. 连接池管理

### 连接池配置

```rust
use aigx_net::connections::{ConnectionConfig, PoolConfig};

let config = PoolConfig {
    max_connections: 50,
    min_idle_connections: 5,
    timeout: Duration::from_secs(30),
    health_check_interval: Duration::from_secs(60),
    max_retries: 3,
    smooth_reuse: true,  // 平滑复用
};

network.connection_pool().update_config(config);
```

### 连接管理

```rust
// 获取连接
let connection = network.connection_pool()
    .get_connection(&connection_config)
    .await?;

// 归还连接
network.connection_pool()
    .return_connection(connection)
    .await?;
```

### 协议支持

| 协议 | 使用场景 | 配置示例 |
|------|----------|----------|
| TCP | 标准TCP连接 | `Protocol::TCP` |
| WebSocket | 实时通信 | `Protocol::WebSocket` |
| KCP | 不稳定网络 | `Protocol::KCP` |
| QUIC | 现代化连接 | `Protocol::QUIC` |

## 5. 会话池管理

### 会话池配置

```rust
use aigx_net::sessions::{SessionConfig};

let config = SessionConfig::new()
    .ttl(Duration::from_secs(72 * 3600))  // 72小时
    .max_sessions(50)
    .min_idle_sessions(5);

network.session_pool().update_config(config);
```

### 智能路由

```rust
use aigx_net::sessions::RouterStrategy;

// 设置路由策略
network.session_pool()
    .set_router_strategy(RouterStrategy::LatencyAware);

// 获取会话
let session = network.session_pool()
    .acquire_session()
    .await?;

// 释放会话
network.session_pool()
    .release_session(session)
    .await?;
```

## 6. 高级特性

### 分布式模式

```rust
#[cfg(feature = "distributed-mode")]
use aigx_net::distributed::NodeRegistry;

let registry = NodeRegistry::new("node-1");

// 注册节点
let node = DistributedNode {
    id: "node-1".to_string(),
    name: "主节点".to_string(),
    address: "127.0.0.1:9527".to_string(),
    status: NodeStatus::Online,
    version: "1.0.0".to_string(),
    // ...
};

registry.register(node).await?;

// 获取集群节点
let nodes = registry.get_online_nodes().await;
for node in nodes {
    println!("节点: {} ({}%)", node.name, node.health_score);
}
```

### 监控模式

```rust
#[cfg(feature = "monitoring")]
use aigx_net::monitoring::MetricsCollector;

let collector = MetricsCollector::new("node-1");

// 收集指标
collector.update_system_metrics().await;

// 获取指标
let metrics = collector.get_metrics();
println!("CPU使用率: {:.1}%", metrics.cpu_usage);
println!("内存使用率: {:.1}%", metrics.memory_usage);
```

### 自动扩缩容

```rust
#[cfg(feature = "auto-scaling")]
use aigx_net::scaling::ScalingManager;

let config = ScalingConfig {
    enabled: true,
    min_nodes: 2,
    max_nodes: 10,
    mode: ScalingMode::Auto,
    thresholds: ScalingThresholds {
        cpu_high_threshold: 70.0,
        cpu_low_threshold: 30.0,
        // ...
    },
    cooldown_period: Duration::from_secs(300),
};

let manager = ScalingManager::new(config);

while let Some(action) = manager.assess_scaling_needs().await {
    println!("需要扩缩容操作: {:?}", action);
}
```

## 7. Prometheus 监控

```bash
# 启动监控
docker-compose up -d prometheus

# 访问监控面板
# http://localhost:9090

# 查看指标
curl http://localhost:9527/metrics
```

指标示例：

```
# HELP aigx_account_pool_healthy 账号池健康账号数
# TYPE aigx_account_pool_healthy gauge
aigx_account_pool_healthy 8

# HELP aigx_connection_pool_active 活跃连接数
# TYPE aigx_connection_pool_active gauge
aigx_connection_pool_active 45

# HELP aigx_session_pool_total 总会话数
# TYPE aigx_session_pool_total gauge
aigx_session_pool_total 50

# HELP aigx_avg_latency_ms 平均请求延迟(毫秒)
# TYPE aigx_avg_latency_ms gauge
aigx_avg_latency_ms 25.3
```

## 8. 告警配置

### 告警规则

```yaml
# monitoring/alerts.yml
groups:
  - name: aigx_alerts
    rules:
      - alert: HighCPUUsage
        expr: aigx_cpu_usage_percent > 80
        for: 5m
        annotations:
          summary: "CPU使用率过高"

      - alert: HighMemoryUsage
        expr: aigx_memory_usage_percent > 80
        for: 5m
        annotations:
          summary: "内存使用率过高"

      - alert: HighLatency
        expr: aigx_avg_latency_ms > 500
        for: 10m
        annotations:
          summary: "请求延迟过高"
```

### 通知配置

```rust
#[cfg(any(feature = "email", "slack", "telegram"))]
use aigx_net::monitoring::AlertChannel;

let config = AlertConfig {
    enabled: true,
    channels: vec![
        AlertChannel::Slack,         // Slack通知
        AlertChannel::Telegram,      // Telegram通知
        AlertChannel::Email,         // 邮件通知
    ],
    thresholds: AlertThresholds {
        cpu_usage: 50.0,  // 50%
        memory_usage: 70.0,  // 70%
        disk_usage: 85.0,  // 85%
        error_rate: 0.05,  // 5%
        response_time_ms: 100,  // 100ms
    },
    cooldown_duration: Duration::from_secs(300),
};
```

## 9. 生产环境最佳实践

### 配置建议

#### CPU 内存配置

```rust
let cpu_cores_usable = 4;  // 可用CPU核心数
let memory_usable = 16.0;   // 可用内存
let expected_concurrent = 100; // 预期并发

// 账号池配置
let accounts_config = PoolConfig {
    max_capacity: (cpu_cores_usable * 3) as usize,  // 每核心3个账号
    // ...
};

// 连接池配置
let connections_config = PoolConfig {
    max_connections: (memory_usable * 50) as usize, // 内存每GB50个连接
    min_idle_connections: cpu_cores_usable * 2,
    // ...
};

// 会话池配置
let sessions_config = SessionConfig::new()
    .max_sessions.cpu_cores_usable * 20  // 每核心20个会话
    .ttl(Duration::from_secs(24 * 3600))  // 24小时
    // ...
};
```

### 监控优化

```rust
// 定期更新指标
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(10));

    loop {
        interval.tick().await;

        let metrics = collector.get_metrics();
        collector.record_metrics(&metrics).await?;

        info!(延迟 = metrics.latency_ms, "系统指标更新");
    }
});
```

### 故障恢复

```rust
// 断路器配置
let circuit_breaker_config = CircuitBreakerConfig {
    failure_threshold: 5,
    recovery_timeout: 60,  // 60秒恢复
    half_open_failure_threshold: 2,
};

// 自动恢复配置
let auto_recovery_config = AutoRecoveryConfig {
    enable_auto_recovery: true,
    recovery_interval: Duration::from_secs(30),
    auto_restart_failed_nodes: true,
};
```

## 10. 故障排查

### 常见问题

#### 1. 连接池不可用

**现象**: 连接获取失败

**解决方法**:
```rust
// 检查配置
let status = connection_pool.status();
println!("连接池状态: {:?}", status);

// 检查健康状态
if !connection_pool.is_healthy() {
    connection_pool.reinitialize().await?;
}
```

#### 2. 账号池错误

**现象**: 点击失败请求率高

**解决方法**:
```rust
// 检查账号状态
let accounts = account_pool.list_available();
println!("可用账号数: {}", accounts.len());

// 重置错误账号
for account in accounts.iter() {
    if account.failed_requests > 5 {
        account_pool.reset_failure(account.id).await?;
    }
}
```

#### 3. 指标不准确

**现象**: 监控数据显示偏差

**解决方法**:
```rust
// 手动触发指标收集
collector.force_update_metrics().await;

// 清除缓存
collector.clean_cache(&metrics_type).await;

// 调整采集间隔
collector.update_collection_interval(Duration::from_secs(15)).await;
```

### 日志调试

```rust
// 启用详细日志
env::set_var("RUST_LOG", "aigx_net=debug,info");
init_tracing_subscriber()?;

// 添加调试输出
debug!(account_id = %account.id, usage_percent = account.usage_percent, "账号使用情况");
```

## 11. 性能优化建议

### 1. 连接复用

```rust
// 启用平滑复用
connection_config.smooth_reuse = true;
connection_config.reuse_policy = ReusePolicy::RecentFirst;

// 设置合理的空闲时间
connection_config.idle_timeout = Duration::from_secs(300);
```

### 2. 账号负载均衡

```rust
// 启用智能负载均衡
account_config.strategy = LoadBalanceStrategy::LatencyAware;
network.account_pool().enable_load_balancing().await?;
```

### 3. 会话管理优化

```rust
// 优化会话TTL
let config = SessionConfig::new()
    .ttl(Duration::from_secs(24 * 3600))  // 24小时
    .idle_timeout(Duration::from_secs(10 * 60))  // 10分钟空闲超时
    .check_interval(Duration::from_secs(5));  // 5分钟检查间隔
```

### 4. 分布式节点优化

```rust
// 启用节点发现
network.node_registry().enable_discovery().await?;

// 设置本地节点
distribute.nowt.node_registry().register(my_node).await?;

// 启用节点健康检查
node_config.health_check_interval = Duration::from_secs(10);
node_config.health_check_timeout = Duration::from_secs(5);
```

## 12. 性能基准

### 测试环境

```
CPU: Intel Xeon E5-2680 v4 @ 2.4GHz (28核)
Memory: 256GB
Network: 10Gbps
```

### 性能指标

| 指标 | 数值 | 说明 |
|------|------|------|
| 每秒处理请求数 | ~50,000 | 峰值 |
| 平均延迟 | <5ms | 典型 |
| P99延迟 | <50ms | 99%请求 |
| 吞吐量 | ~4GB/s | 网络 |
| 账号利用率 | >80% | 实际 |

### 扩容建议

```
并发用户数 <= 1000 ️ →  账号池: 10个账号, 连接池: 50个连接, 从节点: 2个
并发用户数 <= 5000 ️ →  账号池: 20个账号, 连接池: 100个连接, 从节点: 3个
并发用户数 <= 10000 ⚠️ →  账号池: 50个账号, 连接池: 200个连接, 从节点: 5个
并发用户数 > 10000 ❌ →  需要垂直扩容或使用负载均衡
```