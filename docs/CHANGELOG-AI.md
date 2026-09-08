
## 2026-09-08 · B2 成本预估器：原始消息直出预估

### 做了什么
- `src/api/admin/pricing.rs`：`POST /api/pricing/estimate` 新增「消息形态」——
  只传 `model` + `messages`（OpenAI wire 形状），后端用
  `token_estimate::count_chat_prompt` 估算 prompt token，completion 默认 256
  （与数据面预留计费口径一致）；旧「token 形态」完全兼容，两种形态以
  `token_source` 字段区分。token 解析抽成纯函数 `resolve_estimate_tokens`。
- `src/api/openai.rs`：`parse_messages` 提升为 `pub(crate)`，预估与数据面共用
  同一消息解析路径，杜绝口径漂移。
- 测试：`resolve_estimate_tokens` 6 个单测（消息估算/显式 output 覆盖/非法角色
  拒绝/显式透传/单 input 默认 output/空请求拒绝）。

### 为什么
- 旧实现要求前端自带 tokenizer 先算 token 再请求，前端负担重且口径可能漂移。
  网关侧已有 `count_chat_prompt`，让预估与真实计费共用一条路径是零漂移方案。

### 验证结论
- `cargo clippy --workspace --all-targets` 零警告。
- `cargo test --workspace` 全绿（lib 378 + 新增 6）。
- 未触碰同事并行编辑的前端文件（App.css / network.ts / vite 配置）。

## 2026-09-08 · P1 第二波（1/N）：计费闭环修复 G1/G5

### 做了什么
- `src/user/mod.rs`：UserStore 新增 `release_quota()`——纯解冻预留，不产生消费。
- `src/api/auth.rs`：ApiKeyStore 新增同名 `release_quota()`，语义对称。
- `src/api/openai.rs`：
  - `reserve_usage()` 的 key 预留失败回滚从 `settle_quota(..., 0)` 改为 `release_quota()`（G1：settle 的并发钳制可能归还他人预留）。
  - 新增 `release_reservation()` 帮助函数。
  - 流式与非流式 chat 的失败路径（渠道全败/4xx 客户端错误）在返回前调用 `release_reservation()`（G5：原先预留永久冻结）。
- 测试：UserStore 3 个（reserve_then_settle / reserve_then_release / release_clamped），ApiKeyStore 3 个（同名三件套）。

### 为什么
- 二轮审查发现的 G5 是确定性泄漏：每次上游失败都会永久冻结用户/Key 额度，必须立即修复。
- G1 的回滚语义错误在并发场景会错还他人预留，release 与 settle 必须分离。

### 否决方案
- 不为其它端点（responses/completions/embeddings/rerank/images）立即接入预留——它们尚未启用两段式，保持旧路径一致，待 P1 收尾统一切换。

### 验证结论
- `cargo clippy --workspace` 零警告。
- `cargo test --workspace` 全绿：363（lib）+ 355（bin）+ 3（aigx-net）。
- 我改动的三个文件在 `cargo fmt --check` 下无新增 diff（仓库既有 fmt 差异为并行会话未提交代码，未触碰）。
- 新增 6 个测试全部通过。
# AI 跨会话迭代日志

## 2026-09-08 · 二轮头脑风暴：批判式审查 P1 已落地代码

### 做了什么
- 以 P1 第一波落地代码为靶子做二轮审查，产出 `BRAINSTORM-IDEAS.md` G 类清单（G1–G9）。
- 修复 `ROADMAP.md` 三处状态滞后：TOTP 全链路（后端接口 + 前端 Login/Security + SeaORM 迁移）已落地但路线图仍列待办；P0 宣称「已完成」但 `/chat` 页面与路由从未落地；P1 第二波补充计费闭环完整性五项。

### 关键发现
- 🔴 G5 疑似 bug：chat 失败路径建立预留后未释放，可能永久冻结用户/Key 额度。
- 🔴 G1 回滚语义：reserve 的 key 失败回滚用 settle 当 release，并发场景可能归还他人预留。
- ⚠️ G1：`/v1/rerank` 仍走旧式事后计费，与主链路预留/结算语义分裂。
- ⚠️ G2：预留无超时回收，进程崩溃会永久冻结。
- ⚠️ G4：缓存命中计费后记账维度未走 cache_read，省钱看板（B1）拿不到准确数据。
- ⚠️ P0 状态：`/chat` 用户聊天工作区（P0 第一目标）未落地，验收标准未满足。

