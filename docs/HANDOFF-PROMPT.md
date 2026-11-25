# AIGX 交接提示词（给下一个 AI）

> 把下面「提示词正文」整段复制给接手的 AI。末尾「环境信息」含凭据，按需删改。

---

## 提示词正文

你在接手一个已经进行到一半的项目。请先读完全部内容再动手。

### 项目位置

- 主仓库：`D:\GitHub\AIGX`（Rust + axum 0.7 后端，React + TS 前端），分支 `main`，**直接改 main，不要新建分支/worktree/PR**
- 上游参考仓：`D:\GitHub\Agent2API`（简称 A2A），当前 tag `v3.5.1`（commit `33f5e797`）。**只读，不要改它**
- 前端源码 `frontend/src/`，后端源码 `src/`，构建产物 `static/`（**在版本库里，必须与源码同步提交**）

### 项目是什么

AIGX 是自建 AI 网关：多渠道/多账号池转发到各家上游（Trae、Kimi、GLM、DeepSeek、小浣熊、讯飞星火等），带账号池、路由、签到、破甲（prompt 改写）、插件（wasm）等模块。A2A 是同类开源项目，**AIGX 的功能面参照 A2A 对齐**。

### 绝对纪律（违反会直接搞坏东西）

1. **禁止在本机编译 Rust**：`cargo build` / `run` / `test` / `check` / `clippy` / `install` / `rustc` / `maturin` / `wasm-pack` 一律不许跑。只允许 `cargo fmt` / `cargo fmt --check`（纯格式化）。Rust 改动的验证一律交给 CI，交付里写「待 CI 验证点」。
2. **`.tsx` / `.json` 用 Edit 工具改，不要整文件 Write**。整文件写会破坏中文编码、产生 BOM 和文件尾残渣（本项目已因此踩过多次：文件末尾多出孤儿 `}` 导致整树编译失败）。
3. **不要 `git add -A`**。用 `git commit -F <消息文件> -- <具体路径>...`，**commit 也要带 pathspec**，否则会带走共享工作树里别人暂存的文件。提交信息写**根因**（「为什么会坏」），不要写「已完成」。
4. **禁止** `git reset --hard` / `checkout --` / `restore` / `clean`（git 不可恢复）。
5. **改了前端源码就必须重建 `static/` 并一起提交**：`cd frontend && node node_modules/vite/bin/vite.js build`，然后 `git add ../static`。CI 有「static 与源码一致」闸门，漏了必红。
6. **新增 `t('…')` 字面量必须补 i18n**：`cd frontend && node scripts/i18n-fill-missing.mjs`，然后提交 `frontend/src/i18n/{zh,en}.json`。CI 有 i18n 闸门。
7. **本地验证用准确参数**（这条踩过坑）：
   ```
   cd D:\GitHub\AIGX\frontend
   node node_modules/typescript/bin/tsc --noEmit
   node node_modules/eslint/bin/eslint.js . --ext ts,tsx --report-unused-disable-directives --max-warnings 0
   node node_modules/vitest/vitest.mjs run
   node node_modules/@playwright/test/cli.js test e2e/<file>.spec.ts --project=desktop --trace=off --output=D:\tmp\pw-x
   ```
   注意 `eslint` 必须带 `--report-unused-disable-directives`（只跑 `eslint src` 会漏掉未使用的 disable 指令，CI 会红）。`npx` 可能被 PowerShell ExecutionPolicy 拦，直接用 `node node_modules/...`。
8. **凭据不进日志、不进提交信息、不进交付正文**。

### 当前状态（务必先核对，不要相信这段文字，去仓库核对）

- 线上跑的是 **v1.0.101**（`https://api.lxseek.com/version.json` 可查）
- 代码仓库 `main` 上**已经领先线上很多个提交**，这一轮的修复全部**已提交、未部署**
- **所以第一步应该是：让 CI 绿 → 发版 → 部署 → 实地验证**，而不是继续加新功能

### CI 现状与怎么发版

CI 有这些 job：`CI`（含 lint/rustfmt/clippy、cargo test、build frontend、route inventory gate、e2e）、`e2e-browser`、`alerts-lint`、`Release & Deploy`、`Multi-platform Build`、`Docker Publish`。

