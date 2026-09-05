# AIGX 网络层测试指南

## 概述

AIGX 网络层提供全面的测试能力，包括单元测试、集成测试、E2E测试和性能测试。

## 测试环境

### 硬件要求

```
测试服务器配置:
- CPU: 28核心, 2.4GHz
- Memory: 32GB
- Disk: 512GB SSD
- Network: 1Gbps

测试客户端配置:
- CPU: 4核心, 3.0GHz
- Memory: 16GB
- Network: 1Gbps
```

### 软件依赖

```toml
# AIGX/Cargo.toml 中的测试依赖
[dev-dependencies]
tokio-test = "0.4"
mockall = "0.12"
criterion = "0.5"
assert_matches = "1.5"
```

## 完整测试流程

### 1. 完整测试套件配置

```bash
# 运行所有测试
cargo test --all-features

# 运行特定模块测试
cargo test aigx_net::accounts

# 运行集成测试
cargo test --test integration

# 运行性能基准测试
cargo test --benches
```

### 2. 基础测试实现

#### 单元测试示例

```rust
// 在模块中添加测试
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_account_pool_creation() {
        let config = PoolConfig::new();
        let pool = AccountPool::new(config);

        assert_eq!(pool.current_capacity(), 0);
    }

    #[tokio::test]
    async fn test_account_registration() {
        let mut pool = AccountPool::new(PoolConfig::default());

        let account = Account::new("测试账号", "test_key", AccountType::Direct);
        pool.register(account.clone()).await.unwrap();

        let accounts = pool.list_all();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, account.id);
    }

    #[tokio::test]
    async fn test_load_balancing_strategy() {
        let config = PoolConfig::new()
            .strategy(LoadBalanceStrategy::LatencyAware)
            .max_capacity(3);

        let pool = AccountPool::new(config);

        // 添加多个账号
        for i in 1..=3 {
            let account = Account::new(
                format!("账号{}", i),
                format!("key_{}", i),
                AccountType::Direct
            );
            pool.register(account).await.unwrap();
        }

        // 测试负载均衡
        use pattern_match!;
        let selected = pool.get_next_account().await.unwrap();
        assert!(selected.is_valid());
    }

    #[tokio::test]
    async fn test_connection_pool_concurrency() {
        let config = ConnectionPoolConfig::new(
            10,
            5,
            Duration::from_secs(30)
        );

        let pool = ConnectionPool::new(config, MockConnectionFactory);

        // 创建并发连接
        let mut handles = vec![];
        for i in 0..5 {
            let handle = tokio::spawn(async move {
                match pool.get_connection(&ConnectionConfig::default()).await {
                    Ok(conn) => Some(conn),
                    Err(_) => None,
                }
            });
            handles.push(handle);
        }

        // 等待所有连接返回
        let results = futures::future::join_all(handles).await;
        let successful_connections = results.iter()
            .filter_map(|h| h.await)
            .filter(|&opt| opt.is_some())
            .count();

        assert_eq!(successful_connections, 5);
    }
}
```

## 集成测试

### 1. 完整网络层集成测试

