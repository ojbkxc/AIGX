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

### 19. 统计导出 CSV（四档宽表）
- `GET /api/purge/stats.csv`：`stats_csv(last_run)` 纯函数产出表头 + 全程 +
  近 1h + 近 24h + 上次运行（**无记录则不出该行**——「没有上次运行」与
  「上次运行全是 0」是两件事）。
- 宽表而非「指标,值」两列：报表最常见的是同一指标跨档对比，透视表直接可用。
- `lower_bound` 列标出窗口值是否为下界估计；表头与取值共用 `STATS_CSV_COLUMNS`。

### 20. Ctrl/Cmd+S 保存
- dirty 才拦截（未改动时放行浏览器自带保存）；监听只挂一次，读
  `dirtyRef`/`saveRef`——否则 `save` 依赖 `numRaw`，每敲一个字都重挂监听。

### 21. 配置导入两级结构校验
- `validateImportedSettings`：**fatal**（拒收并点名）= 根非对象、8 个数组字段
  非数组或含非对象条目、词表行 from/to 非字符串（点名第几条）；**warnings**
  （照导，toast 说全）= 数字越界/非数字、模板缺名、布尔非布尔。
- 缺省字段一律不算问题（旧导出文件必须能导回）。

### 22. 「当前生效项」摘要 + 每条可点
- `buildActiveSummary(settings)` 按数据面真实语义汇总（词表只算启用行、开关开着
  无启用行要明说）；返回 `{key, params, anchor}` 而非拼好的中文串。
- `anchor` 复用体检告警的 `data-purge-fix`，点击滚动到对应配置区；总开关关闭
  那条不带锚点（无处可跳，不给可点的假象）。

### 23. 体检告警 severity 分级
- 后端 `Severity{Error,Warn,Info}` + `severity_of(code)` 纯函数（**刻意不塞进
  `ConfigWarning` 结构体**：severity 完全由 code 决定，存字段就多一处不一致
  来源）。Error = 静默失效、Warn = 效果不符预期、Info = 脏但无害、未登记兜底 Error。
- 前端 `sortWarningsBySeverity` 稳定排序（Error 先），每行加徽标与色条。
- **锁步测试**：从源码抽 code 字面量全集，断言每个都在 `severity_of` 显式出现。

### 24. 技能快照空态常驻 + 手动重新扫描
- 原条件 `skills.length > 0` 让空目录时卡片完全不出现，看起来像「扫描成功但没
  内容」；改常驻 + 空态文案 + 「重新扫描」按钮（复用 `refreshSkills`）。

### 25. 词表 CSV 导入「替换」模式
- 默认追加（既有语义）；勾「替换」则整表换掉，**跳过「全部重复就拒收」预检**
  —— 那个判据是为「追加时导错文件」设计的，替换场景里「全部重复」是正常情况。
- 表非空先 `confirm` 并报出「N 条替换当前 M 条（含未保存编辑）」。

### 26. 预设库筛选持久化 + 样例历史导出/导入
- `aigx.purge.presetFilter`：两项都空时**移除键**；坏值 → 空筛选；搜索词读写
  都截 200 字符。
- 样例历史导出 `{version, samples}`；导入接受三种形态，**校验口径与写历史完全
  一致**（否则「导进 20 条、刷新只剩 8 条」），导入是合并而非替换。

### 27. 「重置为默认」
- `GET /api/purge/defaults` 返回**经 normalize 的** `PurgeSettings::default()`
  （不读当前配置、不写库）；前端填进**草稿**并清预览四态，用户再点保存。
- 默认值必须服务端取：前端硬编码会与 `PurgeSettings::default()` 漂移。

### 28. 移动端开关触摸目标（跨页面修复）
- 根因是页面级覆盖把开关缩得更小：`.mm-alias-switch` 22×12、两处
  `transform: scale(0.72)` → 实测 25.9×14.4。
- `@media (hover: none) and (pointer: coarse)` 下撤销这些缩放 + `.glass-switch::before`
  补 44×44 触摸区；桌面零回归（实测对照见 §本地真服务冒烟）。

### 29. 词表行级「试」
- 每行加「试」按钮（from 非空才显示）：用该条原文生成样例、滚到预览区并
  **立刻跑一次**。命中受表序/停用/链式改写影响，看代码推不出来。
