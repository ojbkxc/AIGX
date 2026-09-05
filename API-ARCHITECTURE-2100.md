# AIGX后端架构2100：Rust优先的持久化设计

> **核心原则**：后端尽量为Rust，面向100年演进，结合rustapi全项目群最佳实践

---

## 🏗️ 后端技术栈选择

### 当前技术栈
```rust
// 当前后端（推测）
Cargo.toml
Backend/
├── src/
│   ├── main.rs      // axum web framework
│   ├── handler/     // handler层
│   ├── models/      // 数据模型
│   ├── service/     // 业务逻辑
│   └── config.rs    // 配置管理
```

### 目标前端技术栈（基于new-api验证）
```rust
[dependencies]
axum = "0.7"              // Web framework
tauri = { version = "2", features = ["all"] }  // Desktop embedding
tower = { version = "0.5", features = ["json"] }
tower-http = { version = "0.5", features = ["fs", "trace"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
jsonwebtoken = "9"
bcrypt = "0.15"
argon2 = "0.5"

// 持久化
rusqlite = { version = "0.32", features = ["bundled"] }
sea-orm = { version = "1.1", features = ["runtime-tokio-rustls", "sqlite", "macros"] }
mongodb-driver = "3.2"

// 缓存
redis = { version = "0.25", features = ["all"] }
dashmap = "6.0"

// 日志与监控
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
metrics = "0.23"

// 任务队列
tokio-cron-scheduler = "0.13"
```

### 参考new-api的Rust实现
```rust
// new-api核心特性
- AIGX相同功能但更新堆栈
- 完整的channel adapter系统
- 统一的billing expenditure
- 文档化在RELAY 和 BILLING_EXPRESS中
```

---

## 📐 后端分层架构（100年不过时）

### 1. Adapter模式渐层
```rust
pub trait AIAdapter {
    async fn relay_stream(
        &self,
        request: AIRequest<'_>
    ) -> Result<Stream<Box<dyn Reader>>>;

    fn supports(&self, model: &str) -> bool;
}

// 具体实现
pub struct CloudflareAdapter { /* ... */ }
pub struct OpenAICompatibleAdapter { /* ... */ }
pub struct StatusRateAdapter { /* ... */ }
pub struct TokenLimiterAdapter { /* ... */ }
```

### 2. 语义化状态机
```rust
#[derive(Debug, Clone)]
pub enum ChannelState {
    Healthy,
    Degraded,
    Failed,
    Maintenance,
    Deprecated
}

pub struct ChannelStateMachine {
    state: ChannelState,
    transition_rules: TransitionRules
}

impl ChannelStateMachine {
    pub fn can_transition(&self, changed_state: ChannelState) -> bool {
        // 状态转换规则引擎
    }

    pub fn on_health_failure(&mut self) -> ChannelAction {
        if self.state.is_critical() {
            ChannelAction::CircuitBreaker
        } else {
            ChannelAction::Alert
        }
    }
}
```

### 3. 业务领域建模
```rust
// 核心领域模型，永不废弃
mod domain {
    pub struct User {
        pub id: Uuid,
        pub email: String,
        pub api_key: String,
        pub quota_remaining: i64,
        pub roles: Vec<Role>
    }

    pub enum Role {
        Admin,
        Manager,
        User
    }
}

mod billing {
    pub struct BillingRecord {
        pub user_id: Uuid,
        pub token_count: i64,
        pub quota_rate: f64,
        pub actual_cost: i64,
        pub created_at: DateTime
    }
}
```

---

## 🔩 Rust API设计规范

### 基于new-api的AGENTS.md规范

#### 数据库兼容性（三套数据库）
```rust
// 用户界面驱动的数据库兼容层
pub async fn query_users(db: &Database) -> Result<Vec<User>> {
    match &db.driver {
        DatabaseDriver::Postgres => postgres_query(),
        DatabaseDriver::MySQL => mysql_query(),
        DatabaseDriver::SQLite => sqlite_query()  // 默认
    }
}

// GORM迁移策略
pub async fn migrate_all() -> Result<()> {
    // SQLite: ALTER TABLE ... ADD COLUMN
    // Postgres: ALTER TABLE ... ADD COLUMN
    // MySQL: ALTER TABLE ... ADD COLUMN

    // 文档记录迁移命令
    record_migration("ALTER TABLE users ADD COLUMN role VARCHAR")
}
```

#### HTTP响应标准化
```rust
use axum::{
    extract::State,
    response::Json
};

pub struct APIResponse<T> {
    pub Option<T>,
    pub error: Option<String>,
    pub meta: ResponseMeta
}

pub struct ResponseMeta {
    pub timestamp: DateTime,
    pub request_id: String
}

#[derive(Debug, Serialize)]
pub enum ErrorType {
    Unauthorized,
    Forbidden,
    NotFound,
    ValidationError,
    InternalServerError
}
```