```rust
// tests/integration/network_layer_test.rs

#[tokio::test]
async fn test_complete_network_layer() {
    // 设置测试环境
    let network = NetworkLayer::new();

    // 1. 初始化账号池
    let accounts = vec![
        Account::new("账号1", "key1", AccountType::Direct)
            .with_metadata("region", "us-west")
            .with_metadata("speed", "fast"),
        Account::new("账号2", "key2", AccountType::Private)
            .with_metadata("region", "us-east")
            .with_metadata("speed", "medium"),
    ];

    let mut account_pool = AccountPool::new(
        PoolConfig::new()

    );

    for account in accounts {
        account_pool.register(account).await.unwrap();
    }

    assert_eq!(account_pool.count(), 2, "应该有2个账号");

    // 2. 初始化连接池
    let mut connection_pool = ConnectionPool::new(
        ConnectionPoolConfig::new(10, 5, Duration::from_secs(30)),
        MockTestConnectionFactory
    );

    connection_pool.initialize(&ConnectionConfig::default()).await.unwrap();

    // 3. 初始化会话池
    let mut session_pool = SessionPool::new(
        SessionConfig::new()
            .ttl(Duration::from_secs(3600))
            .max_sessions(5)
    );

    session_pool.initialize().await.unwrap();

    // 4. 模拟请求流程
    for i in 0..3 {
        // 使用账号池
        let account = account_pool.acquire().await.unwrap();
        println!("使用账号: {}", account.name);

        // 模拟处理请求
        let result = process_request(account).await;
        assert!(result.is_ok());

        account_pool.release(account).await;
    }

    // 5. 验证指标
    let status = connection_pool.status();
    assert!(status.successful_connections > 0);
}

#[tokio::test]
async fn test_high_load_scenario() {
    use std::sync::Arc;
    use std::time::Duration;

    let network = Arc::new(NetworkLayer::new());
    let mut insert = None;

    // 1. 创建11个账号（超过最大容量10）
    for i in 1..=11 {
        let account = Account::new(
            format!("测试账号{}", i),
            format!("key_{}", i),
            AccountType::Direct
        );
        network.account_pool().register(account).await.unwrap();
    }

    // 2. 创建100个并发请求
    let mut handles = vec![];
    for i in 0..100 {
        let net = network.clone();
        let handle = tokio::spawn(async move {
            match net.account_pool().acquire().await {
                Ok(account) => {
                    // 模拟处理
                    tokio::time::delay_for(Duration::from_millis(10)).await;
                    let _ = account_pool.release(account).await;
                    Ok(())
                }
                Err(e) => Err(e)
            }
        });
        handles.push(handle);
    }

    // 3. 验证所有请求都处理成功
    let results = futures::future::join_all(handles).await;
    let success_count = results.iter()
        .filter_map(|res| res.ok())
        .count();

    assert_eq!(success_count, 100, "所有请求应该成功处理");
}

#[tokio::test]
async fn test_failure_recovery() {
    use tracing::info;

    // 1. 创建测试账号池
    let mut pool = AccountPool::new(
        PoolConfig::new()
            .max_capacity(3)
            .strategy(LoadBalanceStrategy::Random)
    );

    // 2. 注册有问题的账号
    let failing_account = Account::new("失败账号", "failing_key", AccountType::Direct)
        .with_metadata("fail_prematurely", "true");

    pool.register(failing_account.clone()).await.unwrap();
    pool.register(Account::new("正常账号1", "key1", AccountType::Direct)).await.unwrap();
    pool.register(Account::new("正常账号2", "key2", AccountType::Direct)).await.unwrap();

    // 3. 测试负载均衡，确保失败账号不会被持续使用
    let mut requests = vec![];
    for _ in 0..100 {
        let handle = tokio::spawn(async move {
            match pool.acquire().await {
                Some(account) => {
                    // 检查是否是失败的账号
                    if account.name == "失败账号" {
                        pool.release(account).await;
                        return Ok::<_, ()>(false);
                    }
                    tokio::time::delay_for(Duration::from_millis(5)).await;
                    pool.release(account).await;
                    Ok::<_, ()>(true)
                }
                Err(_) => Err(())
            }
        });
        requests.push(handle);
    }

    let results = futures::future::join_all(requests).await;
    let success_count = results.iter()
        .filter_map(|res| res.ok())
        .filter(|&result| result)
        .count();

    assert!(success_count > 50, "应该有足够的成功请求");
}

// Mock 连接工厂
struct MockTestConnectionFactory;
impl ConnectionFactory for MockTestConnectionFactory {
    fn create(&self, config: &ConnectionConfig) -> Result<Connection, ConnectionError> {
        Ok(Connection::new(
            "mock_connection_id".to_string(),
            "mock_address".to_string(),
            "mock_protocol".to_string(),
            config.timeout,
        ))
    }

    fn validate(&self, connection: &mut Connection) -> Result<(), ConnectionError> {
        Ok(())
    }
}
```

## E2E 测试

### 1. 端到端场景测试