- `runPreview(inputOverride?)`：`setState` 异步，直接读 state 会拿到上一次的
  样例（结果与点的那行对不上）。

### 30. diff「只看变更行」
- 长样例里上下文行占绝大多数，遮住真正的改写点；勾选后只留 add/del/mod，
  并提示「已隐藏 N 行未变更内容」（不静默省略）。默认关闭。

### 31. 未保存变更明细
- `dirty` 只说「有改动」，不说哪几项。`diffSettings(saved, draft)` 按**数据面
  语义**比对：9 布尔 + 3 数字 + 5 字符串逐项 `旧 → 新`；两张词表按条数+启用
  行数；模板按**名称集合**（只改正文不报）；异源模型逐个比较。
- 警示条展示前 6 项，超出「另有 N 项」。

### 32. 预设检索覆盖正文 + 「正文命中」徽标 + 命中片段
- `matches` 纳入 `preset.body`（`include_str!` 内联的 `&'static str`，零 IO）。
- `PresetBrief.body_match` 只在「元数据搜不到、正文搜得到」时 `Some(true)`
  （元数据已命中不挂徽标，否则每条都带标记等于噪音）；`None` 时字段跳过。
- 展开面板给 `findBodySnippet` 的命中片段（前后 60 字符 + `<mark>` 高亮），
  大小写不敏感定位但**返回原文切片**。

### 33. 破甲页迭代面 e2e（9 条）
- `e2e/purge-console-iter.spec.ts`，desktop + mobile 两个 project 各 9 条：
  生效项摘要语义 / 点击跳转闪烁 / 告警分级排序 / 词表整理回报条数 / 行级「试」/
  只看变更行 / 重置为默认不直接保存（断言 PUT 计数 0）/ 搜索框提示 / 正文命中
  片段高亮。
- **纯函数单测验不到「页面真的这么反应」**：dialog 确认后的草稿变化、toast 文案、
  diff 渲染条件只有真浏览器能验。

### 34. 跨项目：pi-agent-desktop 破甲页可用性验证
- 该仓是 Next.js 独立实现（`components/PurgeConsoleSection.tsx` + `app/api/purge/*`），
  不是引用 AIGX 产物。验证方法：`PI_WEB_PASSWORD=… npm run dev`（端口 30141）→
  无头浏览器登录 → 设置 → 破甲控制台。
- 实测：面板完整渲染（43 个交互控件）、提示语由真实配置算出、写入链路
  `GET 200 → PUT 200（返回 settings）→ GET 200` 幂等且值稳定。
- **验证写入时用「原值幂等回写」**：只证明链路通，不改用户配置。

### 35. 预设「最近用过」徽标 + e2e 第 10 条
- `aigx.purge.presetRecent`（去重置顶、上限 5、坏值容错、写失败静默）；
  「设为结构化模板」与「写入技能目录」成功后各记一次。
- **只做标记不做筛选**：改成「最近」筛选会与后端分页冲突——后端不认识这份
  id 表，`total` 与翻页都会错。这条写进代码注释防后人误改。
- e2e 补齐「应用预设 → 记录 → 徽标出现」真链路（纯函数单测只验了读写）。

### 36. 真实部署环境审计（内网 http / 缓存 / 下载 / 移动端）
这一批不是新功能，而是把「本地开发看不出、真实部署必踩」的入口逐个打穿：

| 问题 | 症状 | 修法 |
|---|---|---|
| 内置 30s 技能缓存挡住「重新扫描」 | 放好 `SKILL.md` 点一下，界面仍空 —— 按钮像坏的 | `skill_briefs_with(dir, true)` + `GET /api/purge?skills=fresh` |
| `navigator.clipboard` 在非安全上下文不存在 | 内网 `http://192.168.x.x` 下「复制全文」必失败，还提示「权限问题」 | `lib/clipboard.ts::copyText` 降级 textarea+execCommand |
| `downloadText` 三处隐患 | 0 字节下载 / 老 Firefox 不触发 / 不可见 BOM 字面量 | revoke 延后 1s、append→click→remove、显式 `'\uFEFF'` |
| 词表行窄屏 5 行高 | 加了「试」按钮后单列布局每控件一行 | 操作组收成 `.purge-rule-ops`，窄屏两行（83px→54px） |
| 缺布局回归防线 | 再加控件挤爆窄屏没人发现 | e2e 第 13 条：页面级不溢出 + 容器级只报「被裁剪」 |

