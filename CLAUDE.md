# AIGX项目开发者指南

## 项目概览

**AIGX**（AI Gateway Extended）是一个现代化的AI网关管理系统，采用Rust后端+React前端架构。

### 核心特性
- 🚀 Rust后端 - 高性能、安全
- 🎨 现代化前端（计划） - React 19 + TypeScript
- 🔐 完整的权限管理系统
- 📊 实时监控和仪表盘
- 🌐 多协议支持（OpenAI、Anthropic等）

## 技术栈

### 当前技术栈
```yaml
前端：
  - React（JS实现）
  - Vite（构建工具）
  - Tailwind CSS

后端：
  - Rust（Rust 2021）
  - Axum（Web框架）
  - 标准库 + Cargo生态系统
```

### 目标技术栈
```yaml
前端：
  - React 19
  - TypeScript 6.0+
  - Tailwind CSS 4
  - shadcn/ui（组件库）
  - TanStack Query（数据层）
  - TanStack Router（路由）

后端：
  - Rust（最新稳定版）
  - Axum 0.7+
  - 异步运行时（Tokio）
```

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
│   └── ...                 # 其余见 lib.rs 模块声明
├── frontend/               # React 18 + TypeScript + Vite 前端
│   ├── src/               # 页面 / 组件 / hooks / i18n
│   └── package.json
├── aigx-net/               # 独立网络层 crate（workspace 成员）
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

### 项目迁移

#### 第1步：TypeScript迁移
- [ ] 将JS文件转换为TypeScript
- [ ] 添加类型定义
- [ ] 集成TypeScript检查

#### 第2步：React 19升级
- [ ] 升级React到19.x
- [ ] 适配新API
- [ ] 测试兼容性

#### 第3步：shadcn/ui集成
- [ ] 初始化shadcn/ui
- [ ] 替换现有UI组件
- [ ] 自定义配置

#### 第4步：TanStack Query集成
- [ ] 重构API层
- [ ] 添加查询缓存
- [ ] 优化数据获取

### CI/CD
所有构建和测试都在GitHub Actions中运行：
- 本地编译不会成功验证
- 所有提交必须通过CI

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
- [aisix项目规范](../../rustapi/aisix/CLAUDE.md)

## 常见问题

### Q: 为什么本地编译会失败？
A: 因为项目依赖GitHub Actions的编译验证。所有构建必须在云环境中完成。

### Q: 如何进行功能性测试？
A: 必须使用E2E测试框架（未来）或手动测试套件。不支持单元测试。

### Q: 100年是怎么来的？
A: 基于费米估算，一代人约28年，100年≈4代技术周期。这个时间框架用于架构设计。