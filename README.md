# AIGX — AI Gateway Extended

<div align="center">

**高性能 · 多协议 · 可扩展的 AI 中转网关**

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange)](https://www.rust-lang.org)
[![React](https://img.shields.io/badge/React-18+-61DAFB)](https://react.dev)
[![License](https://img.shields.io/badge/License-Source_Available-blue)](#许可证)
[![Docker](https://img.shields.io/badge/Docker-Supported-2496ED)](https://www.docker.com)

[English](#english) · [中文](#中文)

</div>

---

## 项目简介

AIGX 是一个 **OpenAI / Anthropic 兼容的 AI 中转网关**。它聚合多个上游 AI 服务（Cloudflare Workers AI、OpenAI 兼容上游、Anthropic、Gemini、智谱 Z.AI），向下游客户端暴露统一的 API 入口，并在网关层提供认证鉴权、多维度限流、分组权限、模型定价与倍率、多渠道智能调度、支付充值、日志审计与安全监控等完整能力。

架构上参照了 [new-api](https://github.com/QuantumNous/new-api) 的功能布局与 [aisix](https://github.com/aisix/aisix) 的 Rust 实现思路，UI 设计借鉴了 [cf-ai-gw](https://github.com/o-t-w/cf-ai-gw) 的玻璃拟态风格。

### 典型使用场景

- **个人 / 团队自用**：统一管理多个 AI 服务的 API Key，用一套密钥访问全部模型，按渠道优先级与健康状态自动调度，支持用量统计、限额控制与成本核算。
- **模型能力聚合**：将 Cloudflare Workers AI、第三方 OpenAI 兼容服务与官方模型聚合为一个端点，客户端无需关心上游差异。
- **内部网关**：通过用户分组、模型白名单、IP 黑白名单、RPM/TPM 限流与审计日志，构建企业内部的模型访问入口。

> 本项目**开源免费**，供个人与非商业用途使用。**任何商业用途（包括但不限于对外售卖、SaaS 化部署、集成到商业产品中）须获得作者书面授权**，详见[许可证](#许可证)。

---

## 核心特性

### 数据面（面向下游调用方）

- **OpenAI 兼容**：`/v1/chat/completions`、`/v1/responses`、`/v1/completions`、`/v1/embeddings`、`/v1/images/generations`、`/v1/audio/*`、`/v1/models`
- **Anthropic 原生协议**：`/v1/messages`，支持 Claude 原生格式与流式响应
- **统一 API Key**：一套密钥访问所有上游渠道，支持模型白名单、过期时间、配额上限与 IP 限制
- **响应缓存**：相同请求命中缓存，降低上游调用成本与延迟
- **Token 估算**：BPE 级 token 统计（基于 `tiktoken-rs`），用于计费与限流

### 渠道调度

- **多上游类型**：Cloudflare Workers AI（经 cf-ai-gw Worker 的 AI Binding 桥接）、OpenAI 兼容、Anthropic、Gemini、智谱 Z.AI
- **智能调度**：优先级降序 → 同优先级按权重加权随机 → 断路器 / 健康状态 / 亲和性叠加修正
- **断路器模式**：渠道连续失败自动熔断，支持半开探测与手动重置
- **健康巡检**：后台周期探测渠道延迟与可用性，EMA 平滑
- **AIMD 限流协同**：上游限流反馈自适应调整发送速率
- **空响应防护**：连续空响应检测与渠道降权
- **工具调用修复**：跨协议转换时的 `tool_calls` 兼容性修正

### 管理与计费

- **用户体系**：邮箱注册、角色权限（管理员 / 普通用户）、GitHub / Google OAuth 登录、密码找回
- **用户分组**：分组倍率、分组允许模型
- **定价引擎**：按模型定价（输入 / 输出 / 缓存命中）、模型倍率 × 分组倍率、多币种汇率、价格数据源自动同步
- **支付充值**：易支付（Epay）与 Stripe，支持支付回调验签、订单状态管理
- **兑换码**：批量生成 / 单码生成 / 兑换充值 / 有效期管理
- **API 令牌**：创建、编辑、禁用、轮换（一次性展示新密钥）、用量重置

### 安全与可观测

- **多维度限流**：全局 / 用户 / 密钥的 RPM 与 TPM，支持窗口配置
- **IP 管理**：全局 IP 白名单 / 黑名单，支持 CIDR 网段
- **关键词护栏**：prompt / 响应关键词过滤
- **安全事件**：认证失败、限流触发、IP 拦截、滥用检测、入侵尝试，含严重程度分级
- **日志审计**：请求日志（模型、token、费用、延迟、错误）与管理员操作审计日志，支持 CSV / JSON 导出
- **告警通知**：Telegram、SMTP（含 STARTTLS）、Slack、Webhook，后台巡检触发（断路器打开 / 渠道延迟 / 进程内存）
- **系统监控**：CPU / 内存 / 磁盘 / 网络 / 负载 / 进程采集（非 Linux 平台自动降级）
- **Prometheus 指标**：文本格式指标导出，便于接入现有监控体系

### 架构特性

- **Rust 后端**：Axum 0.7 + Tokio 异步运行时，单二进制交付
- **React 前端**：React 18 + Vite，玻璃拟态管理后台，支持中英文切换与暗色主题
- **存储灵活**：默认 FileStore（bundled SQLite KV，零配置）；可选 SeaORM 接入 PostgreSQL / MySQL
- **多平台**：Linux / Windows / macOS，AMD64 / ARM64 预编译产物
- **容器化**：多阶段 Dockerfile，非特权用户运行，健康检查内建

---

## 快速开始

### Docker Compose（推荐）

```bash
# 克隆仓库
git clone https://github.com/ojbkxc/AIGX.git
cd AIGX

# 启动服务（后端默认映射到宿主 9527，前端映射到 80）
docker compose up -d
```

启动后访问 `http://localhost`（前端管理面板），API 数据面入口为 `http://localhost:9527`。

> 首次启动时，查看后端日志获取初始管理员密码；生产环境请务必通过环境变量修改 `ADMIN_PASSWORD` 与 `JWT_SECRET`。

### 本地构建运行

```bash
# 后端（默认 FileStore + SQLite，零外部依赖）
cargo build --release
./target/release/aigx

# 前端（开发模式，端口 3000）
cd frontend
npm install
npm run dev
```

后端配置保存在 `~/.aigx/config.toml`，数据保存在 `~/.aigx/data`。配置文件在首次启动时自动生成，常用配置项包括：

```toml
[server]
host = "127.0.0.1"
port = 8080
data_dir = "~/.aigx"

[database]
# 留空使用默认 FileStore；填写则启用 SeaORM（需按 feature 编译）
url = ""

[usage]
daily_limit = 10000
monthly_limit = 100000
```

### 预编译二进制

从 [GitHub Releases](https://github.com/ojbkxc/AIGX/releases) 下载对应平台的二进制，直接运行即可。Linux 下需先赋予执行权限：

```bash
chmod +x aigx-linux-x86_64
./aigx-linux-x86_64
```

### 对接你的 AI 应用

网关暴露的是标准 OpenAI / Anthropic 协议，将应用中的 API Base URL 指向网关，API Key 替换为在网关「API 令牌」页面创建的密钥即可：

```text
Base URL: https://your-gateway.example.com/v1
API Key:  sk-aigx-xxxx（网关创建的令牌）
```

支持 OpenRouter / DeepSeek / Kimi / Claude Code 等主流客户端的 `base_url + api_key` 对接方式。

---

## 项目结构

```text
├── src/               # Rust 后端（axum）
│   ├── api/           #   数据面 + 管理面 handler
│   ├── channel/       #   渠道存储与调度（断路器/健康/亲和/AIMD）
│   ├── bridge/        #   上游协议适配（cf/openai/anthropic/gemini/zai）
│   ├── pricing/       #   定价目录/汇率/价格同步
│   ├── payment/       #   易支付 + Stripe
│   ├── ratelimit/     #   RPM/TPM 多维度限流
│   ├── notify/        #   告警规则 + Telegram/SMTP/Slack/Webhook
│   └── storage/       #   FileStore / SQLite KV
├── aigx-net/          # 独立网络层 crate
├── frontend/          # React + Vite 管理后台
├── containers/        # 容器构建辅助
├── monitoring/        # 监控配置
├── docs/              # 详细文档
└── Dockerfile         # 多阶段生产镜像
```

---

## 文档

- [业务全景](./docs/BUSINESS.md) — 模块职责、数据模型、前后端 API 契约
- [API 文档](./docs/api-documentation.md) — 完整 REST API 说明
- [网络层指南](./docs/network-layer-guide.md) — 网络层架构与使用
- [测试指南](./docs/testing-guide.md) — 测试与验证方法
- [部署指南](./DEPLOYMENT.md) — 生产环境部署
- [隐私说明](./PRIVACY.md) — 数据处理规范

---

## 贡献

欢迎提交 Issue 与 Pull Request：

1. Fork 本仓库
2. 创建特性分支（`git checkout -b feature/amazing-feature`）
3. 提交更改（`git commit -m 'Add some amazing feature'`）
4. 推送到分支（`git push origin feature/amazing-feature`）
5. 提交 Pull Request

提交前请确保：

- 后端通过 `cargo fmt` 与 `cargo clippy -- -D warnings`
- 前端通过 `npm run build`（TypeScript 严格检查）
- 新增功能附带必要的文档更新

---

## 许可证

本项目的源代码以 **源码可用（Source Available）** 方式发布：

- ✅ **允许**：个人学习、研究、自用、非商业用途的部署与修改
- ❌ **禁止**：未经授权的商业使用，包括但不限于对外售卖、SaaS 化服务、集成到商业产品、以本项目为基础提供付费服务
- 📩 **商业授权**：如需将 AIGX 用于商业场景，请联系作者获取书面授权

> 本说明不构成法律意见。任何超出上述范围的使用，请务必事先与作者确认。

---

## 致谢

本项目参考与使用了以下优秀开源项目：

- [new-api](https://github.com/QuantumNous/new-api) — 功能布局参考
- [aisix](https://github.com/aisix/aisix) — Rust 实现思路参考
- [cf-ai-gw](https://github.com/o-t-w/cf-ai-gw) — UI 风格参考
- [Axum](https://github.com/tokio-rs/axum) — Rust Web 框架
- [SeaORM](https://www.sea-ql.org/SeaORM/) — Rust ORM
- [React](https://react.dev/) — UI 框架

---

<div align="center">

**AIGX · AI Gateway Extended**

</div>

---

## English

**AIGX** is an OpenAI / Anthropic-compatible AI gateway that aggregates upstream AI services (Cloudflare Workers AI, OpenAI-compatible providers, Anthropic, Gemini, Zhipu Z.AI) behind a unified API endpoint, adding authentication, rate limiting, group permissions, pricing, multi-channel scheduling, payments, audit logging and security monitoring.

The source code is **free for personal, research and non-commercial use**. **Commercial use (including resale, SaaS hosting, or integration into commercial products) requires written authorization from the author.** See [License](#许可证) for details.

For setup instructions, configuration, and API details, refer to the Chinese sections above and the [docs](./docs) directory.