### 为什么
- 头脑风暴不应只往前想，也要回头看——对着已落地代码找弱点比提出新点子更能逼近「百年完美」。
- 文档是跨会话交接棒，宣称的进度必须与仓库实况一致，否则下一个会话会基于错误前提继续。

### 否决方案
- G6 维持 cron 单任务单协程现状（任务 ≤20 不提前优化），仅记录决策理由供未来维护者参考。

### 验证结论
- 本轮仅文档审查与路线图修正，未改业务代码。
- 发现均标注了证据位置（行号/文件），待所有者裁决后进入 P1 第二波修复。

---

## 2026-09-08 · 第二阶段编码：P1 后端能力落地（含 TOTP 补全）

### 做了什么

**1. 缓存命中差异化计费（P1-1）**
- `src/pricing/mod.rs`: 新增 `calculate_cost_with_cache()` 和 `calculate_cost_quoted_with_cache()` 方法，支持 `cache_price` 差异化计费
- `src/api/openai.rs`: 在缓存命中分支接入新计费方法，命中时按 `cache_price` 计费而非免费
- 新增 4 个单元测试验证计费逻辑（cache_price 未配置保持免费、配置后按缓存读价格计费、count 型不受影响、旧签名委托兼容）

**2. Rerank 端点（P1-8）**
- `src/bridge/mod.rs`: 新增 `RerankRequest`/`RerankResponse`/`RerankResult` DTO 和 `Bridge::rerank()` trait 方法（默认返回 Config 错误，failover 循环切换到下一渠道）
- `src/bridge/openai.rs`: OpenAI 兼容 bridge 实现 rerank 方法，POST `{base}/rerank`，兼容 Cohere/Jina 风格请求/响应形状
- `src/main.rs`: 注册 `/v1/rerank` 和 `/rerank` 路由
- 新增 2 个单元测试验证请求体形状和响应解析逻辑

**3. 配额预留/结算两段式（P1-3）**
- `src/user/mod.rs`: User 结构体新增 `reserved_quota` 字段；新增 `reserve_quota()`/`settle_quota()` 方法；`remaining()` 改为扣除已用+已预留
- `src/api/auth.rs`: ApiKey 结构体新增 `reserved_quota` 字段；`is_quota_exhausted()` 改为含已预留；新增 `reserve_quota()`/`settle_quota()` 方法
- `src/api/openai.rs`: 新增 `Reservation` 结构体和 `reserve_usage()`/`settle_usage()` 函数；`StreamBillingState` 新增 `reservation` 字段；`handle_chat_completions` 在请求前调用 `reserve_usage()`，流式/非流式完成后调用 `settle_usage()` 结算
- `src/api/Workspace_B780A2.rs`: Workspace_B780A2 分支暂走旧路径（`reservation: None`），后续接入

**4. 并行协作纪律（AI-UPGRADE-PROMPT.md）**
- 新增第 7 条：动手前重新读取文件当前内容、改动保持内聚、不删除看不懂但能编译的代码、以仓库现状为准调整方案、禁止破坏性 git 操作

**5. 2FA/TOTP（P1-5，补全）**
- 新建 `src/auth/mod.rs` + `src/auth/totp.rs`：自研 RFC 3174 SHA-1 + RFC 2104 HMAC + RFC 6238 TOTP + RFC 4648 Base32（不引 crate，约 300 行纯函数无 unsafe）
- 测试向量锁死：SHA-1（NIST/RFC 3174）、HMAC-SHA1（RFC 2202 三组）、TOTP（RFC 6238 附录 B 全 6 组）、Base32（RFC 4648 + 往返）
- User 结构加 `totp_secret`/`totp_enabled` 字段（serde default，FileStore 旧 JSON 零迁移）
- 登录流程（密码 + 邮箱验证码两条路径）在用户开启 TOTP 时返回 `require_2fa` + `tmp_token`（`totp_pending_cache` TTL 5 分钟）
- 新增 `POST /api/auth/login/totp`：验证失败计入 login_failures（复用 5 次锁定）；tmp_token 一次性（验证通过即移除）
- 修复并行会话遗留的 `PH_*` 占位符编译错误（reserve_usage/settle_usage 签名、User/ApiKey 初始化补 reserved_quota、anthropic 分支 StreamBillingState 补 reservation: None）

