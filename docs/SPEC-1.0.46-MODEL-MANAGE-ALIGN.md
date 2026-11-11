# SPEC-1.0.46 — 模型管理对齐 Agent2API + 账号模型可见性 + 获取模型修复

> 状态：待团队实施。本文件是**唯一事实源**；实施中任何偏离必须回写本文件 §9。
> 基线 commit：`c65242c0`（v1.0.45）。目标版本：`1.0.46`。

---

## §0 背景与生产实证（不要重新侦察）

生产 `api.lxseek.com` 的库是 **PostgreSQL**（`postgres://aigx:***@127.0.0.1:5434/aigx`），
**不是** `/root/.aigx/data/aigx.db` 那个 SQLite 文件 —— 后者是陈旧残留（810 行），
曾导致误判。**任何"生产实查"必须走 PG。**

PG 实查（2026-11-11）：

| 账号 | 渠道 | provider | status | models |
|---|---|---|---|---|
| 雨沐🍀 `7ae8a2d8` | `629704ff` 智谱 Z.ai Coding 账号池 | workbuddy | disabled | `[]` |
| 乂 `83c54a4b` | `765afd27` WorkBuddy 国内版 账号池 | workbuddy | enabled | `[]` |

- 两个账号型渠道的 `models` / `discovered_models` 也全是 `[]`。
- `model_rules` = 9 条 disabled（`openai_compatible` × `auto` / `deepseek-v4.1-flash` / `glm-5.3` …）。
- `model_custom_map` = `{"auto":"spark-x2.6","deepseek-v4.1-flash":"glm-5.3-flash"}`。

nginx 访问日志铁证（`/www/wwwlogs/api.lxseek.com.log`）：
- `11/Nov/2026:06:39:49 POST /api/account-sessions/catalog/sync 200`
- `11/Nov/2026:06:40:12 POST /api/account-sessions/login/start 200` → 之后 `login/wait` 轮询
  （**登录型建号**，非直连建号）
- `11/Nov/2026:06:44:03` 与 `06:44:05` `POST /api/channels/fetch_models` → **400 / 68 字节**
  （68 字节 = `{"success":false,"error":"Auth failed: HTTP 403 (invalid api key?)"}`，正好 68）

---

## §1 三条根因（已定案，勿再论证）

### 根因 1 — 账号模型不可见
`build_view`（`src/model/manage.rs:292-296`）与 `collect_available_models`
（`src/api/openai/models.rs:54-58`）在账号型渠道下**只读 `session.models`**，
空则回落 `channel.effective_models()` —— 账号型渠道的该字段也是空 → 一行不贡献。

而白名单为空的原因有二，**两条都要修**：
1. **登录型建号不触发模型发现**：`src/account_login/mod.rs:570-592` 直接
   `AccountSession::new(...)` + `upsert`，`models: []` 落盘即止。
   只有**直连建号**路径 `src/api/admin/account_sessions.rs:873-919` 才 `tokio::spawn`
   补一次目录。
2. **WorkBuddy 没有目录实现**：`src/account_session/catalog.rs` 的
   `spec_for()`（`:376`）与 `supported_providers()`（`:1328-1360`）**都没有 workbuddy /
   workbuddy-intl** → `catalog::supports("workbuddy") == false` → 落到
   `discover_models_plain()` 去请求 `https://copilot.tencent.com/models`（不存在）→ 失败。
   Agent2API 对 WorkBuddy 用的是**内置静态目录** `core/models/mod.rs::global_catalog(region)`
   （`desktop-tauri/src-tauri/server/src/server/core/models/mod.rs:763`），
   种子清单含 `deepseek-v4-pro` / `deepseek-v4-flash` / `deepseek-v4.1-flash` /
   `deepseek-v3-2-volc` / `hy3-preview` / `hy3-preview-agent` / `hunyuan-chat` /
   `hunyuan-2.0-instruct` / `hunyuan-2.0-thinking` 等（`models/mod.rs:104-152`）。

