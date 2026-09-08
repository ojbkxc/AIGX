# AIGX 演进路线图（ROADMAP）

> 跨会话恢复入口：下一次 AI 会话先读本文件，再读 `CHANGELOG-AI.md`。
> 本文件是后续所有连续迭代的驱动文件。三份提案文档与它配套：
> - `BACKEND-GAP-MATRIX.md` —— new-api 八大能力域 × AIGX 差距矩阵
> - `FRONTEND-EVOLUTION-PLAN.md` —— open-webui 九大范式 × AIGX 前端演进路线
> - `BRAINSTORM-IDEAS.md` —— 参照项目之外的原创增强点清单
> - `UI-AESTHETIC-MIGRATION.md` —— 现有 34 页面的审美迁移优先级

## 当前阶段

**第二阶段 · P1 后端编码（进行中）**。已完成 P1 第一波（缓存差异化计费 + Rerank + 配额预留/结算 + 2FA/TOTP 全链路：登录验证、管理端设置/启用/禁用接口、前端 Login/Security 区块、SeaORM totp 列迁移）。P1 第二波按二轮审查修订为「计费闭环完整性 + 会话撤销收尾 + 模型元信息同步」（见下）。G2 预留 TTL 解冻、G4 缓存命中记账维度已落地（2026-09-08）。

## 分期总览

| 阶段 | 主题 | 状态 |
|---|---|---|
| 阶段一 | 提案：差距矩阵 + 演进路线 + 头脑风暴 + 分期路线图 + UI 迁移方案 | 已完成 |
| P0 | 用户侧聊天中心 + 工程基座 | **已完成**（2026-09-08：`/chat` 三段式工作区 + reasoning 折叠 + fenced code 复制，验收标准全项落地） |
| P1 | 体验跃升：计费正确性 + 安全水位 + Playground V2 + 分区设置 | 进行中（第一波完成） |
| P2 | 锦上添花：长任务框架 + Realtime + 调度智能运营化 | 未开始 |

## P0 目标与验收标准（延续上一轮，已部分完成）

### P0 目标

- 用户侧 `/chat` 三段式聊天工作区：会话侧栏、消息流、输入框。
- Markdown/代码块/推理折叠的可读消息展示。
- `localStorage` 会话存储，后端 API 保持可替换。
- Dropdown、Drawer、ConfirmDialog、主题系统等可访问基础组件。
- 完成后端能力盘点，避免把已存在的 bridge/端点再实现一遍。

### P0 验收标准

- `cd frontend && npm run typecheck` 通过。
- `cd frontend && npm run build` 通过。
- 未登录访问 `/chat` 会回到登录页；登录后可创建会话、发送消息、刷新页面保留会话。
- 消息中的 fenced code 可复制，reasoning 默认收起并可展开。
- 键盘 Tab/Enter/Escape 可操作下拉菜单、抽屉和确认对话框。

## P1 目标与验收标准（本提案新增 + 上轮保留项合并）

### P1 目标（按优先级排序）

**计费正确性**

1. 缓存命中差异化计费：把 `cache_price` 接到数据面命中计费（当前命中免费但留痕）。
2. 配额预留/结算两段式（`quota_reserve` / `tiered_settle` 语义），对齐流式请求正确扣费。
3. 请求前成本预估器（B2）：Playground/聊天输入实时显示预计消耗，管理端成本模拟器。
   （后端已落地 2026-09-08：`POST /api/pricing/estimate` 支持原始 messages 直出
   预估；前端输入框实时显示待同事审美改造合入后接入。）
4. 缓存命中省钱看板（B1）：仪表盘「缓存节省」卡片。

**安全水位**

5. 2FA/TOTP（自有实现，不引 crate）+ email 登录二次验证。
6. 会话注册表（jti + 用户维度计数）与会话撤销。
7. 调度决策回放（C2）：请求日志记录候选集/过滤原因/选中渠道。

**数据面与模型治理**

