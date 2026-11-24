# 破甲控制台迭代批次（v1.0.94 窗口，2026-11-24）

> 目标：破甲控制台功能持续完善。本文件汇总本轮多个迭代的交付与门禁，
> 作为 v1.0.94 的变更说明底稿。前置基础（开关/词表/模板/技能/预设库/
> 收益统计/配置体检）见 `docs/PURGE-*.md` 与 v1.0.93 发布记录。

## 批次清单（按落地顺序）

### 1. 预览命中明细（AppliedDetail）
- 后端 `Applied { changed, skill_hit, detail }`；`detail` 含
  normalized / dead_zone_rules / dead_zone_replacements / synonym_rules /
  synonym_replacements / structured_appended / skill_names(≤16) /
  skill_truncated / skill_keywords。
- `rewrite_texts`+`rewrite_texts_counted` 合并为 `rewrite_texts_detailed`
  （替换语义逐字节不变，只顺路计数落桶）；`normalize_system_counted`
  （保守计数：丢弃差值 + 改写至少 1）；`inject_skills` 记技能名+首个命中关键词。
- `POST /api/purge/preview` 响应加 `detail` 字段（`applied` 形状不变）。
- 前端预览卡 chip 行：归一化 N / 死区词 R/S / 同义词 R/S / 结构化已追加 /
  命中技能：foo（命中关键词 "bar"）。

### 2. 统计时间窗（近 1 小时 / 近 24 小时 / 全程）
- 后端事件环 `EventRing`（4096 槽 `(epoch_ms, EventKind)` 单环聚 11 类），
  既有 `AtomicU64` 累计不动；`windowed_snapshot(window_ms)` 按窗口裁剪，
  `truncated`（环溢出=下界估计）如实携带。
- `GET /api/purge` 加 `stats_1h` / `stats_24h`。
- 前端统计卡上方时间窗切换条（旧后端无数据 → 窗口档禁用）；卡标题拼窗口
  后缀，窗口值溢出前缀「≥」；失败分类条联动。

### 3. 词表 CSV 导入导出（死区词 / 同义词单表）
- 导出 `toCsv`（RFC 4180）+ `downloadText`（BOM），`aigx-purge-{表}-日期.csv`。
- 导入 `parseRulesCsv`（引号内逗号/换行/双写引号、CRLF、BOM、中英文表头
  可选丢弃）；**追加**进草稿（不整表替换，重复靠行内提示+后端去重兜底）；
  列数错/from 空 → 整单拒绝。

### 4. 预览多消息模式
- `parsePreviewMessages`：`system:`/`user:`/`assistant:` 行前缀开消息，
  裸文本仍单条 user。归一化作用于多条 system，词表同时改 system+user
  （与线上链路同口径）。
- 输出多消息时带 `[role]` 分段；单条纯文本不变。

### 5. 配置体检告警「去修复」
- `config_warning_items` → `Vec<ConfigWarning{code, message}>`；六个稳定
  机器码（fallback_empty / fallback_after_retry / skill_dir_empty /
  chained_rewrite / structured_empty / preset_missing）。
- `GET /api/purge` 加 `config_warning_items`（旧字段保留）。
- 前端按 code 显示「去修复」：平滑滚动 + 聚焦 + 1.8s 闪烁；6 个锚点
  （data-purge-fix）。旧后端退化纯文本。

### 6. 预览输出 diff 染色
- `diffLines`（行级 LCS，>400×400 退化整红整绿）＋ `diffTokensInLine`
  （词级：首尾公共前后缀剥离 + 中段贪心子序列；CJK 单字成词）。
- 有差异时输出区换 diff 视图（删行红/增行绿/行内删词删除线/增词加粗），
  无差异保持纯文本 pre。

### 7. 预览样例历史
- localStorage `aigx.purge.previewHistory`，最近 8 条；成功才入历史；
  置顶去重、空白拒收、单条 4KB 上限、坏值容错。
- 输入框上方 chip 回显（首行 46 字符截断，角色前缀带 `~`），点击回填。

### 8. 「设为结构化模板」落点导航
- 预设库写入模板草稿后自动滚动到模板编辑区并闪烁高亮（复用 purge-fix-flash）。

### 9. 模板 JSON 导入（同名 upsert + 配置级反导）
- `parseTemplateJson` 三形态：`{name,text}` / 数组 / 配置级 `{structured_templates:[…]}`
  （本页「导出配置」产物可直接反导回模板）。校验 name 非空 ≤64、text ≤128KB、
  文件内同名后者跳过；写入时已有同名**覆盖**并报「新增 N / 覆盖 M」。

### 10. 词表 CSV 导入冲突预检
- 导入前对同表做 from 冲突统计（trim + 大小写不敏感）：全重复 → 拒收并警告
  （多因导错文件/错表）；部分重复 → 正常追加但降为 warning 并附重复条数。