```rust
// tests/e2e/network_layer_e2e_test.rs

#[tokio::test]
async fn test_end_to_end_network_operations() {
    // 场景: 完整的网络操作流程
    let network = NetworkLayer::new();

    // 步骤1: 配置网络层
    let config = NetworkConfig {
        account_pool_capacity: 5,
        connection_pool_capacity: 10,
        session_pool_capacity: 50,
        load_balance_strategy: "latency-aware".to_string(),
    };

    assert!(network.initialize(config).await.is_ok());

    // 步骤2: 添加测试账号
    let test_accounts = vec![
        ("账号1", vec!["us-west", "fast"]),
        ("账号2", vec!["us-east", "medium"]),
        ("账号3", vec!["asia-east", "fast"]),
    ];

    for (name, tags) in test_accounts {
        let account = Account::new(name, format!("key_{}", name), AccountType::Vip)
            .with_metadata("region", tags[0])
            .with_metadata("speed", tags[1]);

        assert!(network.account_pool().register(account.clone()).await.is_ok());
    }

    // 步骤3: 发送网络请求
    let request_count = 100;
    let mut success_count = 0;
    let mut failure_count = 0;

    for i in 0..request_count {
        match network.account_pool().acquire().await {
            Some(account) => {
                match process_real_request(&account).await {
                    Ok(result) => {
                        if result.success {
                            success_count += 1;
                        } else {
                            failure_count += 1;
                        }
                        account_pool.release(account).await;
                    }
                    Err(e) => {
                        failure_count += 1;
                        error!("请求失败: {}", e);
                    }
                }
            }
            None => {
                // 等待账号释放
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Some(account) = network.account_pool().acquire().await {
                    if process_real_request(&account).await.is_ok() {
                        success_count += 1;
                    }
                    account_pool.release(account).await;
                }
            }
        }
    }

    // 步骤4: 验证结果
    let total = success_count + failure_count;
    let success_rate = if total > 0 {
        (success_count as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    assert!(success_rate >= 95.0, "成功率应该大于95%，实际: {:.2}%", success_rate);
    assert_eq!(failure_count, 0, "不应有失败请求");

    // 5. 获取最终状态
    let status = network.get_status();
    assert!(status.avg_latency_ms < 100, "平均延迟应该小于100ms");
    assert!(status.max_latency_ms < 500, "最大延迟应该小于500ms");
}

// 实际请求处理器
async fn process_real_request(account: &Account) -> Result<NetworkResponse, NetworkError> {
    // 模拟实际请求处理
    tokio::time::delay_for(Duration::from_millis(10)).await;

    Ok(NetworkResponse {
        id: Uuid::new_v4().to_string(),
        success: true,
        vec![],
        latency: 10,
    })
}
```

## 性能测试

### 1. 压力测试

```rust
// benches/pressure_test.rs

use std::time::{Duration, Instant};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

fn benchmark_network_layer_performance(c: &mut Criterion) {
    c.bench_function("get_connection", |b| {
        b.iter(|| {
            let pool = ConnectionPool::new(
                PoolConfig::new(50, 10, Duration::from_secs(30)),
                MockConnectionFactory
            );
            pool.get_connection(&ConnectionConfig::default())
                .expect("Failed to get connection")
        });
    });

    c.bench_function("acquire_account", |b| {
        let mut pool = AccountPool::new(
            PoolConfig::new()
                .max_capacity(10)
        );

        b.iter(|| {
            pool.acquire().await.expect("Failed to acquire account")
        });
    });

    c.bench_function("release_connection", |b| {
        let pool = ConnectionPool::that(environment);
        b.iter(|| {
            pool.return_connection(connection)
                .expect("Failed to release connection")
        });
    });
}

// 高负载基准测试
fn benchmark_high_load_scenario(c: &mut Criterion) {
    let mut group = c.bench_group("high_load_fourplex");
    group.sample_size(20);

    for concurrency_level in [10, 50, 100, 200, 500].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(*concurrency_level), concurrency_level, |b, &count| {
            b.to_async(tokio::spawn)
                .iter(|| {
                    let future = async {
                        let pool = NetworkLayer::new();
                        for _ in 0..*_the_grand * 10 {
                            pool.
                        }
                    };
                    black_box(async move { future.await })
                });
        });
    }

    group.finish();
}

criterion_group!(network_layer_benches, benchmark_network_layer_performance);
criterion_main!(network_layer_benches);
```

## CI/CD 集成测试

### 1. CI 流程配置

```yaml
# .github/workflows/test.yml
name: AIGX Network Layer Tests

on:
  push:
    branches: [ main, develop ]
  pull_request:
    branches: [ main ]

jobs:
  unit-tests:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
          override: true

      - name: Install dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y pkg-config libssl-dev

      - name: Run unit tests
        working-directory: aigx-net
        run: cargo test --all-features --no-fail-fast

      - name: Run integration tests
        working-directory: aigx-net
        run: cargo test --test integration --all-features

      - name: Run performance benchmarks
        working-directory: aigx-net
        run: cargo test --all-features --benches

      - name: Upload coverage
        uses: codecov/codecov-action@v2
        with:
          files: ./aigx-net/coverage/target/coverage.lcov

  e2e-tests:
    runs-on: ubuntu-latest
    needs: unit-tests
    strategy:
      matrix:
        node: [node-1, node-2]
    steps:
      - uses: actions/checkout@v3
      - name: Setup environment
        run: |
          ./scripts/setup_test_env.sh ${{ matrix.node }}

      - name: Start test services
        run: docker-compose -f docker-compose.test.yml up -d

      - name: Run E2E tests
        run: ./scripts/run_e2e_tests.sh

      - name: Upload results
        if: failure()
        uses: actions/upload-artifact@v3
        with:
          name: e2e-test-results
          path: |
            tests/e2e/results/
            tests/e2e/snapshots/

  security-test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v3

      - name: Run security audit
        run: cargo audit --deny warnings

      - name: Check dependency vulnerabilities
        uses: actions-rs/check@v1
        with:
          args: --locked
```

