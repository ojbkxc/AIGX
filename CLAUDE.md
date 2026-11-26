# AIGX项目开发者指南

## 项目概览

**AIGX**（AI Gateway Extended）是一个现代化的AI网关管理系统，采用Rust后端+React前端架构。

### 核心特性
- 🚀 Rust后端 - 高性能、安全
- 🎨 现代化前端 - React 18.2 + TypeScript 5.3 + Vite 5
- 🔐 完整的权限管理系统
- 📊 实时监控和仪表盘
- 🌐 多协议支持（OpenAI、Anthropic等）

## 技术栈

### 当前技术栈（以 `frontend/package.json` 与 `Cargo.toml` 为准）
```yaml
前端：
  - React 18.2 + TypeScript 5.3（源码全部 .tsx/.ts，无 JS 文件）
  - Vite 5（构建）+ esbuild（压缩）
  - Tailwind CSS 3.4 + 项目自有组件层（不是 shadcn/ui）
  - TanStack Query 5（数据层）+ react-router-dom 6（路由）
  - vitest + @testing-library（单测）、Playwright（E2E）

后端：
  - Rust（edition 2021）
  - Axum 0.7 + Tokio
  - 存储：FileStore / SQLite KV（默认），可选 SeaORM（PostgreSQL/MySQL）
  - 插件宿主：wasmtime 36（cranelift + async）
```

### 历史规划（2026-11 复核：以下目标均**未执行**，不要按它行事）
```yaml
前端（未执行）：
  - React 19 / TypeScript 6 / Tailwind CSS 4
  - shadcn/ui、TanStack Router
```
TanStack Query 已在用；路由是 react-router-dom 6，不是 TanStack Router。

## 项目结构

```
AIGX/
├── src/                    # Rust 后端源码（axum 0.7 + Tokio）
│   ├── api/                # 数据面 /v1/* 与管理面 /api/* 路由
│   ├── channel/            # 渠道调度 / 熔断 / AIMD / 健康巡检
│   ├── auth/               # JWT / TOTP 认证
│   ├── bridge/             # 上游协议转换（openai/anthropic/gemini/zai/cf）
│   ├── pricing/            # 定价引擎 / 倍率 / 汇率
│   ├── payment/            # Epay / Stripe 支付
│   ├── db/                 # SeaORM entity / migration
│   ├── storage/            # FileStore / SQLite KV
│   ├── agent/              # AI 运维 Agent
│   └── ...                 # 其余见 main.rs 模块声明
├── frontend/               # React 18 + TypeScript + Vite 前端
│   ├── src/               # 页面 / 组件 / hooks / i18n
│   └── package.json
├── static/                 # 前端构建产物（由 frontend build 生成，提交入库）
├── tests/e2e/             # Playwright 端到端测试
├── tools/                 # 可入库工具脚本（如 newapi_sync.py）
├── README.md
├── Cargo.toml              # Rust workspace 配置
├── AGENTS.md              # 开发规范（权威）
├── PRIVACY.md             # 隐私政策
└── CLAUDE.md              # 本文件
```

## 开发规范

详见[AGENTS.md](./AGENTS.md)获取完整开发规范。

### 关键原则
1. **思考优先编码** - Don't assume, surface tradeoffs
2. **简化优先** - 最少的代码解决问题
3. **测试纪律** - E2E测试是最高优先级
4. **研究纪律** - 仅基于官方文档和源代码验证

## 工作流程

### 本地开发
```bash
# 安装依赖
cd frontend && npm install

# 启动前端开发服务器
npm run dev

# 启动后端服务
cargo build --release
./target/release/aigx
```

### 迁移清单（2026-11 复核：均为未执行的旧计划，现状已不适用）
TypeScript 早已迁移完成（全部 `.tsx/.ts`，`tsc --noEmit` 在 CI 里跑）；TanStack Query
已在用。剩余项（React 19 / shadcn/ui）仍是**未执行**状态，不代表当前方向。

### CI/CD
所有构建与测试都在 GitHub Actions 中运行（`.github/workflows/ci.yml`）：
- 6 组 `cargo check` 特性组合（default / no-default / sea-orm+mysql / sea-orm+postgres / all features / release+sqlite-kv+postgres）
- `lint`（`cargo fmt --check` + `clippy -D warnings`）、`cargo test`、`cargo audit`
- `build frontend`（eslint + tsc + vitest + i18n 双检 + 构建 + static 漂移校验）
- `e2e (playwright)` 与 `e2e (browser, real backend + mock upstream)`
- `route inventory gate`、`upstream drift`、`alerts-lint`

本地不编译 Rust 是**环境限制 + 项目约定**（见下方 FAQ），改动靠 CI 验证。

## 贡献指南

1. 遵循[AGENTS.md](./AGENTS.md)中描述的行为规范
2. 通过workflow-authoring技能规划复杂任务
3. 使用yume--architect技能进行架构设计
4. 通过yume--implementer技能实现改动
5. 使用yume--guardian技能进行代码审查

## 参考文档

- [架构设计文档](./ARCHITECTURE-HORIZON-2100.md)
- [后端架构文档](./API-ARCHITECTURE-2100.md)
- [前端进化路线](./FRONTEND-EVOLUTION-V3.md)
- [aisix项目规范](../rustapi/aisix/CLAUDE.md)（本机路径，不入库）

## 常见问题

### Q: 为什么本地编译会失败？
A: 本机没有 MSVC 工具链（无 `cl.exe`），`cargo check` 会在链接 proc-macro 的 build
script 时以 exit 101 失败。这是环境限制，不是项目设计。**项目约定：不在本地编译
Rust**，`cargo check` / `clippy` / `cargo test` 一律交给 CI；本地只跑
`cargo fmt --all -- --check`、前端 `tsc --noEmit` / `eslint` / `vitest`。

### Q: 如何进行功能性测试？
A: 单测与 E2E 都有，不是「不支持单元测试」：
- Rust：`src/` 内有大量 `#[cfg(test)]` 内联测试，CI 跑 `cargo test`。
- 前端：`vitest` + `@testing-library`（`*.test.ts(x)`），CI 的 `build frontend` 里跑。
- E2E：`frontend/e2e/*.spec.ts`（Playwright，desktop + mobile 两套），以及带真实后端
  + mock 上游的 `e2e (browser)`。
新增逻辑应补对应层的测试；纯 UI 交互优先 E2E。

### Q: 100年是怎么来的？
A: 基于费米估算，一代人约28年，100年≈4代技术周期。这个时间框架用于架构设计。