### 根因 2 — 「获取模型」必然 400
`frontend/src/pages/ModelManage.tsx:1024` 只发 `{ channel_id }`。
`src/api/admin/legacy.rs:1410-1457` 对 `api_key` 做了"从存储渠道兜底"（`:1430-1440`），
但对 `channel_type` 与 `base_url` **没兜底** → `ChannelType::from_str_lossy("")`
落到 `_ => Self::Cloudflare`（`src/channel/mod.rs:89`）→ 拿 `cf_binding_url`
（`https://cf-ai-gw.lxseek.workers.dev`）去请求 `/v1/models` 且不带 key → 上游 403 → 400。
**该路径对任何渠道都是坏的**。

### 根因 3 — 「已启用」是独立开关，A2A 里它是从映射派生的
AIGX 两套存储并存：`model_rules.disabled`（行级开关，键 `model_rules`）
与 `model_custom_map_disabled`（别名开关）。
A2A 只有一套：`mappings[]`，每条一开关；**行级启用 = 该行还有任一条映射开着**
（`models-panel-state.ts:767-769` `rowEnabled = bindingsOf(m).some(b => b.enabled !== false)`），
且模型自身的 `enabled` 就是"默认绑定（id→id 自映射）"那颗开关
（`providers/catalog/view.rs:172-178`）。A2A 无 `enable` 列。

---

## §2 冻结契约（跨成员接口，不得自行改动）

### §2.1 `GET /api/models/manage` → `ManageModel` 增字段
```rust
// src/model/manage.rs 的 struct ManageModel
pub enabled: bool,          // 【语义变更】派生值：默认绑定开着 OR 任一条别名开着
pub default_enabled: bool,  // 【新增】默认绑定（id→id）那颗开关 = !rules.is_disabled(provider, id)
```
- `default_enabled` 的写入仍走既有 `POST /api/models/state`（body `{provider, id, enabled}`，
  `src/api/admin/model_manage.rs:108-129`）→ `model_rules.set_enabled`。**存储不变、零迁移。**
- 非管理员分支（`model_manage.rs:72-76`）保持"只保留 `m.enabled` 且在分组白名单内"的口径，
  `default_enabled` 一并按 `m.enabled` 过滤后的集合输出。

### §2.2 `GET /api/account-sessions` → `AccountSessionItem` 增字段
```rust
pub struct ModelsStatus {
    pub state: String,        // "whitelist" | "catalog" | "none" | "failed"
    pub count: usize,         // 当前可调度模型数（白名单优先；空则目录缓存计数；否则 0）
    pub error: Option<String>,// state=="failed" 时的原因（人类可读，中文）
}
// AccountSessionItem
pub models_status: ModelsStatus,
```
语义：
- `whitelist` — `session.models` 非空。
- `catalog` — 白名单空，但目录缓存 `model_catalog:{provider}:{account_id}` 有内容。
- `none` — 两者都空且该 provider `catalog::supports()` 为 false（**不支持发现**）。
- `failed` — 支持发现但最近一次探测失败（`error` 写原因）。

### §2.3 `POST /api/channels/fetch_models`
- 后端：`body.channel_type` 为空时从 `channel_id` 对应渠道取 `channel_type.as_str()`；
  `body.base_url` 为空时取渠道 `base_url`。**兜底后仍必须过 `net_guard::validate_outbound_url`**
  （不能因兜底绕过 SSRF 守卫）。
- 兜底后 `channel_type` 仍为空（既无 body 也无 `channel_id` 可查）→ **明确 400**
  （如 `channel_type is required`），不允许空串流进 `ChannelType::from_str_lossy` 的
  `_ => Cloudflare` 分支（`src/channel/mod.rs:89`）——该落点是本次 400 事故的放大器。