8. Rerank 端点（bridge 方法 + 路由 + 独立 DTO）。
9. 模型元信息同步（owned_by/上下文长度/能力）+ 缺失模型检测。
10. 渠道自动分组（auto_group）。

**前端体验跃升**

11. Playground V2：Chat / Completions / Images 三模式（参数调节 + 双栏 JSON）。
12. 分区式 Settings V2（通用/界面/通知/账户/用量五分区）。
13. theme store 三态（system/light/dark）+ 主题色变量 + 统一断点 store。
14. 毫秒级 failover 看板（A2）+ 渠道健康档案（C1）。
15. GraphQL 灵活查询面板（D1）+ 用户侧用量页（D2）。
16. 模型预设（Models workspace）+ 提示词库（Prompts workspace）。

**运营自动化**

17. 定时系统任务框架（轻量 cron，自有实现）。

**二轮审查新增（2026-09-08，见 BRAINSTORM-IDEAS.md G 类）**

18. rerank 计费对齐：从旧式事后计费切换到预留/结算路径（G1）。
19. 预留失败路径释放：渠道全败/客户端错误时 release_quota，杜绝冻结泄漏（G5）。
20. 预留超时回收：reserved_at 时间戳 + cron 回收任务（G2）。
21. 缓存命中记账维度：accumulate 走 cache_read，日志加 cache_hit 标记（G4）。
22. 会话撤销 + 模型元信息同步（原第二波项）。

### P1 验收标准

- OpenAI 兼容请求有 API key 鉴权、错误 envelope 与渠道 failover（保持）。
- 缓存命中按 `cache_price` 计费且留痕；预留/结算、预估成本均有单元测试和至少一条真实 API 路径验证。
- TOTP 登录链路 E2E：启用 → 登录需二次验证 → 错误码拒绝；会话撤销后旧 token 立即失效。
- Rerank 请求有路由级测试；`/v1/models` 返回元信息字段且与启用渠道聚合一致。
- Playground 三模式均有 Playwright 用例；Settings 分区导航键盘可达。
- `cargo clippy --workspace` 零警告 + `cargo test --workspace` 全绿 + 前端 typecheck/build 通过。

## P2 目标与验收标准

### P2 目标

1. 实时渠道竞速（hedged request，A1）：2–3 渠道并发、首 token 胜出、取消尾请求。
2. 长时任务框架（提交/轮询/取消/产物存取/任务计费），首用例：视频生成任务。
3. Realtime WebSocket（`aigx-net` 实验层已有 WebSocket 池，评估复用）。
4. 渠道自动调参（A3）+ 故障自愈时间线（A4）+ 渠道竞速看板。
5. Artifacts 侧边实时预览画布 + 轻量代码编辑器。
6. OIDC（一个实现覆盖多家 IdP）+ Passkey/WebAuthn（自研）。
7. 订阅制（周期配额包）+ 意图层路由立项（E1）+ 微支付流边界（E2 的 `BillingSink` trait）。
8. 阿里百炼 / 腾讯混元 / MiniMax 三家原生适配器。
9. 会话后端持久化 + 分享链接 + RAG 知识库立项。

### P2 验收标准

- 竞速请求有取消尾请求、超时和故障注入测试。
- 长任务可提交/轮询/取消，产物和计费状态可恢复（重启不丢）。
- 每个新 bridge 有协议级集成测试；每个新依赖先过百年工程红线审查。
- 后端持久化数据结构带版本号字段与迁移路径，旧数据文件无损读取。

## 下一步行动（按顺序）

1. P1 第二波：计费闭环完整性（G1/G2/G5 + G4）→ 会话撤销收尾 → 模型元信息同步。
2. P1 前端体验跃升：Playground V2（三模式 + 参数调节）、Settings 分区式、
   模型预设（Models workspace）+ 提示词库（Prompts workspace）。
3. 每轮迭代出口：clippy 零警告 + test 全绿 + typecheck/build 通过 + 更新 `CHANGELOG-AI.md` + 推进本文件状态。
4. 遗留技术债（P2 清债）：Workspace_B780A2 分支计费语义落后主链路（G9）。