**判据取舍（写进 e2e 注释）**：容器级溢出只报 `overflow-x: hidden` 那类。
`visible` 的溢出可能只是绝对定位的触摸区撑大了 `scrollWidth`（开关的 44×44），
既不产生滚动条也不裁剪；把它算缺陷会逼人砍可达性——本末倒置。

### 37. 真实后端契约联调（不是 mock）
此前所有前端验证都跑在 mock 上——mock 的形状是我们**以为**的契约。本轮起真实
二进制、隔离数据目录，逐接口核对：

```
AIGX_SERVER__PORT=18096 AIGX_SECURITY__CAPTCHA_ENABLED=false \
  AIGX_SERVER__DATA_DIR=D:\tmp\aigx-real2  target\debug\aigx.exe
# 一次性 admin 密码在 stdout 的 WARN 行
```

**两个前置坑（真联调必踩）**：
1. 首次登录后**必须改密码**，否则管理面一律 `403 PASSWORD_CHANGE_REQUIRED`
   ——不是 401、不是空数据，容易误判成「接口坏了」。
2. 改密会 **revoke 旧会话**，必须用新密码重新登录拿 token。

**实测结果（全部 200，逐字段核对）**：
| 接口 | 结果 |
|---|---|
| `GET /api/purge` | 11 个字段齐全、类型全对（`settings/stats/runtime/config_warnings/skills/skill_dir_resolved/stats_1h/stats_24h/stats_since/stats_last_run/config_warning_items`） |
| `GET /api/purge/presets` | `total=53`，条目字段与 `PurgePresetBrief` 完全一致 |
| 正文检索 `q=vanta` | 命中 4 条，`body_match` 全为 true（元数据都不含该词） |
| `GET /api/purge/presets/{id}` | `body` 长度 25462 |
| `GET /api/purge/defaults` | 200，含 settings |
| `GET /api/purge/stats.csv` | 表头 13 列与 `STATS_CSV_COLUMNS` 一致；无 `last_run` 行（从未关机，符合设计） |
| `POST /api/purge/preview` | 返回 1 条 messages + `detail`，`changed=false`（默认配置下无改写，符合预期） |
| `POST /api/purge/stats/reset` | 200，`responses` 归零 |

**结论**：mock 与真实契约**没有偏差**——这批功能在真后端上可直接用。

### 38. 真实后端 + 真实前端页面联调（第二轮）
上一轮只核对了 API 契约；本轮把**真后端 + 真 static + 无头浏览器**串起来，看页面
在真实数据下的渲染。

**手工注入 token 调试要三个键**（路由守卫的口径，缺一个就被踢回登录页）：
```js
localStorage.setItem('token', t);
localStorage.setItem('expires_at', String(Date.now() + 3_600_000)); // 缺它 isAuthenticated() 恒 false
localStorage.setItem('role', 'admin');                              // 缺它 AdminRoute 判非管理员
```

**实测（1440×900，真实 53 条预设）**：标题、7 个页内导航按钮、50/53 条预设
（默认 limit=50，符合分页设计）、1 条真实体检告警、5 条生效项、保存按钮、
2 个「整理」按钮、统计基线时间——全部按预期渲染，`pageErrors` 为空。

**由此抓到一个真实缺陷**：`skill_dir_resolved` 显示成
`D:\Users\…\.aigx/skills` ——**混用两种分隔符**。`expand_home` 用
`PathBuf::from(home).join(rest)`，而 `rest` 里带 `/`。修法：`components()`
重建 PathBuf，按平台原生分隔符输出；测试断言「不混用」。

### 39. 真机交互验收清单（点下去真的这样反应）
上一轮验的是「渲染」，本轮验「交互」。真后端 + 真 static + 无头浏览器，逐项点：