- **前端不改**（2026-11-11 裁决，理由见下）。`ModelManage.tsx` 的 `fetchOne` 保持只发
  `{ channel_id }`。

  **为什么不做「前端也一并发送 channel_type/base_url」**（本文件初稿曾这么写，已撤销）：
  1. 该端点的 body 参数语义是「未保存的草稿」（Channels 页新建渠道时先试拉模型），
     而 `channel_id` 语义是「已保存的渠道」。模型管理的「获取模型」弹窗列的全是**已保存**
     渠道，应用后者语义 —— 只有一个事实源（存储里的渠道）。
  2. 前端 `fetchChannels` 里那份 `ChannelItem` 可能是**过期快照**（别的标签页刚改过 base_url）。
     一旦前端发过去，后端按「body 优先」用过期值，**静默打错上游** —— 比现在这个响亮的 400 更糟。
  3. 因此根因回归护栏**不能**写成 e2e 断言「请求体含 channel_type/base_url」（那反而会把
     错误设计钉死）；正确护栏是**后端纯函数单测**（见 §6.4）。

---

## §3 后端改造清单（成员 `be-core`）

1. **`src/account_session/catalog.rs`**
   - `spec_for()` 增 `"workbuddy"` 与 `"workbuddy-intl"`：`path` 留空 / 用一个不可能成功的探测，
     关键是 **`fallback` 填 WorkBuddy 静态目录**（典据：Agent2API `core/models/mod.rs:104-152`）。
     按 AIGX 现有 `FallbackModel { id, upstream_key, reasoning, vision, tool, context_window }`
     逐项映射；Agent2API 的 `maxOutputTokens` AIGX 结构没有 —— 若要用需扩结构，
     **本期允许只填 `context_window`（取 `maxInputTokens`）**，并在 §9 记录这条窄口径。
   - `supported_providers()` 同步加 `"workbuddy"` / `"workbuddy-intl"`（有断言测试会红，是刻意的）。
2. **登录型建号补目录**：把 `src/api/admin/account_sessions.rs:873-919` 那段
   "白名单为空 → 探测 → `apply_catalog_to_session`" 抽成一个可复用函数（如
   `spawn_catalog_probe(state, account_id)`），在
   `src/account_login/mod.rs:583-592` 建号成功后同样调用。
   **禁止抄第二份**（AIGX 有"唯一实现"文化，见 `src/api/openai/models.rs:249-303` 的
   反向断言测试先例）。
   - 探测失败**不再静默**：写 `tracing::warn!`，并把失败原因落到 §2.2 的 `failed` 态
     （存储键建议 `model_catalog_error:{provider}:{account_id}`，或复用 catalog 快照结构，
     自选但要在 §9 记录）。
3. **模型聚合回落目录缓存**：
   - `src/model/manage.rs::build_view` 账号分支（`:292-296`）：
     `session.models` 为空时 → 读 `model_catalog:{provider}:{session.id}` 的 id 列表；
     仍为空 → 保留现有 `channel.effective_models()` 回落。
   - `src/api/openai/models.rs::collect_available_models` 账号分支（`:54-58`）**同口径**，
     否则 v1.0.45 建立的"客户 API 与控制台同源"不变量会被打破。
   - `src/api/openai/models.rs` 末尾 `mod shared_catalog_tests` 增/改断言。
4. **`row.enabled` 派生语义**（§2.1）：`build_view` 收尾处（`:355-374`）
   `row.default_enabled = !rules.is_disabled(provider, id)`；
   `row.enabled = row.default_enabled || alias_entries.iter().any(|e| e.enabled)`。
   `ProviderGroup.enabled` 统计沿用新的 `row.enabled`。
5. **`src/api/admin/legacy.rs::handle_fetch_channel_models`**（§2.3）兜底。
6. **`src/channel/mod.rs::from_str_lossy`**（`:82-91`）：空串/未知回落 `Cloudflare` 是本次事故的
   放大器。**改为显式区分**：新增 `from_str_lossy_strict` 或让 `""` 回落 `OpenaiCompatible`
   —— 二者择一，且必须检查所有调用点不会被破坏（`grep -rn from_str_lossy src/`）。
   若判断风险大，**允许不改**，但要在 §9 写明"靠调用方永远传全字段"的约定。

**Rust 验证纪律**：本地只跑 `cargo fmt --all` + `cargo fmt --all -- --check`。
编译/测试/clippy **一律交 CI**（用户硬约束）。CI 绿之前不算完成。

---

## §4 前端改造清单（成员 `fe-table`，文件 `frontend/src/pages/ModelManage.tsx` / `.css`）

