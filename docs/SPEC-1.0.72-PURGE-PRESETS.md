# SPEC-1.0.72 — 破甲预设库（Purge Presets）

> 来源：`D:/GitHub/jailbreaks`（第三方公开破甲 prompt 研究集合，53 个 `.md` / 19 个厂商目录）。
> 目标版本：**v1.0.72**。本文件是该批次的唯一事实源。

---

## 0. 合规边界（先读这个）

- 53 个文件是**第三方公开研究集合的原文**，本批次**只搬不改**：不做内容过滤、不做改写、不做截断。
- 库内默认词表只含中性/合规改写示例；清空拒绝词表仅禁用「字面拒绝检测」这一条规则，不是关闭其它治理。
- UI 顶部固定「仅用于授权的防御性安全评估」提示，**常驻可见**（不是可关闭的 toast）。
- 不内置任何**隐式**自动绕过逻辑：预设的「浏览 / 落模板 / 写技能目录」都是管理员手动动作，`skill_file` 写盘只落 SKILL.md。
  唯一的自动化入口是控制台的 `auto_preset` 开关（默认打开，可关）：按请求模型查同名预设，命中才把正文**原样**前插到 system 提示词，名字对不上就不套；总闸关闭时整条链零改动。
- 合规责任在使用方：AIGX 只提供搬运与浏览能力。

---

## 1. 数据流（冻结）

```
jailbreaks/*.md
   └─ tools/sync_purge_presets.py（CRLF→LF 规范化、sha256、preview 200 字符）
        └─ assets/purge-presets/<vendor>/<slug>.md（53 文件，UTF-8+LF）
        └─ src/purge/preset_catalog.rs（include_str! 内联 + 生成代码）
             └─ src/purge/presets.rs（检索/详情/应用，16 单测）
                  └─ src/api/admin/purge.rs（三条 handler）
                       └─ main.rs 路由 → openapi.rs 登记 → registry 守卫
                            └─ frontend PurgeConsole.tsx PresetsSection
```

- 排序确定性：`(vendor, model, id)`；产物无时间戳；资产目录 `rmtree` 全量重建。
- `--check` 与写出共用渲染逻辑（幂等门禁，CI 可跑）。
- 源文件 53/53 为 CRLF；`sha256` 与 `--check` 均基于**规范化后** body 计算。

## 2. 同步脚本 `tools/sync_purge_presets.py`

- 放 `tools/`（`scripts/` 被 .gitignore 排除，见批次纪律）。
- `python tools/sync_purge_presets.py`：写出 `assets/purge-presets/**` + 重新生成 `src/purge/preset_catalog.rs`。
- `python tools/sync_purge_presets.py --check`：幂等校验，exit 0 = 干净。
- 路径段安全（`is_safe_segment`）：非空、≤128 字节、仅 `[a-z0-9-]`、不以 `-` 首尾。
- 多字节安全：preview 用 `chars().take(n)` 截断（不切 UTF-8 半个字符）。

## 3. 检索层 `src/purge/presets.rs`

| API | 说明 |
|---|---|
| `list(q, vendor, limit) -> (items, vendors, total)` | 关键词命中 id/vendor/model/title/tags；vendor 精确（忽略大小写）；`total` = 过滤后全量（items 已被 limit 截断）；vendor 计数基于过滤后结果 |
| `get(id) -> Option<PresetDetail>` | 含完整正文 |
| `apply(&mut PurgeSettings, id, target, activate)` | 见 §4 |

常量：`DEFAULT_LIMIT=50` / `MAX_LIMIT=500` / `PREVIEW_CHARS=200`。

## 4. 应用两落点

| target | 行为 | 落库 | 生效 |
|---|---|---|---|
| `structured_template` | 以 `title` 为模板名 upsert 进 `structured_templates`（同名幂等） | **不直接落库**：只改内存 cfg，由管理面 PUT 统一保存 | 随保存热生效；`activate=true` 时同时设 `active_template` |
| `skill_file` | 往 `skill_dir` 写 `<id>/SKILL.md`（frontmatter 含 keywords=tags） | 只写盘，不动 cfg | 下次技能扫描可见 |

- `structured_template` 复用保存链路（`StructuredTemplate` 同名 upsert 幂等），`skill_file` 走 `FileStore` 写盘。
- `apply_skill_file_writes_into_skill_dir` 测试断言「写盘前后比对」（目录从无到有）。

## 5. API 面（`src/api/admin/purge.rs`）

| 路由（main.rs `:id`） | OpenAPI 文档路径（`{id}`） | 说明 |
|---|---|---|
| `GET /api/purge/presets` | 同名 | `?q=&vendor=&limit=`，默认 50 / 上限 500 |
| `GET /api/purge/presets/:id` | `/api/purge/presets/{id}` | 详情含完整正文 |
| `POST /api/purge/presets/:id/apply` | `/api/purge/presets/{id}/apply` | body `{target, activate}`；404=预设不存在，500=写盘失败 |

- `real_admin_routes()` 手写清单与 openapi `paths` **双向一致**（一致性断言红 = 漏登记）。
- registry `EXEMPT_PREFIXES` 登记 `/api/purge/presets`：UI 侧浏览/落盘便捷入口；AI 工具面已有 `/api/purge` GET/PUT 等价读写，`/apply` 的「按 id 写技能目录」副作用不向 AI 面开放。

## 6. 观测

`RequestLog` 新增 `purge_applied: bool`；chat 路径按 `enabled && (changed || skill_hit)` 赋值（4 处），其余 6 处初始化点补 `false`。

## 7. 前端（`PurgeConsole.tsx` PresetsSection）

- 厂商 chip（过滤后计数）+ 搜索（防抖 250ms）+ 列表 + total + 展开预览面板（等宽只读，限高 420px 滚动）。
- 三动作：「设为结构化模板」（写草稿置脏，**不自动 PUT**）/「复制全文」（剪贴板）/「写入技能目录」（后端写盘 + 只重拉 skills 快照，不动 draft）。
- `data-testid`：`purge-preset-item` / `purge-preset-preview` / `purge-preset-apply-template` / `purge-preset-apply-skill`。
- e2e：`frontend/e2e/purge-console-presets.spec.ts`（7 条，desktop+mobile）。

## 8. 版本

六处同步：VERSION / Cargo.toml / Cargo.lock(aigx) / frontend/package.json / package-lock.json / static/version.json → **1.0.72**。

## 9. 验收记录（lead 本地实测）

| 项 | 结果 |
|---|---|
| `python tools/sync_purge_presets.py --check` | exit 0 |
| `cargo test --bin aigx purge::` | 38 passed |
| `cargo test --bin aigx`（全量） | 2289 passed |
| `cargo test --bin aigx registry` | 99 passed（含 openapi 一致性 + EXEMPT_PREFIXES） |
| `cargo test --bin aigx openapi` | 4 passed |
| `npx tsc --noEmit` | 0 error |
| `npx vitest run` | 157 passed |
| `npx playwright test e2e/purge-console-presets.spec.ts`（desktop / mobile） | 7+7 passed |
| `python .github/workflows/verify-version.py` | 六处一致 |

> 团队说明：本批次原计划 5 人团队派发（`aigx-purge-presets`），成员全部因 provider 账号代理 400 故障失败、复活重试仍失败；按既定纪律改由 lead 在主会话按 T1→T5 顺序落地，交付与验收不受影响。