| 交互 | 期望 | 实测 |
|---|---|---|
| 预设库「加载更多」 | 50 → 53 | **50 → 53** ✓ |
| 搜索 `vanta`（正文命中） | 命中数 = `body_match` 徽标数 | **4 条 / 4 徽标** ✓ |
| 词表「整理」（默认空表） | 提示「已经干净」而不是删 0 行 | ✓ |
| 「清零统计」 | 成功提示 + 基线行仍在 | ✓ |
| `pageErrors` | 空 | 空 ✓ |

**这轮没有改代码** —— 它的产出是证据：这批功能在真实后端上确实按设计工作。
**「没有缺陷」也是一种需要被记录的结果**，否则下次还得重新怀疑一遍。

### 40. e2e 跑在「static 中间态」上＝白跑一轮
破甲页 e2e 托管的是 `static/`（构建产物）。同树多 agent 协作时，**别人 `npm run
build` 的那十几秒里 `static/` 是被文件替换覆盖的**——这期间起 e2e，页面会加载到一半
新一半旧的 chunk。

实测形态（同一份代码，连续几轮结果完全不收敛）：
| 跑法 | 结果 |
|---|---|
| static 正在被重建时跑全量 | desktop 3 failed → 排除新用例后 **5 failed**（越跑越多） |
| 单跑那几条失败的 | **全部 passed**（2 passed ×3） |
| static 静止后连跑 2 次 | **18/18 ×2**，mobile **18/18** |

**判据**：跑 e2e 前先看 `static/index.html` 的 mtime 与 `git status -- static`；
mtime 在最近一分钟内变过、或 static 有未提交改动 → **先等它静止**。失败条数随重跑
变化本身就是「环境在动」的信号，不是 flaky。

顺带记两个诊断坑：
- 用 `--grep-invert` 排除新用例后失败**更多**（3→5），一度误以为是新用例污染——
  实际是那几分钟 static 还在被替换，排除法在这种环境下没有意义。
- 我自己的诊断脚本用 `/api/.*/` catch-all 返回 `{}`，把 `StatusLine` 的
  `channels.reduce` 打崩，制造出「破甲页崩在 `d.reduce`」的假象。**mock 的兜底
  范围越大，越容易伪造出别人的 bug。**

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
- **severity / anchor 这类「从 code 派生」的属性不要存进结构体**：存一份就多一处
  可能与来源不一致的副本。纯函数 + API 输出层补键，配锁步测试防漏分类。
- **移动端触摸目标 ≥44×44**：页面级 `transform: scale()` 与小尺寸覆盖在桌面是
  紧凑，在手机上是点不中；修法一律用 `@media (hover:none) and (pointer:coarse)`，
  不要用 `max-width`（会误伤桌面窄窗口）。
- **改 TSX 一律用 `edit` 工具**：PowerShell here-string 会被 `${}` 插值吞掉、
  CRLF 与 LF 混用导致 `Replace` 静默不生效、行数组手术会误删相邻声明
  （第 51/55/58 轮各踩一次，行号与 `ReadAllLines` 索引也对不上 `Select-String`）。

## 验证汇总（本机 mingw / pnpm，2026-11-24 实测）

| 门禁 | 结果 |
|---|---|
| `cargo test --bin aigx` | **2575 passed / 0 failed**（含 purge:: 87） |
| `cargo clippy --bin aigx -- -D warnings` | 0 |
| `rustfmt --check`（触碰文件） | 0 |
| vitest 全量 | **279 passed / 0 failed**（PurgeConsole 84） |
| e2e（破甲页两个 spec，desktop） | **18 passed / 0 failed** |
| tsc / eslint `--max-warnings 0` | 0 / 0 |
| i18n:check | zh=en 键集一致 |
| static 零漂移 | 双重构建后 `git status static/` 为空 |

> 数字随批次增长；以最后一次全文门禁的实测为准（上表为第 36 批收口值）。
> **已修**：`channel::balance_refresh::tests::sweep_survives_channel_deleted_mid_round`
> 曾偶发假红——固定 `sleep(50ms)` 当同步手段，并发负载下不够。已改为轮询等待
> （每 5ms 查一次、最多 2s，判据不变），单跑 5 次 + 全量连跑 3 次全绿。
> 教训：**测试里不要用固定 sleep 当同步**，一律「观测到就继续 + 超时即失败」。

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