1. **删「启用」列**
   - `COL_ORDER`（`:79`）去掉 `'enable'` → `['check','model','price','source','caps','alias','act']`。
   - `COL_PANEL_LABEL` / `COL_DEFAULT_W` / `COL_DEFAULT_ALIGN`（`:90-118`）同步删 `enable`。
   - 删 `handleToggle`（`:536-549`）之外的列渲染 `case 'enable'`（`:1397` 一带）与
     `.mm-row-switch` / `.mm-c-enable` 样式（`ModelManage.css:347`）。
2. **「映射」列内置「默认绑定」开关**（照 A2A `models-page.tsx:554-563` + `.binding-default`）
   - `case 'alias'`（`:1529-1614`）最前面插一颗 chip：`{row.id} → {row.id}`，
     class `mm-binding-default`，开关 = `row.default_enabled`，
     onChange → `api.setModelEnabled(row.provider, row.id, next)`。
   - 其后接现有别名 chips（alias → target + 开关 + 删除）。
   - chip 文案/样式对标 A2A `.binding-default`（`page-gateway.css:429`：`font-size:10px`、
     边框、`--r-xs` 圆角）。
   - 空映射时不显示 A2A 那句"原样透传"只读提示的语义要重新校准：**默认绑定 chip 常驻**。
3. **行启用态与筛选/统计**
   - 行压淡 `mm-off`（`:1729`）改用 `row.enabled`（派生）。
   - `StateFilter`（`all|on|off|mapped`，`:74`）与面板统计（`:1250-1256`）口径同步为新 `enabled`；
     `mapped` 仍按"有别名映射"判。
4. **列宽 / 字号 / 对齐对齐 A2A**（典据 `Agent2API/desktop-tauri/ui/css/page-gateway.css`）
   - A2A 列宽：`check 3.5% / model 19% / rate 5.5% / source 6.5% / budget 10% / caps 11.5% / alias 33% / act 11%`
     （`:224-237`）。AIGX 保留「定价」列占 A2A `rate` 的位置：建议
     `check 3.5% / model 19% / price 8% / source 6.5% / caps 11.5% / alias 33% / act 11%`
     （合计 92.5%，余量给 model 或 alias；实施时按实际观感定值并回写 §9）。
   - 字号：表 `12.5px`（已同）、`thead th 10.5px`（已同）但**去掉 `letter-spacing: 0.05em`**
     （A2A `page-gateway.css:195-197` 明确为中文去掉字距）；`td padding 8px`（AIGX 现为 `7px 10px`
     → `8px`，横向保持 10px 以免列太挤，取 `8px 10px` 并回写 §9）。
   - 单元格字体（A2A `page-gateway.css:289-303, 354`）：
     模型名 `font-family: mono; 12px; weight 600`；价格/倍率 `mono; 11.5px; tabular-nums`；
     来源 `11.5px`；映射别名 `mono 12px`。AIGX 对应用 `.mm-c-model` / `.mm-c-price` /
     `.mm-c-source` / `.mm-c-alias` 落到同档。
   - **保留「定价」列**（本项目拍板）：不改成 A2A 的「倍率」列；倍率信息在定价弹窗内已有。
5. **账号来源提示**：来源列 `kind==='account'` 的 tag 悬停文案补充"该账号当前生效模型 N 个"，
   数据来自 §2.2（若 `sources` 里没有，则不加，别硬塞）。

---

## §5 前端改造清单（成员 `fe-account`，文件 `frontend/src/pages/AccountSessions.tsx`）

1. 账号行/详情展示 §2.2 的 `modelsStatus`：
   - `whitelist` → 「模型清单：白名单 N 个」
   - `catalog` → 「模型清单：目录发现 N 个」（提示这是自动发现的，可在编辑弹窗改成白名单固化）
   - `none` → 「模型清单：无（该供应商不支持自动发现，请手填）」
   - `failed` → 「模型同步失败：{error}」+ 重试按钮
2. 「同步模型」按钮：调既有 `POST /api/account-sessions/catalog/sync`
   （`src/api/admin/account_sessions.rs:2017` `handle_sync_account_catalog`），
   成功后重载并 toast 结果。