### 为什么
- 缓存命中差异化计费：让缓存省钱可视化，用户可感知缓存价值
- Rerank 端点：RAG 客户刚需，补全数据面能力
- 配额预留/结算：修正「事后扣费」语义，流式请求中途断连不会零计费；预留阶段即可拒绝余额不足请求，避免上游成本由站方承担
- 并行协作纪律：多 AI 会话并行改代码，防止互相覆盖或误删同事进行中的工作

### 关键决策
- 缓存命中计费：`cache_price` 未配置时命中免费（升级零破坏），配置后按缓存读价格计费
- Rerank 实现：归并到 OpenAI 兼容 bridge，不新建独立适配器（参照差距矩阵决策）
- 两段式计费：预留阶段估算 token（prompt 精确估算，completion 默认 256），结算阶段按实际 token 多退少补
- Workspace_B780A2 分支：暂不接入两段式（`reservation: None`），走旧 `charge_usage_with_tools` 路径，降低改动爆炸半径

### 否决方案
- 否决在 Workspace_B780A2 分支立即接入两段式：该分支有独立的流式计费逻辑（content_block 事件），改动风险高，留待后续单独迭代
- 否决引入外部 cron crate：定时系统任务框架（P1-17）用自有实现，符合百年工程红线

### 验证结论
- `cargo clippy --workspace` 零警告
- `cargo test --workspace` 全绿：346（lib）+ 338（bin）+ 3（aigx-net）
- TOTP 正确性由 RFC 官方向量锁死（SHA-1/HMAC/TOTP/Base32 四组标准测试）
- SeaORM 侧的 user 表迁移文件（加 totp 两列）尚未落地：FileStore 后端已零迁移兼容；sea-orm feature 的 DB 实体与迁移是独立路径，记入 ROADMAP 待办
- 待所有者确认后提交；提交后按 ROADMAP.md 进入 P1 下一波：会话撤销 + 模型元信息同步 + 定时任务框架

---

## 2026-09-07 · 第一阶段提案：双轨升级五件套

### 做了什么
- 通读 AI-UPGRADE-PROMPT.md（含第八/九/十章的长期迭代协议与审美定向）。
- 盘点 AIGX 当前状态（后端 8 大模块 + 前端 21 路由页 + 组件库），并抽样核对两个参照项目（new-api 的 relay/service/controller，open-webui 的 chat/playground/workspace 组件目录）。
- 产出第一阶段五份提案文档：
  - `docs/BACKEND-GAP-MATRIX.md`：new-api 八大能力域 × AIGX 差距矩阵，逐项标注采纳/改造采纳/放弃与工作量粗估。
  - `docs/FRONTEND-EVOLUTION-PLAN.md`：open-webui 九大范式 × AIGX 演进路线，标注后端依赖项与玻璃拟态去留决策。
  - `docs/BRAINSTORM-IDEAS.md`：原创增强点清单（hedged request、failover 看板、意图层路由等），含被否决的炫技方向。
  - `docs/ROADMAP.md`：重写为 P0/P1/P2 分期路线图 + 每期验收标准（合并上一轮 P0 草稿与本提案）。
  - `docs/UI-AESTHETIC-MIGRATION.md`：34 项页面/组件改造优先级 + 色彩控件对齐表。
- 更新本迭代日志。

### 为什么
- 本阶段是「先提案后动工」：任务要求第一阶段只输出差距矩阵与路线图，等所有者确认后再进入编码。
- 差距矩阵中明确放弃项（MJ-Proxy、Suno、违规罚金、ClickHouse、语音自建等），防止功能肥大——放弃同样有价值。

### 关键决策
- 采纳分级：OpenAI 兼容上游归并到现有 `bridge/openai.rs`，只给阿里/腾讯/MiniMax 三家原生协议建独立 bridge（P2）。
- 玻璃拟态保留但收敛：品牌层保留毛玻璃，内容区转 open-webui 式干净平面。
- P1 顺序按「价值/成本」排序：缓存差异化计费 → Rerank → 配额预留/结算 → TOTP → 会话撤销。

### 否决方案
- 否决 new-api 式「每家一个适配器目录」的膨胀模式；AIGX 保持 Rust trait 边界。
- 否决引入 JS 插件执行环境（不可审计，违反百年工程红线）。
- 否决 ClickHouse（与单二进制交付冲突）。

### 验证结论
- 本轮为纯文档提案，无代码改动，无编译验证要求。
- 待所有者确认后进入第二阶段编码；编码轮次的出口标准按第七章执行（clippy/test/typecheck/build + 日志）。

