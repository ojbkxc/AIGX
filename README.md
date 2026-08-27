# AIGX

Rust 实现的 AI 中转网关，同时支持 **OpenAI 兼容格式** 与 **Anthropic 兼容格式**。参考 new-api / cf-ai-gw / ds2api / aisix 的设计，聚合多账号 Cloudflare Workers AI，并提供多用户配额、易支付（Epay）在线充值与流式 /v1 接口。

## Cloudflare AI 架构：Binding 方式

AIGX 通过 **cf-ai-gw Worker** 桥接 Cloudflare Workers AI，cf-ai-gw 内部使用 **AI Binding**（`env.AI.run()`）调用模型，而非 REST API 方式。架构如下：

```
客户端 → AIGX (Rust) → cf-ai-gw Worker (AI Binding) → Cloudflare Workers AI
```

- **cf-ai-gw Worker**：部署在 Cloudflare Workers 上，使用 `wrangler.toml` 配置 AI Binding，通过 `env.AI.run(model, input)` 直接调用模型
- **AIGX Bridge**：`src/bridge/cf.rs` 通过 HTTP 调用 cf-ai-gw Worker，实现多账号负载均衡、故障切换、串行重试
- **优势**：AI Binding 零延迟（Worker 内部调用），无需额外 API Token 鉴权，免费额度由 Cloudflare 提供