3. 不新建接口；所有字段来自 §2.2。

---

## §6 e2e 与门禁（成员 `verify`）

- 必跑门禁（前端）：`npx --no-install tsc --noEmit`、`npx --no-install eslint .`、
  `npx --no-install vitest run`、`npm run i18n:check`、
  `node scripts/i18n-interpolation-check.mjs`、`npm run build`。
- e2e：`E2E_PORT=4174 npx playwright test --trace=off`（本地可跑，不编译 Rust）。
  **列结构变更必然打破现有选择器/断言**（尤其 `mm-c-enable` / `.mm-h-enable` /
  列设置面板的列名快照），逐条修。
- 新增 e2e 覆盖（新建或并入现有 spec）：
  1. 模型管理**不存在**「启用」列；「映射」列首颗是默认绑定 chip；
     关掉它 → 行压淡；行还有别名开着时**不**压淡（A2A 口径）。
  2. 账号池能看到 `modelsStatus` 四种态的渲染（用 mock，不依赖真上游）。
  3. 「获取模型」单渠道走通：mock `/api/channels/fetch_models` 返回 200 →
     断言**请求体只含 `channel_id`**（对齐 §2.3 的裁决：前端不发明细，避免过期快照覆盖存储值），
     且弹窗把返回的模型渲染出来 / 失败时显示失败态。
     **不要把「请求体含 channel_type/base_url」写成断言** —— 那是错误设计（见 §2.3）。

### §6.4 获取模型的**根因**护栏在后端（不在 e2e）
`channel_id`-only 的兜底语义是**后端行为**，e2e 用的是 API 桩，测不到。正确护栏是
**Rust 纯函数单测**（成员 `be-core`）：把 `handle_fetch_channel_models` 里
「body → 存储渠道」的解析逻辑抽成一个纯函数（如
`resolve_fetch_target(channel_type, base_url, stored_channel) -> Result<(ChannelType, String, String), &'static str>`），
断言：① 两者都空 + 有 stored channel → 取存储值；② 两者都空 + 无 stored channel → Err；
③ body 显式给了值 → body 优先（草稿语义）。纯函数便于单测，与 AIGX 既有的
`must_change_password` 门禁同一手法。本地**不跑**，交 CI。

- 后端验证：**只跑 `cargo fmt --all -- --check`**；编译/测试/clippy 交 CI。

---

## §7 交付与发版（lead 负责）

1. 版本号 6 处：`VERSION`、`Cargo.toml`、`Cargo.lock`、`frontend/package.json`、
   `frontend/package-lock.json`（2 处）→ `cd frontend && npm run build` 生成 `static/version.json`。
2. CI 必须 12/12 绿 + Multi-platform Build 5 平台全 success（Release 才有 15 资产）。
3. 生产部署（`104.223.65.202:22`）：下载+sha256 → SFTP → 备份 → staging 解压 static
   （**包内顶层自带 `static/`，解压到父目录**）→ 原子 mv → 单次 `systemctl restart aigx` →
   健康检查（`/`、`/index.html`、`/version.json`、`/readyz`、`/livez`）→ 失败自动回滚
   （回滚分支先 `test -d $BK` 自检）。
4. 冒烟必须包含：模型管理无启用列、映射列有默认绑定、账号池能看到 WorkBuddy 账号的模型态、
   「获取模型」返回 200 而非 400。

---

## §8 不做清单（防止范围蔓延）

- 不做 `hidden`（删除/恢复）机制 —— A2A 已明确移除。
- 不改 A2A 的 provider 目录（左栏）结构 —— 本次不动左栏。
- 不改 `model_rules` 的存储键与结构（零迁移是硬约束）。
- 不把「定价」列换成「倍率」列。
- 不做规避上游检测类功能。

---

## §9 实施偏离记录（成员实施中追加，勿删）

### fe-account（账号池模型状态展示 + 手动同步入口）