- 查 CI：`curl -s -H "Authorization: Bearer <GH_TOKEN>" "https://api.github.com/repos/ojbkxc/AIGX/actions/runs?per_page=10"`
- **发版流程**：①六处版本号一起 bump（`VERSION` / `Cargo.toml` / `Cargo.lock` 里 aigx 那条 / `frontend/package.json` / `frontend/package-lock.json` / `static/version.json`）②`python .github/workflows/verify-version.py` 校验一致 ③`vite build` 重建 static ④提交 + `git tag -f vX.Y.Z HEAD` + push tag ⑤等 `Release & Deploy` 产出 `AIGX-x.y.z-linux-amd64` ⑥下载并 SFTP 到服务器部署
- **注意**：`Release & Deploy` **不依赖 CI 全绿**也能跑（tag 一推就触发）。但别把编译不过的代码发上去。

### 未解决的问题（这是你要做的工作）

#### ① Trae 通道仍然打不通（最高优先）

**现象**：`deepseek-v4.1-flash` + 「你好」返回 Trae 上游的错误：
```
{"code":"11-128","msg":"request illegal","displayMsg":{"zh":"请求被拦截，请重新发送"}}
```
或 `[4001] We're sorry, the param is invalid. Please try with a valid param.`
或 `404 <html><center>TLB</center></html>`

**已做过的修复（都已提交，别重复）**：
- `content` 必须是数组 `[{type:"text",text}]` 不是字符串（上游 Go 类型是 `[]*idecopilot.LLMRawMessageContent`）
- UA 按凭证 `variant` 分两套：`VSCode 1.107.1 (TRAE SOLO CN)` / `(TRAE CN)`
- 13 个身份/渠道头（`x-user-region: CN`、`package-type: stable_cn`、`x-lgw-req-sdk-type: 3`、`x-market-client-id`、`x-device-type`、`x-app-version`、`x-ide-version`、`x-ide-version-type`、`x-ide-version-code: 20260917`、`x-plugin-channel: icube-ai`、`request-traffic-type: prod`、sec-fetch 三个）
- 认证头三处：`authorization: Cloud-IDE-JWT <token>` + `x-cloudide-token` + `x-ide-token`
- 每请求现生成 `x-request-id` / `x-trae-request-id` / `x-tt-trace-id`
- 凭证里的 `uid` / `deviceId` / `machineId` 转成 `x-uid` / `x-device-id` / `x-machine-id`
- **空文本消息补 `content:[{type:"text",text:""}]`**（不能省略 content，否则 4001）
- `tool_choice` 折成裸字符串、`tools[].function.parameters` 用 JSON 字符串
- `base_url` 归一：`api.trae.cn` / `www.trae.cn` / `api.trae.com.cn` → `https://trae-api-cn.mchost.guru`

**已确认的事实（用真实凭据在服务器上逐项探针复现过）**：
- wasm 里 `/api/agent/v3/*` 只有 `llm_utils_chat` 一条路径；`chat_v3` 是 function 值不是路径
- `function` 四个候选 `solo_work_lite` / `solo_work_remote` / `solo_agent` / `chat_v3` **都能出 token**；`solo_coder` 会 4001（模型表里没有 deepseek）
- 线上 9 个 trae 账号**全部 `variant=solo`**，其中 7 个 `trae-solo-*` 的库里 `base_url` 仍是 `https://api.trae.cn`（代码层已归一，**库里没改**）
- 对照组：同一台服务器上 A2A 的 `deepseek-v4.1-flash` **能正常返回**（真实 reasoning_content），所以上游没变、账号没失效，差距在 AIGX 的请求构造