- 三层防线：导入预检 → 行内 dupHint → 运行时按表序（后者不再匹配）。

### 11. 预设库检索相关性排序
- `relevance` 五档（标题全等 > 前缀 > 包含 > 厂商/模型 > 仅标签/id），
  **仅排序不匹配**（`matches` 仍是唯一判据），在 `take(limit)` 之前排——
  否则标题精确命中项会被翻页截掉。无检索词保持目录原序。

### 12. 预设库分页（加载更多）
- `list_page(q,vendor,model,offset,limit)`；`list` 为 offset=0 薄壳。
  前端按 `items.length` 增量拉取并追加，按钮显示「还有 N 条」，
  筛选变更自动回第一页。55 条目录的最后 5 条此前永远不可见。

### 13. 词表单条启停（RewriteRule.enabled）
- `#[serde(default)] enabled=true`：旧数据只有 from/to，反序列化后全启用，
  **行为逐字节不变**；引擎跳过禁用行、明细不计；前端行内 checkbox +
  整行降透明度（不隐藏，草稿历史可见）。

### 14. 词表 200 条 / 异源 16 条 / 其余 5 处上限前置提示
- 后端 `normalize` 的 `take(N)` 会静默丢弃超出部分。前端三档覆盖：
  `RULE_TABLE_CAP=200`（两表顶部红条 + CSV 导入 toast 降级）、
  `FALLBACK_MODELS_CAP=16`（输入框 error 槽）、`capError()` 统一处理
  `LIST_CAP=64`×4（剥离前缀/拒绝话术/过滤词/空答词）与 `TEMPLATES_CAP=32`。
  现在**所有 take(N) 截断点都有前置提示**。

### 15. 预设落模板前覆盖同名确认
- `presetApplyNeedsConfirm` 三段：无同名 → 直落；同名同文 → 静默幂等；
  同名异文 → 就地二段确认（覆盖/取消），不引弹窗栈。

### 16. 预览可切「按已保存配置」
- 勾选后用已保存快照试跑，与默认草稿试跑形成对照——同一输入两次跑，
  差异即「保存会改变什么」，避免拿未保存配置误判线上行为。

### 17. 统计口径第四档「上次运行」+ 快照持久化
- `RunSnapshot{saved_at, uptime_secs, 展平 PurgeStats}`，KV `purge:last-run`；
  优雅关机 + **cron 每 10 分钟**双写（崩溃/kill 也不丢）；读侧校验
  （saved_at>0、uptime≤一年，坏值当无记录）；`GET /api/purge` 出
  `stats_last_run`，前端四档切换（该档是精确快照，不带 truncated）。

### 18. 体检扩到 10 类 + 全通过态 + 启用感知
- 新增：`skill_keyword_short`（单字符关键词命中面过宽）、
  `dead_zone_all_disabled` / `synonym_all_disabled`（开关开着但整表被单条
  停用）、`fallback_duplicate`（异源模型重复项）。
- `chained_rewrite` 只看**启用行**（停用行正是用户掐链的手段，报警是噪音）。
- 无告警且总开关开 → 绿字「配置体检通过」；总开关关 → 不显示。

## 关键设计取舍（防回归备忘）

- 事件环单环 vs 11 个独立计数器环形：省 ~700KB 常驻 + 读侧一次锁。
- `mod.rs` 的 `bump` 已删（全部走 `bump_with` 双写），clippy 勿再留单写版本。
- diff 的行级 LCS 用 O(行数²) 全表；预览样例行数很小，不要换 ESI 简化版
  （上下文行会漂移）。
- `parsePreviewMessages` 尾部空行剔除在 flush 内做（CRLF 残留/结尾空白
  不黏进上一条消息）——有回归测试钉住。
- 词表 CSV 是**追加**导入：与「添加规则」同口径；改成替换前先想清楚
  用户手工行会不会被导入文件吞掉。
- **加体检项前先读入口防御**（`parse_skill`/`keywords_of`）：解析层已挡掉
  的形态不会出现在体检输入里（见负结论两条）。
- **重复行不会被后端删**：`normalize` 只 trim/丢空/截断，重复项靠「按表序
  后一条匹配不上」自然失效；前端提示文案必须这么写，别误导成「后端会清」。
- 上限常量必须与后端 `take(N)` 逐一对齐并注明来源；新增字段时同步加提示。

## 验证汇总（本机 mingw / pnpm，2026-11-24 实测）

| 门禁 | 结果 |
|---|---|
| `cargo test --bin aigx` | **2546 passed / 0 failed**（含 purge:: 81） |
| `cargo clippy --bin aigx -- -D warnings` | 0 |
| `rustfmt --check`（触碰文件） | 0 |
| vitest 全量 | **215 passed / 0 failed**（PurgeConsole 41） |
| tsc / eslint `--max-warnings 0` | 0 / 0 |
| i18n:check | zh=en 键集一致 |
| static 零漂移 | 双重构建后 `git status static/` 为空 |