- **契约字段名 `models_status`（snake_case）已确认**（非偏离）：后端 `session_json`
  （`src/api/admin/account_sessions.rs:641`）用 `json!` 显式 snake_case 键输出，故
  §2.2 的 `models_status` 序列化名即 snake_case；前端类型与读取均用 `models_status`。
- **展示位置**：四态文案渲染在「账号」列（`AccountSessions.tsx` 的 `case 'name'`
  单元格尾部，`modelStatusNode`），非新增列/详情抽屉——最小改动，不动列定义
  （`AS_COLS`）与多人共用的表结构。
- **「重试」按钮**复用既有 `handleSyncCatalog`（POST `/api/account-sessions/catalog/sync`
  → 重载 + toast）；操作列既有 Download「同步模型」按钮即 §5.2 的同步入口，
  未新增接口、未新增按钮语义，仅 `failed` 态多一个行内重试。
- **i18n**：复用既有键 `重试` / `同步中...`；新增 5 键（四态文案 + catalog/none 的
  title 提示 + `未知原因`），zh/en 同步补齐。
- **容错**：`models_status?` 可选，后端未落地（当前 `session_json` 尚未下发）时
  `modelStatusNode` 返回 `null`，不渲染、不占位、不崩。

### fe-table（模型管理表格改造）

- **列宽定值（§4.4 授权"按实际观感定值"）**：`check 3.5% / model 20% / price 8% /
  source 6.5% / caps 11.5% / alias 39.5% / act 11%`（合计 100%）。SPEC 建议值合计 92.5%，
  余量 7.5% 按 §4.4「给 model 或 alias」分给 model +1%（20%）、alias +6.5%（39.5%）。
  `check`/`act` 由原 `34px`/`132px` 改为百分比，与 A2A 一致；副作用是公共列宽层
  （`tableColumns`）不再有 px 默认列参与拖拽记忆，拖拽/双击还原行为不变。
- **`td` padding**：`7px 10px` → `8px 10px`（§4.4 建议 8px；横向保 10px 防列挤）。
  表头去掉 `letter-spacing: 0.05em`。
- **非列宽字号档**：`.mm-price` 11→11.5px、`.mm-src-tag` 10.5→11.5px、
  `.mm-alias-text` 11→12px（§4.4 指定"映射别名 mono 12px"，A2A 实际 `.alias` 为 11px，
  此处遵 SPEC 取 12px）。模型名 `.mm-mid` 12px/600 原已达标，未动。
- **默认绑定 chip**：`mm-alias mm-binding-default`（复用别名 chip 外壳），首颗常驻；
  关闭态走 `.is-off`；「默认」小标 `mm-binding-default-tag` 对标 A2A `.binding-default`
  （10px / 细边框 / 4px 圆角）。名字用 `mm-binding-name` 撤掉 `.mm-alias-name` 的
  指针/悬停下划线（不可点，开关在 chip 里）。
- **删除 identity 提示**：原「模型名→自己（原样透传）」只读 chip（及 `hiddenIdentity` /
  `HIDE_IDENTITY_KEY` / `loadHiddenIdentity` / `hideIdentity`）随默认绑定 chip 上线一并移除
  ——默认绑定 chip 已常驻覆盖该语义（§4.2）。相关 CSS `.mm-alias-identity*` 删除。
- **容错**：`default_enabled` 按冻结契约在 `ManagedModel` 上为必填；渲染取
  `row.default_enabled ?? row.enabled`，后端新字段未落地的窗口期内不至于整列误判为关闭
  （非第二套语义，仅回落旧同源字段）。
- **旧 localStorage `enable` 残留**：公共层 `sanitizeItems`/`applyTableColumns` 本就丢弃
  未声明键；另在旧键 `aigx-mm-colcfg` 的一次性迁移过滤器里显式追加"键必须是当前列声明之一"，
  从源头挡掉 `enable`，双保险无幽灵列。
- **来源列账号模型数提示（§4.5）未做**：`ManagedModelSource`（§2.2 只给 `AccountSessionItem`）
  不含 `modelsStatus`，`sources` 里没有该数据；按 §4.5「没有则不加，别硬塞」跳过。
