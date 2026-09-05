# 🏗️ AIGX Network Layer - 网络层架构

## 📝 概述

AIGX Network Layer 是一个独立的网络层 crate，为 AIGX 项目提供专业的账号池管理、连接池管理和智能路由功能。

## 🎯 设计目标

- **100年架构设计** - 解耦、可扩展、易于演进
- **企业级稳定性** - 自动故障转移、智能负载均衡
- **高性能** - 连接复用、异步处理
- **多协议支持** - TCP/KCP/WebSocket/QUIC

## 📁 架构组成

```
aigx-net/
├── accounts/          # 账号池管理
│   ├── account.rs     # 账号对象
│   ├── account_pool.rs # 账号池
│   └── account_guard.rs # 账号守护
├── connections/       # 连接池管理
│   ├── connection_pool.rs # 连接池
│   ├── protocols/     # 协议实现
│   │   ├── tcp.rs     # TCP协议
│   │   ├── websocket.rs # WebSocket协议
│   │   ├── kcp.rs     # KCP协议
│   │   └── quic.rs    # QUIC协议
│   ├── health_check.rs # 健康检查
│   └── connection.rs   # 连接抽象
├── sessions/         # 会话管理
│   ├── session.rs     # 会话实现
│   ├── session_pool.rs # 会话池
│   └── router.rs      # 智能路由
├── distributed/       # 分布式支持（Phase 4）
│   ├── node.rs        # 分布式节点
│   ├── cluster.rs     # 集群管理
│   └── replication.rs  # 数据复制
├── monitoring/         # 监控与告警（Phase 4）
│   ├── metrics.rs     # 指标收集
│   ├── prometheus.rs  # Prometheus导出
│   └── alerts.rs      # 告警系统
└── lib.rs            # 主库接口
```

## 🚀 快速开始

### 1. 添加依赖

在 AIGX 项目中添加 aigx-net 依赖:

```toml
# AIGX/Cargo.toml
[dependencies]
aigx-net = { path = "../aigx-net" }
```

### 2. 基础使用

```rust
use aigx_net::NetworkLayer;

#[tokio::main]
async fn main() -> Result<()> {
    // 创建网络层实例
    let network = NetworkLayer::new();

    // 初始化网络层
    network.initialize().await?;

    // 使用账号池
    let account = network.account_pool().acquire_account().await?;

    // 使用连接池
    let connection = network.connection_pool().get_connection().await?;

    // 使用会话池
    let session = network.session_pool().acquire_session().await?;

    Ok(())
}
```

### 3. 实际集成示例

```rust
// src/api/admin/channels.rs
use aigx_net::{ConnectionPool, AccountPool};

pub async fn handle_chat_with_network_fallback(
    req: Json<ChatRequest>,
    connection_pool: Arc<ConnectionPool>,
    account_pool: Arc<AccountPool>,
) -> Result<Json<ChatResponse>> {
    // 尝试使用网络层
    match connection_pool.get_connection().await {
        Ok(connection) => {
            let result = handle_via_network_layer(req, connection).await?;
            println!("✅ 通过网络层处理成功");
            Ok(Json(result))
        }
        Err(e) => {
            println!("⚠️ 网络层不可用，回退到直接连接: {}", e);
            handle_chat_direct(req).await
        }
    }
}
```

## 🏛️ 核心组件

### 账号池 (AccountPool)

管理多个账号的状态和负载均衡：

```rust
use aigx_net::AccountPool;

let pool = AccountPool::new(pool_config);
pool.initialize(account_configs).await?;

// 获取可用账号
let account = pool.get_account().await?;

// 查看状态
let status = pool.status();
println!("可用账号数: {}", status.available_accounts);
```

### 连接池 (ConnectionPool)

管理网络连接的复用和健康检查：

```rust
use aigx_net::ConnectionPool;

let pool = ConnectionPool::new(config, factory);
pool.initialize(&default_config).await?;

// 获取连接
let connection = pool.get_connection(&config).await?;

// 归还连接
pool.return_connection(connection).await?;
```

