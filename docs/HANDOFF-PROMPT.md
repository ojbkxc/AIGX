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

**先说清一处容易搞错的根因**：不是「前端读错字段」。改前 `Logs.tsx` 读的是 `l.channel_name`
（`grep provider frontend/src/pages/Logs.tsx` 零命中，产物里也是 `channel_name`×7、`provider`×0）。
所以别去改前端字段名，那条路走不通。

**真实成因有两层**：
1. `channel_name` 是在**写日志那一刻**按 `channel_id` 点查渠道表得到的，两种情况下必然落空：
   - **Hub 兜底候选压根没有 `channel_id`**（见 `src/bridge/hub_fallback.rs` 文件头自己记的这个坑）
   - 渠道事后被删除，查不到
2. **「哪一家」这一维从来没进过日志** —— `RequestLog` 结构体里没有 provider 字段。
   而 `AttemptDetail.provider` 只取到协议族、账号池恒为 `"account"`，信息量不够。

**「供应商」的语义已经被 A2A 定死，照抄它就行**：是**家**（provider id），不是协议族。
- 渠道型 → `channel_type.as_str()`
- 账号池型 → 该渠道下账号的 `provider`（`trae` / `xiaohuanxiong` / `workbuddy`）
- A2A 依据：`request_stats/record.rs:97` `RequestEntry.provider`「实际承载本次请求的 provider id」、
  `:294` `AttemptDetail.provider`；且它明确写着**不给旧数据猜值**（空 = 不知道）

**AIGX 里这个口径已经存在但没用上**：`src/api/admin/routing_workspace.rs` 的 `channel_meta_map`
早就在这么算。把它抽成单点函数，日志与尝试明细共用即可。

**只要做这几步**：
1. `RequestLog` 加 `provider: Option<String>`（`#[serde(default)]`，存量日志不受影响）
2. 抽单点 `provider_label_for_channel(channel_type, account_provider)`：
   渠道型取协议族、账号池型取首个非空账号 provider（取不到回落 `"account"`，**不要写空串**，
   空串会被下游误读成「上游返回空」）
3. 账号池的 `channel_id → provider` 反查要做成 **O(1)**（`DashMap` 索引，在
   `load`/`upsert`（跨渠道搬迁时两边都重算）/`remove`/`remove_for_channel` 同步维护）；
   索引未命中时回落全扫一次，宁慢一次也不给错答案
4. **全仓 grep `RequestLog {` 找齐所有构造点**逐处填（数据面 19 处 + 计费 3 处 +
   内部/管理 8 处，漏一个就 E0063）。`AttemptDetail` 本就有该字段，只需统一口径
5. 前端：主值改读新字段，**但必须保留 `channel_name` 回退** —— 否则老数据会从「有名字」
   退化成 `—`，比改前更差
6. **存量日志不回填**：按 `channel_id` 现算等于猜（渠道可能已删/改名/换类型，账号池成员也会变）。
   老记录仍显示 `—` 是**预期**，不是没修好。A2A 对同一情形同样写明「不给旧数据猜值」

**日聚合不要加 provider 维度**：它与 `channel_id` 1:1，读时 join 即可，硬加会拖慢聚合与回填。

**验收判据（两条同时成立才算修好）**：
```
curl -s -H "Authorization: Bearer $TOKEN" 'http://HOST:9527/api/logs/requests?size=20' \
  | jq '.data[] | {id, channel_id, channel_name, provider}'
```
- 新记录 `provider` 非 null（账号池型应见 `trae`/`xiaohuanxiong`，渠道型见协议族）
- 老记录 `provider` 缺失，且界面仍按 `channel_name` 正常显示
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