- **本地门禁**：`tsc --noEmit` 0、`vitest run` 128/128 通过、`npm run i18n:check` 0 缺失、
  `i18n-interpolation-check` PASS。e2e（选择器 `mm-c-enable`、`models.spec.ts:124` 的
  `td.nth(1)` 开关等）交 verify（#4）。

### be-core（Rust 后端）

1. **WorkBuddy 静态目录 = 种子清单过滤后 23 条（移植时按数据裁掉 4 条非对话项）。**
   典据取 `Agent2API\...\core\models\mod.rs` 的 `builtin_models()`（v3.0.8 副本，
   SPEC 引用的就是它；共 27 条）。Agent2API 运行时还会对种子清单套一层
   `is_chat_model` 准入过滤（`models/mod.rs:164-186`：前缀黑名单 + `supportsExtra`
   为真即排除 + `maxOutputTokens >= 16000`）。**AIGX 侧不实现该运行时过滤**
   （`FallbackModel` 无 `maxOutputTokens` 字段，硬做只能造假判据），改为**移植时按
   数据裁掉**被过滤的 4 条，留下 **23 条**。被裁的 4 条与理由（**不是遗漏，将来
   「补全目录」时不要加回来**）：
   - `kimi-k2-instruct-taiji` —— 命中前缀黑名单 `kimi-k2-instruct`；
   - `hunyuan-chat` —— `maxOutputTokens = 8192 < 16000`（网关只服务对话模型）；
   - `default-1.1` / `default-1.2` —— 命中前缀黑名单 `default-1.`（Claude 内部 id）。
   `workbuddy` / `workbuddy-intl` **共用同一份 23 条**（A2A 的 `builtin_models()`
   是 region 无关的单一函数，`initial_state(region)` 只影响 `/v3/config` 远程刷新）。
   *注意*：上游 v3.4.2 已把 WorkBuddy 目录搬进独立插件仓 `agent2api-providers`，
   主仓不再持有种子清单 —— 故只能取 v3.0.8 副本这份。

2. **`maxOutputTokens` 本期不承载**：`context_window` 一律取种子清单的
   `maxInputTokens`（**输入**窗口）。SPEC §3.1 已授权此窄口径，未扩结构。

3. **`spec_for("workbuddy"/"workbuddy-intl")` 的 `path` 留空**（与 autoclaw /
   orcaterm / zcode / deepseek-web 同一形态）：WorkBuddy 无可拉取的 `/models`
   目录端点。空 path 会让 `sync_account_catalog` 先发一次注定失败的探测请求，
   失败后回落静态兜底表 —— 与既有各家静态兜底行为一致，未新增短路分支。

4. **`models_status` 的 `failed` 兜底文案**：§2.2 只定义四态。当某家
   `catalog::supports()==true` 但“白名单空 + 目录缓存空 + 无失败记录”（**尚未探测过**）
   时四态无法自然落位，归入 `failed` 并给合成原因
   `尚未同步模型目录（可点「同步模型」重试）`（保证 `state=="failed"` 时 `error`
   恒非空，前端不会拿到 `undefined`）。失败原因落盘键用 SPEC §3.2 建议的
   `model_catalog_error:{provider}:{account_id}`；成功时删除该键。

5. **§2.2 的 `AccountSessionItem` 在 AIGX 不是 Rust struct**：账号列表由
   `session_json()`（`src/api/admin/account_sessions.rs`）手工拼 `serde_json::Value`。
   故 `models_status: ModelsStatus` 以 JSON 字段下发（wire 形态 = §2.2 冻结契约），
   `ModelsStatus` 结构定义在 `src/account_session/catalog.rs`。`session_json` 增了
   `&AccountSessionStore` 形参（4 个调用点同步）；建号/更新接口亦返回该字段。

6. **`cached_model_ids` 按“存在性”读目录缓存（不看 TTL）**：它是**可见性**兜底，
   不是新鲜度判据（新鲜度仍由 `sync_account_catalog` 的 `CACHE_TTL_SECS` 把关）。
   `build_view` 与 `collect_available_models` 两处调用**同一函数**，口径唯一（§3.3）。