### 会话池 (SessionPool)

管理 AI 服务会话的生命周期和智能路由：

```rust
use aigx_net::SessionPool;

let pool = SessionPool::new();
pool.initialize().await?;

// 获取会话
let session = pool.acquire_session().await?;

// 释放会话
pool.release_session(session).await%;
```

## 🔄 智能路由策略

网络层支持多种负载均衡策略：

- **LatencyAware** - 基于延迟（推荐，默认）
- **Weighted** - 基于权重
- **SuccessRate** - 基于成功率
- **Random** - 随机选择
- **LeastLoaded** - 最空闲优先

在会话池中配置策略：

```rust
use aigx_net::sessions::RouterStrategy;

let router = SmartRouter::new();
router.set_strategy(RouterStrategy::LatencyAware);

let session = router.select_session(sessions)?;
```

## 🔧 协议支持

网络层支持多种传输协议：

| 协议 | 说明 | 使用场景 |
|------|------|----------|
| TCP | 标准 TCP | 内网穿透、简单连接 |
| WebSocket | WebSocket | 实时通信、流式输出 |
| KCP | KCP 传输 | 网络不稳定环境 |
| QUIC | QUIC 协议 | 现代化长连接 |

## 📊 监控与指标

网络层提供完整的监控指标：

```rust
// 账号池指标
let account_status = account_pool.status();
println!("账号池状态: {:?}", account_status);

// 连接池指标
let connection_stats = connection_pool.status();
println!("连接池指标: {:?}", connection_stats);

// 会话池指标
let session_pool_status = session_pool.status();
println!("会话池状态: {:?}", session_pool_status);
```

## 🤝 团队协作指南

### 架构理解

1. **独立职责** - 每个模块职责单一，互不侵犯
2. **接口清晰** - 模块间通信通过明确定义的接口
3. **配置灵活** - 支持热配置和动态调整
4. **向后兼容** - 保持与现有系统的兼容性

### 代码规范

1. **错误处理** - 使用 `anyhow::Result` 统一错误处理
2. **日志级别** - `debug!` 用于详细调试，`info!` 用于重要信息
3. **唯一ID** - 所有对象使用 `uuid::Uuid` 生成唯一标识
4. **异步优先** - 所有IO操作必须使用异步模式

### 测试要求

```rust
#[tokio::test]
async fn test_module() { ... }
```

## 🛠️ 故障排查

### 常见问题

1. **连接池不可用**
   - 检查健康检查任务是否启动
   - 查看日志中的错误信息
   - 检查网络配置

2. **账号池无可用账号**
   - 检查账号配置和状态
   - 验证账号凭据是否有效
   - 查看账号池健康度

3. **会话池会话过期**
   - 调整会话TTL设置
   - 检查会话池清理任务
   - 手动清理过期会话

### 调试技巧

```bash
# 启用详细日志
export RUST_LOG=info

# 监控指标
curl http://localhost:9527/metrics

# 查看网络层状态
curl http://localhost:9527/network/status
```

## 🚀 未来演进

### Phase 1: 基础功能
- ✅ 账号池管理
- ✅ 连接池管理
- ✅ 会话池管理

### Phase 2: 高级特性
- [ ] 分布式支持
- [ ] 持久化存储
- [ ] 可视化监控

### Phase 3: 企业级
- [ ] 自动扩缩容
- [ ] 故障自愈
- [ ] 多租户支持

## 📚 参考资料

- [项目架构文档](../ARCHITECTURE-HORIZON-2100.md)
- [API架构文档](../API-ARCHITECTURE-2100.md)
- [参考实现](../../rustapi/ds-free-api)
- [参考实现](../../rustapi/ds2api)

## 📞 联系我们

- **邮箱**: aigx-team@example.com
- **文档**: https://docs.aigx.io/network-layer
- **讨论**: https://github.com/aigx-io/aigx/discussions