> 数字随批次增长；以最后一次全文门禁的实测为准（上表为第 18 批收口值）。

## 文件清单（本批 Purge 相关）

```text
M src/purge/mod.rs                     # AppliedDetail/事件环/config_warning_items
M src/api/admin/purge.rs               # detail + stats_1h/24h + config_warning_items
M frontend/src/pages/PurgeConsole.tsx  # 全部前端面
M frontend/src/pages/PurgeConsole.css  # diff/告警行/时间窗/闪烁样式
M frontend/src/pages/PurgeConsole.test.ts（新增）
M frontend/src/types/index.ts          # PurgeAppliedDetail/WindowedStats/ConfigWarning
M frontend/src/i18n/zh.json / en.json  # +16 键（复用安全页 2 键）
```

## 本地真服务冒烟（怎么跑，踩过什么）

```powershell
cargo build --bin aigx                     # 约 15 分钟（首次）
$dir='D:\tmp\aigx-smoke'; New-Item -ItemType Directory $dir -Force
$env:AIGX_SERVER__PORT='18099'; $env:AIGX_SERVER__HOST='127.0.0.1'
Start-Process target\debug\aigx.exe -WorkingDirectory $dir `
  -RedirectStandardOutput "$dir\out.log" -RedirectStandardError "$dir\err.log" -PassThru
# 探活：GET /livez → 200 ok
# 鉴权闸：GET /api/purge 无 token → 401
```

已知坑（实测）：

- **数据目录隔离只认 `AIGX_SERVER__DATA_DIR`**（不是 `AIGX_DATA_DIR`）：
  `config.server.data_dir` 才是权威；环境覆盖是**逐字段硬编码**的
  （`config.rs::apply_env_overrides_from`），只认列出的那几个键。
  用 `AIGX_SERVER__DATA_DIR=D:\tmp\x` 起实例即可得到干净库（实测生效，
  会新建 `data/aigx.db`、`script-plugins/`、`updates/`）。
- **首次启动的默认管理员密码是一次性随机串**，只在库中无 admin 时
  打印到 stdout（`WARN First-time setup: ... password=Aigx-xxxx`），
  所以「干净库 + 抓日志里的口令」是拿到可用 token 的最短路径。
- **登录被 ALTCHA 人机校验挡住**：`POST /api/auth/login` 需要 `captcha`
  字段（ALTCHA proof-of-work 解），无答案直接 400「请先完成机器人校验」。
  `security.captcha_enabled` **没有环境变量覆盖**（逐字段硬编码里没有它），
  且配置文件路径固定 `~/.aigx/config.toml`（`HOME`/`USERPROFILE` 重定向
  对它无效）；settings API 能关但要先登录——鸡生蛋。
  → **想跑通带鉴权的 e2e，必须在测试里实现 ALTCHA 解题，
  或先给 `security.captcha_enabled` 加一个环境变量覆盖**（推荐后者）。
- 登录路由是 **`/api/auth/login`**（不是 `/api/user/login`，后者 405）。
- `Start-Process` 无 `-Environment` 参数（PowerShell 5.1）；用 `$env:` 预设。

**本轮实测到的事实**：`GET /livez` → 200 ok；未鉴权 `GET /api/purge` → 401；
干净库启动 → 打印一次性 admin 口令；`AIGX_SERVER__DATA_DIR` 生效。

## 负结论（做过但回滚，防重复踩）
- **`skill_keyword_empty` 告警（技能扫到但全无关键词）**：不可达。`keywords_of`
  从 `name`（非空即取）+ `description`（≥2 字符 token）**派生**关键词，
  frontmatter 不写 keywords 键也永远有候选——`skill_dir_empty` 已覆盖全部
  真实盲区。实现后测试证伪（name 总是第一个关键词），已整体回滚。
  未来加「关键词质量」类体检时，请从 `body_chars == 0` 或「全部关键词
  均 <2 字符」这类真实可达态入手。
- **`skill_body_empty` 告警（正文全空技能）**：同样不可达。`parse_skill`
  在 `body.trim().is_empty()` 时直接返回 `None`（SKILL.md 只有 frontmatter
  根本进不了 entries）——实现前忘了核对解析层的防御，10 分钟即撤。教训：
  加体检项前先读 `parse_skill`/`keywords_of` 的入口防御，**解析层已挡掉
  的形态不会出现在体检输入里**。
- **`preset_missing` 最近 id 建议（已落地 `3e1c52db`）**：编辑距离 ≤2 提示、
  >2 坚决不猜——与 `preset_for_model` 的不猜立场一致，勿改成模糊自动纠正。