7. **§3.6 `channel/mod.rs::from_str_lossy` 未改（选择 SPEC 允许的“不改”分支）**，
   改在**调用方的边界闸**上补防：
   - **为什么不改默认值**：本事故的放大路径（`fetch_models` 收空 `channel_type` →
     `Cloudflare`）已在 §3.5 于调用方堵死；其余调用点里 `channels.rs` 两处受
     `KNOWN_CHANNEL_TYPE_FILTERS` 守卫、改默认值无影响，而 `to_channel`（新建）与
     `legacy.rs` PATCH 改默认值会变更“空串建渠道”的既有语义，风险大于收益。
   - **补的边界闸**：`handle_fetch_channel_models` 里，兜底之后若 `channel_type` 仍
     无从得出（body 空 **且** `channel_id` 查不到渠道），**直接 400
     `channel_type is required`**，不让空串流到 `from_str_lossy` 的 `_ => Cloudflare`
     兜底上（那正是线上 400/68 字节事故的落点）。这比在文档里约定“调用方必须传全”
     更可靠 —— 由代码承担，而非约定。
   - **实现形态**：该解析抽成纯函数
     `resolve_fetch_target(body_channel_type, body_base_url, stored: Option<&Channel>)
     -> Result<(ChannelType, String), &'static str>`（`src/api/admin/legacy.rs`），
     并配 3 个单测（SPEC §6.4）：① 都空+有 stored → 取存储值（**根因护栏**）；
     ② 都空+无 stored → `Err("channel_type is required")`；③ body 显式值 → **body
     优先**（方向性断言：body 与 stored 取不同值，写反即失败）。

8. **`fetch_models` 的 `api_key` 兜底顺带改用“首个逗号分段 id”**：原代码用原始
   `body.channel_id`（`"ch1, ch2"` 时查不到）。现与 `channel_type`/`base_url` 共用
   同一个 `stored_channel`（`ids.first()`）。单 id 行为不变，多 id 严格变好。

9. **登录型建号的探测触发点落在 `persist_account`（唯一漏斗）**，而非 §3.1 字面写的
   `mod.rs:583-592`：`persist_account` 是 5 个登录/建号入口（回调、轮询、手工提交
   回调、短信、paste）的共同落库点，在此触发即“两处共用”（与直连建号共用
   `catalog::spawn_catalog_probe`）。登录新账号白名单恒为空，故**无条件**触发
   （不再判 `models.is_empty()`）。探测失败改 `tracing::warn!` + 落 `failed` 态
   （原为静默 `debug!`）。

10. **`supported_providers()` 新增 `workbuddy`/`workbuddy-intl` 对漂移闸门无影响
    （已核实，非虚惊）。** `tools/sync_agent2api_capabilities.py` 的 `dirty` 只由
    `checkin_missing / checkin_extra / catalog_missing / catalog_endpoint_diff`
    四项决定，**没有 `catalog_extra`**（AIGX 多出来的目录家不计入失败）；且
    `catalog_endpoint_diff` 只遍历 `up_catalog & ai_catalog` 的交集，而 workbuddy
    上游没有 `providers/workbuddy/models.rs`、永不进交集。故三个 `--check` 门禁不受
    影响，**不改脚本、不加豁免**。

11. **补充对照事实（来自上游 3.4.2，印证“静态表只是兜底”的位置）**：3.4.2 的
    `core/model_rules/workbuddy.rs` 模块头写明，官方客户端 picker 名单来自
    `/v3/config` 的 `agents[cli].models`（实测 18 条，而 `data.models` 有 60 条），
    静态内置表只作「无登录态时的兜底」。**AIGX 这张 WORKBUDDY_FALLBACK 也是兜底，
    真新鲜度靠账号目录拉取**（`sync_account_catalog` → 远程/缓存/兜底），故表内容
    不必追新追全。另：3.0.8 副本是当前**唯一**含该种子清单的权威源（3.4.2 已把
    provider 目录挪进闭源插件仓；`core/providers/catalog_legacy.rs` 只是旧缓存
    scope→provider 映射表，无模型数据），不用再找更新的源。