> 详细说明见 [cf-ai-gw 项目](https://github.com/ojbkxc/cf-ai-gw) 的 `_worker.js` 实现。

## 特性

- **OpenAI 兼容 API**：`/v1/chat/completions`（含 SSE 流式）、`/v1/completions`、`/v1/embeddings`、`/v1/images/generations`、`/v1/audio/transcriptions|translations|speech`、`/v1/models`
- **Anthropic 兼容 API**：`/v1/messages`（含 SSE 流式），支持 Claude Messages 格式
- **通用上游 Bridge**：channel 模块支持 Cloudflare / OpenAI 兼容 / Anthropic 三种渠道**混用**，按 priority/weight 调度 + failover，已接入 `resolve_bridge` 统一选路
- **多账号 Cloudflare Workers AI**：多账号负载均衡 + 故障切换，账号信息加密落盘，AI 调用使用 Binding 方式
- **多用户与配额**：邮箱注册/登录，管理员 / 普通用户角色，配额按 token 估算扣费，argon2 密码哈希
- **易支付（Epay）对接**：MD5 签名下单、异步通知验签、同步跳转，签名规则与 new-api 一致
- **用量统计**：本地日 / 月 token 统计 + Cloudflare GraphQL neurons 查询
- **限流**：ratelimit 模块在请求路径强制执行（OpenAI 6 处 + Anthropic），`/api/ratelimit/config` 可热更新配置
- **日志审计**：`/api/logs/requests`、`/api/logs/audits`、`/api/logs/requests/export`，请求日志与审计日志分离
- **渠道管理**：`/api/channels` 完整 CRUD + test，支持启停、优先级、权重
- **令牌管理**：`/api/tokens` 完整 CRUD + `reset_used`，支持绑定分组、模型白名单、配额
- **模型定价 + 倍率**：`/api/prices` 维护模型单价，`/api/ratios` 维护分组倍率，计费按 价 × 倍率
- **用户分组**：`/api/groups` 按分组限制可用模型与倍率
- **兑换码**：`/api/redemptions` 批量生成 / 兑换 / 删除
- **通知系统**：`/api/notify/config`（Telegram + SMTP）+ `test-telegram` / `test-email` 测试接口
- **Dashboard 高级统计**：`consumption_trend`、`model_distribution`、`user_ranking`、`channel_health`、`realtime`
- **健康检查**：`/livez`（存活）、`/readyz`（就绪）、`/health`（模型健康汇总），支持优雅关闭
- **多数据库后端**：默认 FileStore + rusqlite KV，可选 SeaORM（PostgreSQL / MySQL），按 `config.database.url` 自动切换
- **关键词 guardrail**：请求/响应关键词过滤；**token 估算**：tiktoken；**缓存**：moka
- **管理面板**：cf-ai-gw 风格的玻璃拟态暗色 UI，明暗主题切换
- **单文件部署**：前端静态资源内嵌，二进制 + `static/` 即可运行

## 快速开始

### 二进制

```bash
# 下载对应平台的 AIGX-<version>-<os>-<arch>.tar.gz 后
tar xzf AIGX-*-linux-amd64.tar.gz
./AIGX-*-linux-amd64
# 浏览器访问 http://127.0.0.1:8080
```

首次启动用邮箱 `admin@aigx.local` + 任意密码登录并设置密码。

### Docker

```bash
docker-compose up -d
# 默认监听 8080，数据卷挂载到 /root/.aigx
```

### 从源码构建

```bash
# 前端
cd frontend && npm ci && npm run build   # 产物输出到 ../static/

# 后端
cd .. && cargo build --release
./target/release/aigx
```

## 配置

配置文件位于 `~/.aigx/config.toml`，首次启动自动生成：

```toml
[server]
host = "127.0.0.1"
port = 8080
data_dir = "~/.aigx"
# 站点对外地址，用于构造易支付回调 URL
server_address = ""

[admin]
password = ""          # 首次登录后自动写入 SHA256 哈希
session_secret = ""    # 自动生成

[usage]
daily_limit = 0        # 0 表示不限
monthly_limit = 0
threshold = 0.9

[epay]
pay_address = ""       # 易支付网关地址
epay_id = ""           # 商户 PID
epay_key = ""           # 商户密钥
pay_methods = ["alipay", "wxpay"]
price = 1.0            # 1 元 = 1 配额
min_topup = 1
custom_callback_address = ""
```

也可在管理面板「易支付」页直接配置。

## API 使用

### OpenAI 兼容格式

```bash
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <你的 API Key>" \
  -d '{"model": "@cf/meta/llama-3.1-8b-instruct", "messages": [{"role": "user", "content": "你好"}]}'
```

### Anthropic 兼容格式

```bash
curl http://127.0.0.1:8080/v1/messages \
  -H "Content-Type: application/json" \
  -H "x-api-key: <你的 API Key>" \
  -d '{"model": "claude-3-haiku", "max_tokens": 1024, "messages": [{"role": "user", "content": "你好"}]}'
```

API Key 在管理面板「API 密钥」页创建，格式为 `sk-...`。

## 模型映射

在「模型映射」页将外部模型名映射到 Cloudflare Workers AI 模型（`@cf/...`），客户端即可使用自定义模型名调用。

## 路线图

- [x] 多账号 CF Workers AI failover（AI Binding 方式）
- [x] OpenAI 兼容流式 / 非流式
- [x] Anthropic 兼容流式 / 非流式
- [x] 通用上游 Bridge（Cloudflare / OpenAI 兼容 / Anthropic 渠道混用，priority/weight 调度 + failover）
- [x] 多用户 + 邮箱注册/登录 + argon2 密码
- [x] 易支付下单 / 回调
- [x] 用量统计 + GraphQL 查询
- [x] 日志审计与限流（请求/审计日志 + ratelimit 强制执行 + 配置接口）
- [x] 渠道管理（CRUD + test）
- [x] 令牌管理（CRUD + reset_used + 分组/模型白名单/配额）
- [x] 模型定价 + 倍率配置
- [x] 用户分组（按分组限制可用模型）
- [x] 兑换码（批量生成/兑换/删除）
- [x] 通知系统（Telegram + SMTP + 测试接口）
- [x] Dashboard 高级统计（consumption_trend / model_distribution / user_ranking / channel_health / realtime）
- [x] 健康检查（/livez /readyz /health + 优雅关闭）
- [x] 多数据库后端（FileStore + 可选 SeaORM PostgreSQL/MySQL）
- [x] 关键词 guardrail + tiktoken 估算 + moka 缓存
- [x] 前端管理面板补全（Channels / Pricing / Groups / Notify 独立页面 + Sidebar 入口 + 路由 + i18n）
- [ ] 更多支付方式
- [ ] 邮箱验证（可选）

## 许可

MIT