**你要做的**：
1. 先部署当前代码（含上述修复）到服务器，实测一次
2. 看 failover 日志每条的失败类型变了吗：`journalctl -u aigx --since '5 min ago' | grep -iE 'failed on|trae'`
3. 如果还不行，拿真实 token 直接在服务器上打 `https://trae-api-cn.mchost.guru/api/agent/v3/llm_utils_chat`，逐项试 path/function/header 组合，看哪一项触发了错误
4. **关键待查**：`import.rs` 的测试写明 trae 凭证带 `accessToken / refreshToken / expiresAt / uid / deviceId / machineId / variant / nickname`，其中 `deviceId` / `machineId` 可能需要更具体的值（不是随便的 uuid）。去 A2A 的 `desktop-tauri\src-tauri\server\src\server\core\login\managed\trae.rs` 和 `account_store\trae_accounts.rs` 看它怎么生成/校验这些值
5. 可能的剩余差异：请求体字段（`config_names` / `need_prompt` / `poly_prompt` / `reasoning_effort_options`）、`app-version` 的实际值、`x-app-version-code`

#### ② 日志「供应商」列永远是 `—`

**根因已定位**：后端 `src/log/mod.rs` 的 `RequestLog` 结构体**根本没有 `provider` 字段**。线上 100/100 条记录的字段清单里没有它，而前端「供应商」列读 `row.provider`。

**你要做的**：
1. 判断「供应商」该记哪一层：渠道的 `provider`（`openai_compatible` 这类）还是账号的 `provider`（`trae` / `xiaohuanxiong`）。去 `D:\GitHub\Agent2API` 看 A2A 的请求记录结构里有没有对应维度，对齐它；A2A 没有就自己定并写清理由
2. `RequestLog` 加 `provider: Option<String>`（`#[serde(default)]`，存量日志不受影响）
3. **全仓 grep `RequestLog {` 找齐所有构造点**逐处填值（漏一个就 E0063 编译失败）
4. 前端 `Logs.tsx` 改读新字段。**空值显示 `—` 是合理的**（历史记录没记过），但要区分「真的没有」和「有数据却显示不出来」
5. 导出 CSV、日聚合要不要跟 —— 自己判断，别为了「完整」加一个拖慢聚合的维度

#### ③ 破甲控制台还剩 7 行模型套不上

**现状**：覆盖率已达 189/196（96%）。剩 7 行全是 `auto` / `Auto` / `default` 这类**虚拟路由名**，**故意不套**（那不是模型，是还没解析的别名）。这不算 bug，除非你认为应该按「解析后的真实模型」套 —— 那要把 `apply_auto_preset` 挪到选路之后，是路由层改动。

#### ④ `en.json` 有 108 条中文占位

英文界面看到的是中文原文（`[TODO en]` 前缀已清掉，不再是机器标记）。需要**人工翻译**。可以按批次翻，注意保留 `{{count}}` 这类插值占位符原样、不要翻译占位符名字。翻完跑 `node scripts/i18n-fill-missing.mjs --check` 与 `node scripts/i18n-interpolation-check.mjs` 两个门禁。

#### ⑤ `static/` 同步闸门在多人并行下反复红

**这是结构性问题**：闸门要求「提交里的 static 产物与提交里的前端源码严格一致」。多人共享一个工作树时，A 改了前端没重建产物，B 提交时构建出的产物对应的是工作区状态（含 A 未提交的改动），于是 B 的提交里产物与源码对不上，CI 红。这一轮为此来回折腾了六七次。

**可以做的事**：要么改成「构建产物不进版本库、由 CI 构建后作为 release 资产分发」（要动部署流程），要么接受它、每次改前端都重建。**先别自己大改 CI 配置**，先问清楚。

#### ⑥ A2A 有但 AIGX 后端没有的能力（结构已占位，功能未通）

AI 运维页已经按 A2A 对齐了结构，但下面这些 AIGX 没有对应后端端点，目前只是占位/disabled：
- `/api/assistant/config`（助手配置持久化）→ 现用 localStorage
- `/api/model-access/keys`（API Key 选择）→ 无概念
- 工具执行通道（tool_calls 回灌）→ 无，**不能伪造结果**
- 审批事件只在 `/agent` 流里有，`/chat` 链路没有

#### ⑦ 4 个模型的账号凭证 401

`glm-5.3-flash` / `minimax-m3` / `kimi-k3` / `glm-5.2` 早前测出 `Auth failed: 401 Unauthorized`。**这是上游账号凭证过期，需要在面板里重新登录，代码层修不了**。

