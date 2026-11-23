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

## 关键设计取舍（防回归备忘）

- 事件环单环 vs 11 个独立计数器环形：省 ~700KB 常驻 + 读侧一次锁。
- `mod.rs` 的 `bump` 已删（全部走 `bump_with` 双写），clippy 勿再留单写版本。
- diff 的行级 LCS 用 O(行数²) 全表；预览样例行数很小，不要换 ESI 简化版
  （上下文行会漂移）。
- `parsePreviewMessages` 尾部空行剔除在 flush 内做（CRLF 残留/结尾空白
  不黏进上一条消息）——有回归测试钉住。
- 词表 CSV 是**追加**导入：与「添加规则」同口径；改成替换前先想清楚
  用户手工行会不会被导入文件吞掉。

## 验证汇总（本机 mingw / pnpm，2026-11-24 实测）

| 门禁 | 结果 |
|---|---|
| `cargo test --bin aigx` | **2519 passed / 0 failed**（含 purge:: 72） |
| `cargo clippy --bin aigx -- -D warnings` | 0 |
| `rustfmt --check`（触碰文件） | 0 |
| vitest 全量 | **204 passed / 0 failed**（PurgeConsole 28） |
| tsc / eslint `--max-warnings 0` | 0 / 0 |
| i18n:check | zh=en 键集一致 |

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

## 负结论（做过但回滚，防重复踩）

- **`skill_keyword_empty` 告警（技能扫到但全无关键词）**：不可达。`keywords_of`
  从 `name`（非空即取）+ `description`（≥2 字符 token）**派生**关键词，
  frontmatter 不写 keywords 键也永远有候选——`skill_dir_empty` 已覆盖全部
  真实盲区。实现后测试证伪（name 总是第一个关键词），已整体回滚。
  未来加「关键词质量」类体检时，请从 `body_chars == 0` 或「全部关键词
  均 <2 字符」这类真实可达态入手。
- **`preset_missing` 最近 id 建议（已落地 `3e1c52db`）**：编辑距离 ≤2 提示、
  >2 坚决不猜——与 `preset_for_model` 的不猜立场一致，勿改成模糊自动纠正。