### 状态机驱动的业务逻辑

基于burncloud的PC readiness系统：
```rust
pub struct Product<T> {
    pub name: String,
    pub status: ProductStatus,
    pub health_check: HealthStatus
}

impl<T> Product<T> {
    pub fn status_recommendation(&self) -> Recommendation {
        match (self.status, self.health_check) {
            (ProductStatus::Idle, HealthStatus::Healthy) => {
                Recommendation::AllReady
            }
            (ProductStatus::Idle, HealthStatus::Degraded) => {
                Recommendation::CheckComponent
            }
            _ => Recommendation::NotReady
        }
    }
}

pub enum Recommendation {
    AllReady,
    RequiresConfiguration,
    AttentionRequired,
    NeedsResolution,
    Danger
}
```

---

## 🎯 在AIGX中的具体实现

### 1. Channel Relay架构（与new-api一致的架构）
```rust
// src/relay/channel.rs
pub struct ChannelRelay {
    // Entity: Cloudflare Account ID, Workers AI Binding URL
    pub entity: ChannelEntity,
    // Logic: State machine, priority, weight
    pub logic: PriorityState,
    // Transformer: Convert request/response formats
    pub transformer: FormatTransformer
}

impl ChannelRelay {
    pub async fn process_request(
        &self,
        request: APIRequest<'_>
    ) -> Result<AIResponse> {
        // 1. 检查状态机
        if self.should_block() {
            return self.block_response();
        }

        // 2. format transformation
        let transformed = self.transformer.transform(request)?;

        // 3. send upstream
        let response = self.send_upstream(transformed).await?;

        // 4. log result
        self.log_usage(request, &response);

        Ok(response)
    }
}
```

### 2. Unified Billing计算器
```rust
use billing_expr::ExpressionEngine;

pub struct BillingCalculator {
    // 完整的计费规则引擎，100年不变
    pub expression: ExpressionEngine
}

impl BillingCalculator {
    pub fn calculate(
        &self,
        request: &APIRequest<'_>
    ) -> BillingBreakdown {
        // token normalization
        let tokens = self.to_effective_tokens(request.input);

        // expression evaluation
        let rat = self.rate_value(tokens.len());
        let ratio = self.ratio_value(request.group);

        // safety clamping
        let charge = clamp(
            tokens as f64 * rat * ratio,
            MIN_QUOTA,
            MAX_QUOTA
        );

        BillingBreakdown {
            tokens: tokens as i64,
            quota_charge: charge as i64,
            flag_saturation: false
        }
    }
}
```

### 3. Rel cardus Data Interpretation
```rust
pub enum RelCardusPolicy {
    Exact(ExactPolicy),
    Range(RangePolicy),
    Custom(CustomPolicy)
}

// 100年不变的数据约束
pub struct ExactPolicy {
    pub required_uuid: Uuid,
    pub required_permission: String
}
```

---

## 🛡️ Rust后端质量保障

### 测试策略（基于new-api实践）
```rust
// 每个channel adapter独立编译测试
#[cfg(test)]
mod channel_test {
    #[cfg(test)]
    #[test]
    fn nested_require_build_successful() -> Result<()> {
        // 禁止 relaykit 依赖 root 模块
        // 独立构建验证
    }
}

// 安全错误处理
pub enum SecurityError {
    ExpiredToken(Error),
    InvalidSignature(Error),
    RateExceeded(Instant),
    MissingHeader(Header)
}
```

---

## 📅 100年演进规划

### 后端层面的时间规划

| 年份 | 技术变化 | Rust兼容策略 |
|------|----------|-------------|
| 2025-2030 | Rust 1.x → 2.0 | Adapter layer隔离 |
| 2030-2050 | 系统语言演进 | Trait接口保持 |
| 2050-2090 | 新范式出现 | Async SDK重实现 |
| 2090-2125 | 可能重构 | 对接已废弃的Tracer |

---

## 🔄 持久化状态保证

### 状态机持久化
```rust
pub trait StatePersistence {
    async fn save_state(&self, state: AppState) -> Result<()>;
    async fn restore_state(&self) -> Result<AppState>;
}

// 状态机从Rust Server保存到Database
pub struct eip_server {
    pub state_machine: ChannelStateMachine,
    pub persistence: StatePersistence<Builtin>
}
```

---

*本文档定义了AIGX后端基于Rust的100年不过时架构设计，确保在技术栈完全淘汰时仍能平滑迁移。*