### 这个项目容易踩的坑（血泪）

1. **UTF-8 字节切片 panic**：`&s[0..2]` 在中文/非 ASCII 上会 panic（`byte index 2 is not a char boundary`）。要逐字节判 `is_ascii_digit` 或按字符处理。
2. **React effect 依赖写内联箭头 → 无限循环**：把 `() => {...}` 直接写进 `useEffect` 依赖，每帧新引用 → 无限重渲染。用 `useRef` 持有最新回调，effect 只跟真正的开关走。
3. **`contains(@class,"card")` 会命中 `.card-header`**：CSS/XPath 选择器要用 class 词边界（`contains(concat(" ",normalize-space(@class)," "), " card ")`）。
4. **`Object.entries()` 对数组会拿到下标串**：后端下发数组时前端当对象用，会得到 `0/1/2` 而不是真实键。
5. **i18n 的插值必须双花括号**：`t('上限 {n}')` 不插值，会原样显示 `{n}`，必须是 `{{n}}`。
6. **后端 camelCase vs 前端 snake_case**：`serde(rename_all="camelCase")` 的结构体，前端读 snake_case 会得到 `undefined`，且不报错、静默显示空。
7. **`Number('')` 是 0**：表单留空直接 `Number()` 会得到 0 而不是 null，保存时会把「不限制」变成「限制为 0」（= 一律拒绝）。

### 怎么验证才叫真解决

- **单测绿 ≠ 功能可用**。这个项目反复出现「单测全绿但线上不通」。
- 涉及上下游的改动，必须**部署到线上实测**，看真实响应。
- 断言必须能**区分新旧**：把修复改回去，测试必须变红。做不到就说明测试是空转。
- 报「完成」前先跑一遍：`tsc` / `eslint(准确参数)` / `vitest` / 相关 e2e / i18n 两个门禁。

---

## 环境信息（含凭据，按需替换或删除）

```
服务器:      104.223.65.202:22   root / <SSH_PASSWORD>
AIGX 目录:   /opt/aigx            （二进制 aigx + static/，systemd 单元 aigx.service，端口 9527）
线上地址:    https://api.lxseek.com
管理员:      admin / <ADMIN_PASSWORD>   （登录要过 ALTCHA 人机校验，见下）
普通用户:    ojbkxc / <USER_PASSWORD>
用户网关Key: <USER_GATEWAY_KEY>
GH_TOKEN:    <GITHUB_TOKEN>        （GitHub API 认证，仓库 ojbkxc/AIGX）
A2A 服务器:  /opt/llm-proxy/Agent2API  （agent2api.service，端口 3065，面板 ojbkxc/<A2A_PANEL_PASSWORD>）
```

**过 ALTCHA 人机校验的方法**（脚本化登录要用）：
1. `GET /api/auth/captcha` → 得到 `{salt, challenge, maxnumber, algorithm, signature}`
2. 暴力找 `number`：`sha256(salt + str(number)) == challenge`（maxnumber 通常 5~10 万，秒级）
3. 把 `{algorithm, challenge, number, salt, signature, took:0}` 序列化成紧凑 JSON，**base64 后作为字符串**放进登录体：
   `POST /api/auth/login {"email":"admin","password":"...","captcha":"<base64>"}` → `data.token`
4. 后续请求用 `Authorization: Bearer <token>`

**注意**：`GET /api/account-sessions` 返回的 `api_key` 是**掩码**的（15 字符）。要拿真实 token 得从存储层取（Postgres，`src/account_session/` 里看表结构），或者直接看 `src/log/` 的 debug 输出。

---

## 建议的开工顺序

1. 核对仓库状态与线上版本（`git log --oneline -20`、`git status`、`curl https://api.lxseek.com/version.json`）
2. 让 CI 绿（先看当前红在哪个 job、为什么）
3. 发版部署到线上
4. 实地验证：Trae 对话、破甲覆盖率、日志供应商列、AI 运维页面
5. 按上面的「未解决问题」逐条攻

**不要一上来就改代码**。先部署、先看线上真实行为，再决定改什么。