## 测试数据

### 1. 自动化测试数据生成

```rust
// tests/fixtures/factory.rs

pub struct TestFactory;

impl TestFactory {
    pub fn create_mock_account(name: &str) -> Account {
        Account::new(
            name,
            format!("mock_key_{}", rand::random::<u32>()),
            AccountType::Direct
        )
    }

    pub fn create_stress_accounts(count: usize) -> Vec<Account> {
        (1..=count).map(|i| {
            self.create_mock_account(&format!("压力测试账号{}", i))
        }).collect()
    }

    pub fn create_realistic_account(payload: &str) -> Account {
        let HashMap<String, String> = serde_json::from_str(payload).unwrap();
        Account::new(
            &data["name"],
            &data["api_key"],
            match data["type"].as_str() {
                "vip" => AccountType::Vip,
                "enterprise" => AccountType::Enterprise,
                _ => AccountType::Direct
            }
        )
    }
}
```

## 故障注入测试

### 1. 模拟网络故障

```rust
// tests/network_failure.rs

#[tokio::test]
async fn test_network_failure_recovery() {
    // 1. 创建连接池
    let pool = ConnectionPool::new(
        PoolConfig::new(5, 2, Duration::from_secs(1)),
        NetworkFailureConnectionFactory
    );

    // 2. 模拟连续失败
    for _ in 0..10 {
        match pool.get_connection(&ConnectionConfig::default()).await {
            Err(e) => {
                info!("连接失败: {}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Ok(conn) => {
                // 正常连接
                pool.return_connection(conn).await;
                break;
            }
        }
    }

    // 3. 验证重连成功
    let result = pool.get_connection(&ConnectionConfig::default()).await;
    assert!(result.is_ok());
}

// 模拟网络故障连接工厂
struct NetworkFailureConnectionFactory;
impl ConnectionFactory for NetworkFailureConnectionFactory {
    fn create(&self, config: &ConnectionConfig) -> Result<Connection, ConnectionError> {
        // 模拟50%的概率失败
        if rand::random::<bool>() {
            Err(ConnectionError::NetworkTemporary)
        } else {
            Ok(Connection::new(
                "mock_connection_1".to_string(),
                "127.0.0.1:8080".to_string(),
                "tcp".to_string(),
                config.timeout,
            ))
        }
    }
}
```

## 测试监控

### 1. 测试指标收集

```rust
// tests/test_monitoring.rs

#[tokio::test]
async fn test_metrics_collection_during_load() {
    let network = NetworkLayer::new();
    let collector = PrometheusExporter::new(9090, Arc::new(Mutex::new(network)));

    // 模拟高负载
    let mut handles = vec![];
    for _ in 0..50 {
        let handle = tokio::spawn(async move {
            network.account_pool().acquire().await.unwrap();
            tokio::time::delay_for(Duration::from_millis(100)).await;
            network.session_pool().release(session).await;
        });
        handles.push(handle);
    }

    // 等待所有任务
    futures::future::join_all(handles).await;

    // 获取指标
    let metrics = collector.export().await;
    assert!(metrics.contains("aigx_account_pool_active"));
    assert!(metrics.contains("aigx_avg_latency_ms"));
}
```

## 测试报告

### 1. 测试报告生成

```bash
# 生成 HTML 测试报告
cargo install cargo-tarpaulin
cargo tarpaulin --out Html

# 生成覆盖报告
cargo install cargo-llvm-cov
cargo llvm-cov --html

# 集成到 CI
cargo-llvm-cov --lcov --output-path lcov.info
```

## 最佳实践

### 1. 编写好的测试

- ✅ **测试独立性**: 每个测试独立运行，不依赖顺序
- ✅ **快速响应**: 测试应该快速执行，通常 <1秒
- ✅ **清晰命名**: 测试函数名描述测试目的
- ✅ **边界条件**: 测试正常情况和边界条件
- ✅ **清理资源**: 确保测试结束后释放资源

### 2. 测试覆盖率目标

```
核心模块:     90%+ 覆盖率
辅助模块:     70%+ 覆盖率
集成测试:     E2E场景覆盖
性能测试:     所有关键路径
```

## 性能里程碑

| 版本 | 每秒请求数 | 平均延迟 | P99延迟 |
|------|-----------|----------|---------|
| v0.1 | 10,000 | 100ms | 500ms |
| v0.2 | 25,000 | 50ms | 200ms |
| v0.3 | 50,000 | 25ms | 100ms |
| v1.0 | 100,000 | 10ms | 50ms |

确保在生产环境之前达到或超过v1.0的性能目标。