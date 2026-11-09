//! 路由覆盖率闸门与注册表防漂移测试（#35 批2 拆模块：从 registry.rs 的 `mod tests` 机械切出，模块名仍为 `tests`）。
//!
//! 两处 `include_str!` 相对路径已随目录下沉修正：`../../main.rs`（= src/main.rs）、
//! `../api.rs`（= src/agent/api.rs）、`../mod.rs`（锚定 `exec_tool`/`exec_user_tool` 所在文件）。

use super::*;

// ═══════════════════════════════════════════════════════════════════════
// #35 批1：覆盖率闸门——数据源换成**真实路由表**，不再是手写域清单
//
// 旧闸门（本批删除）手写 17 条 REQUIRED_DOMAINS 前缀 + 13 条裸豁免前缀，
// 判据是「这个前缀下有没有工具 covers」。前缀是人手抄的，只抄了抄的人记得的域。
// 病灶：手写清单与真实路由表脱钩后，绿灯只代表「清单没被人想起来改」，
// 不代表工具面真的覆盖了项目。同一批数据的实证：
//   · 清单里的 `/api/keys` 根本没注册过路由，工具的 covers 也跟着写了幽灵
//     `/api/keys`，闸门一路绿；
//   · 301 条 /api 路由里真正被工具 owns 的只有 60 条（20%），闸门照样绿。
// 新闸门把唯一数据源换成路由注册源码：`main.rs` + `agent/api.rs` 里每条
// `.route("…")` 都是必须被登记的对象，登记去向只有三个：
//   工具 covers（有工具）／ EXEMPT_PREFIXES（有意不给）／ KNOWN_GAPS（有意欠着，带理由与归属批次）。
// 三处都不落 → 红（A）；落了但理由已失效 → 也红（B/C/D/E）。
// 于是闸门「对新缺口立刻变红、对已登记缺口允许绿」，且批 2 补掉一条却忘了删
// 台账行时同样变红——台账跟着实现走，而不是实现跟着台账躺。
//
// ── 与 CI 提取器 tools/extract_routes.py 的口径对齐（逐条）──
//   文件集合  ROUTE_FILES = [src/main.rs, src/agent/api.rs]   ←→ ROUTE_SOURCES（include_str! 编译期内联，不依赖 cwd，也不依赖 CI 镜像里有没有 python）
//   触发写法  正则 `\.route\s*\(`                             ←→ 找 `.route` 后「只允许空白再跟 (」；`.route_layer(` / `set_route_replay` 一律不算
//   路径实参  第一个顶层实参里第一个字符串字面量，须以 / 开头  ←→ 同；**有意不同**：CI 对不合状的静默跳过，本测试 panic（宁可红得难看，不许悄悄少算一条）
//   规范化    `:id` / `:id<正则>` → `{id}`、去结尾 /          ←→ norm_route_path 逐字符同逻辑
//   方法集合  第二个顶层实参文本里扫 get|post|put|delete|
//             patch|head|options 且后跟 `(`，关键字前一个字符
//             不能是标识符字符（挡 handle_delete_xxx）        ←→ scan_methods 同规则；同样不排除字符串内文本（现网 0 处差异）
//   顶层逗号  只取 args[1]，第三个及以后实参不看              ←→ 同样在第一个顶层逗号处截断
//   范围      管理面 /api* + /api-docs* + /swagger-ui*         ←→ gate_universe 同（见其文档）
// 2026-11-09 对 373 处 `.route(` 全量对跑：338 条路径、方法集合逐条相同，
// 无非字面量路径、无注释掉的 `.route(`、无单实参调用。将来路由写法变了导致两边
// 分叉时，`route_extractor_is_still_anchored` 与各处 panic 会响，不会假绿。
//
// ── 粒度：路径，不是「路径 + 方法」──
// 一条路径只要有工具 owns 就算登记。理由：风险级与审批是**工具**的属性，方法级
// 台账会把 301 条炸成 500+ 行、红到没人能推；而方法级欠债其实集中且可枚举，
// 单列成 WRITE_SIDE_PENDING（E）盯住——那才是真盲区。例：`/api/settings` 有 GET
// 有 PUT，aigx_config_view 只 owns GET，在 A/B 里它是「已覆盖」，只有 E 看得出
// 「AI 看得见却动不了」。反例存档：`/api/routing/preview` 是 POST 却是纯投影
// 计算，按「方法=写就欠债」的死规则会被误判，所以 E 判的是「该路径的写方法
// 有没有任何非 ReadOnly 工具 owns」，而不是「有没有写方法」。
// ═══════════════════════════════════════════════════════════════════════

use std::collections::{BTreeMap, BTreeSet};

/// 路由提取源，与 `tools/extract_routes.py::ROUTE_FILES` 一一对应。
const ROUTE_SOURCES: [&str; 2] = [include_str!("../../main.rs"), include_str!("../api.rs")];

/// 写方法：路径有它们、owns 它的工具却全是 ReadOnly ⇒ 方法级欠债。
const WRITE_VERBS: [&str; 4] = ["post", "put", "patch", "delete"];

/// HTTP 方法关键字表（同 CI 的 `METHOD_RE`）。
const METHOD_WORDS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// 跳过 ASCII 空白，返回第一个非空白字节下标（越界则返回 len）。
fn skip_ws(src: &[u8], mut i: usize) -> usize {
    while i < src.len() && src[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// `start` 应在开引号之后；返回收尾引号下标，反斜杠转义整体跳过。
fn quote_end(src: &[u8], mut i: usize) -> usize {
    while i < src.len() {
        match src[i] {
            b'\\' => i += 2,
            b'"' => return i,
            _ => i += 1,
        }
    }
    panic!("路由源码里有没收尾的字符串字面量——提取器口径已与 CI 分叉，请同步本测试与 tools/extract_routes.py");
}

/// `:id` / `:id<正则>` → `{id}`，并去掉结尾多余的 `/`。对齐 python `norm_path`。
fn norm_route_path(p: &str) -> String {
    let cs: Vec<char> = p.chars().collect();
    let mut out = String::with_capacity(p.len());
    let mut i = 0usize;
    while i < cs.len() {
        // 同 CI 正则的 `(?<=/)`：冒号前必须是 `/`，且紧跟字母/下划线才算参数段
        if cs[i] == ':' && i > 0 && cs[i - 1] == '/' && i + 1 < cs.len() {
            let mut name = String::new();
            let mut j = i + 1;
            if cs[j].is_ascii_alphabetic() || cs[j] == '_' {
                while j < cs.len() && (cs[j].is_ascii_alphanumeric() || cs[j] == '_') {
                    name.push(cs[j]);
                    j += 1;
                }
                if j < cs.len() && cs[j] == '<' {
                    let mut k = j + 1;
                    while k < cs.len() && cs[k] != '>' {
                        k += 1;
                    }
                    if k < cs.len() {
                        j = k + 1;
                    }
                }
                out.push('{');
                out.push_str(&name);
                out.push('}');
                i = j;
                continue;
            }
        }
        out.push(cs[i]);
        i += 1;
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// 扫 handler 表达式文本里的 HTTP 方法（对齐 CI：不排除字符串内的文本，
/// 只要求关键字前一个字符不是标识符字符、后面紧跟 `(`）。
fn scan_methods(handler: &str) -> BTreeSet<String> {
    let hb = handler.as_bytes();
    let mut ms = BTreeSet::new();
    for word in METHOD_WORDS {
        let mut s = 0usize;
        while let Some(rel) = handler[s..].find(word) {
            let k = s + rel;
            s = k + word.len();
            if k > 0 && is_ident_byte(hb[k - 1]) {
                continue;
            }
            let t = skip_ws(hb, k + word.len());
            if t < hb.len() && hb[t] == b'(' {
                ms.insert(word.to_string());
            }
        }
    }
    ms
}

/// 提取「真实路径 → HTTP 方法集合」。口径见本节开头与 CI 的对齐表；
/// 遇到不认识的写法直接 panic，绝不静默少算一条路由。
fn extract_real_routes() -> BTreeMap<String, BTreeSet<String>> {
    const ROUTE_MARK: &str = ".route";
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for src in ROUTE_SOURCES {
        let b = src.as_bytes();
        let mut pos = 0usize;
        while let Some(rel) = src[pos..].find(ROUTE_MARK) {
            let at = pos + rel;
            pos = at + ROUTE_MARK.len();
            let mut j = skip_ws(b, at + ROUTE_MARK.len());
            if j >= b.len() || b[j] != b'(' {
                continue; // `.route_layer(` / `set_route_replay`：不是路由注册
            }
            j = skip_ws(b, j + 1);
            if j >= b.len() || b[j] != b'"' {
                panic!("`.route(` 的第一个实参不是字符串字面量：{:.60}", &src[at..]);
            }
            let q = quote_end(b, j + 1);
            let raw = &src[j + 1..q];
            // 括号配对取第二个顶层实参（= handler 表达式）；字符串里的括号跳过。
            let mut depth = 1usize;
            let mut m = q + 1;
            let mut start2: Option<usize> = None;
            // end2 由下面两个 break 点产出；预赋 None 是死值（clippy unused_assignments）
            let end2 = loop {
                if m >= b.len() {
                    panic!("`.route({raw})` 的调用括号未闭合");
                }
                match b[m] {
                    b'"' => m = quote_end(b, m + 1) + 1,
                    b'(' => {
                        depth += 1;
                        m += 1;
                    }
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break Some(m);
                        }
                        m += 1;
                    }
                    b',' if depth == 1 => {
                        if start2.is_none() {
                            start2 = Some(skip_ws(b, m + 1));
                            m += 1;
                        } else {
                            break Some(m); // 第三个及以后实参不看（同 CI）
                        }
                    }
                    _ => m += 1,
                }
            };
            let handler = match (start2, end2) {
                (Some(s), Some(e)) if e > s => &src[s..e],
                _ => "",
            };
            if !raw.starts_with('/') {
                continue; // 对齐 CI：非 `/` 开头不算路由（现网 0 处）
            }
            let mut ms = scan_methods(handler);
            if ms.is_empty() {
                // 对齐 CI 的 {"?"}。真出现说明路由写法超出两边口径，
                // 由 route_extractor_is_still_anchored 响，不留在这儿糊过去。
                ms.insert("?".to_string());
            }
            out.entry(norm_route_path(raw)).or_default().extend(ms);
        }
    }
    out
}

/// 闸门宇宙 = 管理面（与 openapi.rs 的范围口径一致）：`/api*`、`/api-docs*`、
/// `/swagger-ui*`。数据面 `/v1/*` 是产品协议面（OpenAI/Anthropic 兼容，自带
/// `/v1/models` 自描述），`/health`/`/livez`/`/readyz`/`/metrics` 是编排器探针——
/// 都不是「AI 可运维的管理面」，故不入账（35 条，逐条核过）。
fn gate_universe(real: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    real.keys()
        .filter(|p| p.starts_with("/api") || p.starts_with("/swagger-ui"))
        .cloned()
        .collect()
}

/// 工具 covers 的倒排：规范化后的真实路径 → owns 它的工具名。
/// 注意是**精确路径**命中，不是前缀——前缀式匹配正是旧闸门虚报覆盖率的病根。
fn covers_index() -> BTreeMap<String, Vec<&'static str>> {
    let mut m: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    for t in all_tools() {
        for c in t.covers {
            m.entry(norm_route_path(c)).or_default().push(t.name);
        }
    }
    m
}

/// 裸前缀豁免命中判据：等于该前缀，或落在该前缀之下。
fn is_exempt(p: &str) -> bool {
    EXEMPT_PREFIXES
        .iter()
        .any(|(prefix, _)| p == *prefix || p.starts_with(&format!("{prefix}/")))
}

fn ledger_paths(ledger: &[(&str, &[&str])]) -> BTreeSet<String> {
    ledger
        .iter()
        .flat_map(|(_, paths)| paths.iter())
        .map(|p| norm_route_path(p))
        .collect()
}

/// 该路径现算出来的写方法（无写方法 → 空表；顺序固定，报错可读）。
fn write_verbs_of(real: &BTreeMap<String, BTreeSet<String>>, p: &str) -> Vec<&'static str> {
    match real.get(p) {
        Some(ms) => WRITE_VERBS
            .iter()
            .copied()
            .filter(|w| ms.contains(*w))
            .collect(),
        None => Vec::new(),
    }
}

/// 有意**不给 AI 面**的域（裸前缀 + 理由）。判据是「这个域的功能本身不提供」，
/// 不是「这个域暂时没工具」——后者必须进 `KNOWN_GAPS`。每条必须命中 ≥1 条真实
/// 路由（见 `exempt_prefixes_are_alive_and_not_covered`），否则就是僵尸例外：
/// 旧表里的 `/api/payment` 就是这么一条（0 条路由，本批已删）。
const EXEMPT_PREFIXES: &[(&str, &str)] = &[
        ("/api/auth", "登录/会话签发：人在环流程，且会让凭据过一遍 LLM"),
        (
            "/api/chat/sessions",
            "用户私域聊天记录，不给运维 Agent（旧裸前缀 `/api/chat` 太宽，本批收窄；`/api/chat/settings` 是欠债，已进 KNOWN_GAPS）",
        ),
        ("/api/epay", "支付回调（上游签名校验），不是运维动作"),
        ("/api/stripe", "支付回调（同上）"),
        ("/api/topup", "充值下单/跳转页"),
        ("/api/user/epay", "用户侧支付发起（钱 + 人在环）"),
        ("/api/user/stripe", "用户侧支付发起（同上）"),
        ("/api/tickets", "工单是用户↔人工的通道；Agent 侧另有 /api/admin/tickets"),
        ("/api/prompts", "提示词库：未纳入运维工具面规划"),
        ("/api/oauth", "上游 OAuth 跳转，必须浏览器在场"),
        ("/api/mcp", "MCP 服务自身端点（是 Agent 入口，不是被管对象）"),
        (
            "/api/agent/sessions",
            "Agent 会话存储：让 AI 读写自己的会话历史 = 自我改写（lead 裁定①不做）",
        ),
        (
            "/api/agent/approvals",
            "审批队列 REST：审批必须人在环；AI 只能经 aigx_agent_self_status 读现状，不许自己批自己",
        ),
        ("/api-docs", "OpenAPI 静态文档入口"),
        ("/swagger-ui", "文档 UI"),
        (
            "/api/prices",
            "lead 裁定④：已废弃/与 /api/pricing 重复，转豁免不补工具",
        ),
        (
            "/api/upgrade",
            "lead 裁定④：已废弃，与 /api/update 重复，转豁免不补工具",
        ),
    ];

/// 已知缺口台账：真实存在、暂无工具、且**已逐条论证过为什么现在不做**。
/// 每条带理由 + 归属批次；批 2/批 3 补掉后必须删掉该行，否则
/// `gap_ledger_has_no_stale_entries` 红。欠债有归属，才不会像旧 `REQUIRED_DOMAINS`
/// 那样半年后没人记得某条为什么被豁免。
const KNOWN_GAPS: &[(&str, &[&str])] = &[
        (
            "批 2·写侧配置族（改完全局热生效，按批1定调②一律 HighRisk 进审批）",
            &[
                "/api/chat/settings",
                "/api/zcode/captcha-pool",
                "/api/update/cleanup-artifacts",
                "/api/network/config/{config_id}",
                "/api/ip/blacklist",
                "/api/ip/blacklist/{pattern}",
                "/api/ip/whitelist",
                "/api/ip/whitelist/{pattern}",
                "/api/routing/profiles/group/{id}",
            ],
        ),
        (
            "批 2·写侧资源族（单对象增删改；可逆且作用域受限的才逐条论证降 LowRisk）",
            &[
                "/api/channels/{id}/patch",
                "/api/groups/{name}",
                "/api/models/meta/{model}",
                "/api/models/custom",
                "/api/models/custom/remove",
                "/api/models/alias/reasoning",
                "/api/models/alias/state",
                "/api/models/capabilities",
                "/api/accounts/{id}",
                "/api/network/accounts/{account_id}",
                "/api/proxy-pool/update",
                "/api/proxy-pool/remove",
                "/api/proxy-pool/enable",
                "/api/proxy-pool/sync-clash",
                "/api/plugins/enable",
                "/api/plugins/provider",
                "/api/plugins/uninstall",
                "/api/scheduled-tasks/{id}",
                "/api/orders/{id}/complete",
                "/api/checkin-center/schedules/remove",
                "/api/checkin-center/records/mark-kept-active",
                "/api/users/self",
                "/api/admin/tickets/close",
                "/api/admin/tickets/reply",
                "/api/alerts/resolve",
            ],
        ),
        (
            "批 2·运维动作族（测活/刷新/清理/重建/终止；含上游真金，逐条论证）",
            &[
                "/api/channels/chat_test",
                "/api/channels/fetch_models",
                "/api/models/refresh",
                "/api/models/metadata/refresh",
                "/api/models/test",
                "/api/accounts/test",
                "/api/proxy-pool/test",
                "/api/proxy-pool/test-account",
                "/api/logs/agg/rebuild",
                "/api/logs/agg/tz-migrate",
                "/api/logs/audits/clear",
                "/api/logs/requests/clear",
                "/api/logs/inflight/{id}/terminate",
                "/api/cache/clear",
                "/api/routing/sticky/clear",
                "/api/routing/preview",
                "/api/pricing/estimate",
                "/api/pricing/sync",
                "/api/network/restart",
                "/api/checkin-center/schedules/run",
                "/api/checkin-center/benefits/run",
                "/api/scheduled-tasks/{id}/run",
                "/api/checkin",
            ],
        ),
        (
            "不给 AI 面·资金/权益不可逆族（出账/权益剥夺一旦发生即不可回退、且 AI 无对价判断依据；逐条判据见行内注释）",
            &[
                // 判据1 不可逆出账：佣金余额划转
                "/api/aff_transfer",
                // 判据1：try_decrease_quota 全仓唯一调用点，扣款无原路退回机制
                "/api/subscription/balance/pay",
                // 判据3 错发无对价：admin 白绑 + 分组升级，已消耗不可追回
                "/api/subscription/admin/bind",
                // 判据2 权益剥夺：立即取消订阅，期间消耗不可追回（分组回退 best-effort）
                "/api/subscription/admin/subscriptions/{id}/cancel",
                // 判据2 + 影响域跨用户：剥夺他人二因子
                "/api/users/{id}/2fa",
                // 判据1 不可逆价值变更：verify_user 给调用者本人入账（等于 AI 自我增值通道）
                "/api/redemptions/redeem",
                // 判据4 影响域溢出：全库 KV upsert 覆盖，不可逆
                "/api/backup/import",
                // 判据4：同上（加密备份整体覆盖）
                "/api/backup/import-encrypted",
            ],
        ),
        (
            "不给 AI 面·影响域溢出族（判据4：test-* 会真的向配置里的收件人/webhook 外发，AI 无「自测 vs 打扰真人」判断依据）",
            &[
                "/api/notify/test-email",
                "/api/notify/test-slack",
                "/api/notify/test-webhook",
                "/api/notify/test-telegram",
                "/api/alerts/test",
            ],
        ),
        (
            "不给 AI 面·凭据/隐私/二进制流（明文密钥、含凭据的导出、加密备份、CSV 流、管理员个人域）",
            &[
                "/api/tokens/{id}/key",
                "/api/account-sessions/export",
                "/api/backup/download",
                "/api/backup/export-encrypted",
                "/api/logs/requests/export",
                "/api/logs/runtime/export",
                "/api/aff",
                "/api/users/me",
                "/api/orders/me",
                "/api/session",
                "/api/session/refresh",
            ],
        ),
        (
            "不给 AI 面·人在环流程（登录/验证码/导入向导/批量重算：需要浏览器或人工在场，工具化只会让 AI 卡在等人）",
            &[
                "/api/account-sessions/login/callback",
                "/api/account-sessions/login/cancel",
                "/api/account-sessions/login/oauth/captcha-config",
                "/api/account-sessions/login/oauth/start",
                "/api/account-sessions/login/paste",
                "/api/account-sessions/login/sms/send",
                "/api/account-sessions/login/sms/verify",
                "/api/account-sessions/login/start",
                "/api/account-sessions/login/wait",
                "/api/account-sessions/desktop-import",
                "/api/account-sessions/import",
                "/api/account-sessions/import/agent2api",
                "/api/account-sessions/import/json",
                "/api/account-sessions/batch",
                "/api/account-sessions/usage",
                "/api/zcode/captcha",
            ],
        ),
        (
            "不给 AI 面·人工试跑与对话入口（playground 真金烧上游 / assistant 对话本身；工具回路另有条目）",
            &[
                "/api/playground/chat",
                "/api/playground/images",
                "/api/playground/transcriptions",
                "/api/playground/tts",
                "/api/assistant/chat",
            ],
        ),
        (
            "批 2·观测补漏（需具体账号 ID、账号缺席即 404 的窄视图，等批 2 一起定 enum 口径）",
            &[
                "/api/zcode/captcha/config",
            ],
        ),
        (
            "批 3·形状不合（handler 不吃 State / 返回裸 Json 而非 HandlerResult / 公开探测接口），要工具化得先改 handler",
            &[
                "/api/users/check",
                "/api/public/geo-lang",
            ],
        ),
    ];

/// 方法级欠债台账：路径有写方法（POST/PUT/PATCH/DELETE），但 owns 它的工具**全是**
/// ReadOnly——「AI 看得见这个东西，却动不了」。路径粒度看不出这层（该路径算已覆盖），
/// 所以单列本表，并由 E 按真实方法集现算逐条对跑：批 2 给某条加了写工具后该行必须删。
/// 这也是 lead 裁定②（写侧 config 族一律 HighRisk 进审批）的机器化落点，
/// 免得「哪些写侧要给、给什么级别」退化成口头知识。
const WRITE_SIDE_PENDING: &[(&str, &[&str])] = &[
        (
            "批 2·写侧配置族（lead 裁定②：全局热生效的 config 一律 HighRisk 进审批；读侧已由 aigx_config_view 提供）",
            &[
                "/api/settings",
                "/api/limits",
                "/api/security/policy",
                "/api/sanitize",
                "/api/gateway/settings",
                "/api/gateway/prompt",
                "/api/update/settings",
                "/api/logs/retention",
                "/api/queue",
                "/api/ratelimit/config",
                "/api/ratios",
                "/api/ip/filter",
                "/api/notify/config",
                "/api/cline-headers",
                "/api/user-tools/policy",
                "/api/routing",
                "/api/routing/profiles",
                "/api/alerts/rules",
                "/api/pricing/sync-config",
                "/api/pricing/exchange-rates",
                "/api/models/metadata",
                "/api/debug",
                "/api/report/settings",
                "/api/checkin-center/benefits/keepalive",
            ],
        ),
        (
            "批 2·运维动作族（清理/重算：删日志、清流量缓冲、刷新汇总——可逆性差，逐条论证）",
            &[
                "/api/logs/audits",
                "/api/logs/requests",
                "/api/debug/traffic",
                "/api/usage/summary",
            ],
        ),
        (
            "批 2·写侧资源族（单对象 create：列表工具只 owns GET，POST 侧无人）",
            &[
                "/api/accounts",
                "/api/users",
                "/api/proxy-pool",
            ],
        ),
        (
            "不给 AI 面·Agent 自身边界（lead 裁定①：只读给 aigx_agent_self_status，PUT /api/agent/settings 永不让 AI 改自己）",
            &[
                "/api/agent/settings",
            ],
        ),
    ];

/// 哨兵：提取器自身必须仍在真实地提取到东西。
/// 若有人把路由注册改成 nest / 宏 / 动态拼接，宇宙会（半静默地）缩小甚至归零，
/// 那么 A–E 五条断言会因为「空集 ⊆ 任何东西」而全体假绿——那正是本闸门要根治的病，
/// 所以先给闸门自己上一道：量级地板 + 口径漂移必须在这里响。
#[test]
fn route_extractor_is_still_anchored() {
    let real = extract_real_routes();
    let universe = gate_universe(&real);
    assert!(
        universe.len() >= 200,
        "路由提取异常：管理面只提取到 {} 条（量级应为 ~300）。多半是路由写法变了导致提取口径分叉，\
             此时 A–E 会全体假绿，必须先同步本测试与 tools/extract_routes.py",
        universe.len()
    );
    assert!(
        real.values().all(|ms| !ms.is_empty()),
        "存在方法集为空的路由（提取器缺陷）"
    );
    let unknown = real
        .values()
        .flatten()
        .filter(|m| m.as_str() == "?")
        .count();
    assert_eq!(
        unknown, 0,
        "有 `.route(…)` 的 handler 里扫不出 HTTP 方法：本测试与 CI 提取器口径分叉，\
             或新写法（any()/fallback）需要两边同时支持"
    );
}

/// A：真实路由表里不允许有「谁都没登记」的路径。
/// 登记去向 = 工具 covers ∪ `EXEMPT_PREFIXES` ∪ `KNOWN_GAPS`。
#[test]
fn every_real_route_is_registered() {
    let real = extract_real_routes();
    let universe = gate_universe(&real);
    let covered = covers_index();
    let gaps = ledger_paths(KNOWN_GAPS);
    let mut orphans: Vec<String> = Vec::new();
    for p in &universe {
        if covered.contains_key(p) || gaps.contains(p.as_str()) || is_exempt(p) {
            continue;
        }
        orphans.push(format!(
            "    {p}  [写方法: {}]",
            write_verbs_of(&real, p).join("+")
        ));
    }
    assert!(
            orphans.is_empty(),
            "管理面路由未登记（{}/{} 条）：\n{}\n\
             修法三选一——① 给某条工具的 covers 加这条路径；② 有意不给 AI 面 → 进 `EXEMPT_PREFIXES` 并写理由；\
             ③ 有意先欠着 → 进 `KNOWN_GAPS` 并写理由 + 归属批次。",
            orphans.len(),
            universe.len(),
            orphans.join("\n")
        );
}

/// B：台账不许腐烂。每条 `KNOWN_GAPS` 必须仍是真实路由、仍无工具、且不与豁免重叠。
#[test]
fn gap_ledger_has_no_stale_entries() {
    let universe = gate_universe(&extract_real_routes());
    let covered = covers_index();
    for &(reason, paths) in KNOWN_GAPS {
        for p in paths {
            let np = norm_route_path(p);
            assert!(
                universe.contains(&np),
                "`KNOWN_GAPS` 僵尸行：`{np}`（批次「{reason}」）已不是真实路由，删掉它"
            );
            assert!(
                !covered.contains_key(&np),
                "`KNOWN_GAPS` 过时行：`{np}` 已被工具 covers（{:?}），从「{reason}」删掉它",
                covered.get(&np).map(|v| v.join(",")).unwrap_or_default()
            );
            assert!(
                !is_exempt(&np),
                "`KNOWN_GAPS` 与 `EXEMPT_PREFIXES` 重叠：`{np}` 既算豁免又算欠债，二者留一个"
            );
        }
    }
}

/// C：工具 `covers` 必须命中真实路由——幽灵 `/api/keys`（有 covers、无路由）就是这么抓的。
#[test]
fn tool_covers_point_at_real_routes() {
    let universe = gate_universe(&extract_real_routes());
    let mut ghosts = Vec::new();
    for t in all_tools() {
        for c in t.covers {
            if !universe.contains(&norm_route_path(c)) {
                ghosts.push(format!("    {} → covers `{c}`", t.name));
            }
        }
    }
    assert!(
            ghosts.is_empty(),
            "工具 covers 指向不存在的路由（{} 条）：\n{}\n\
             要么路由本就没有（covers 改指真实路由，参考本批 aigx_key_add/aigx_key_delete 的收敛），\
             要么路由已删（删掉 covers 里的这条）。",
            ghosts.len(),
            ghosts.join("\n")
        );
}

/// D：豁免必须活着（否则是僵尸例外），且不与工具 covers 矛盾。
/// 后半段沿用旧闸门的判据：豁免域的本意是「不提供该域的功能」，但「以该域为
/// **数据源**」的工具不算提供功能——`aigx_openapi_search` 的 covers 是
/// `/api/openapi.json`，它读 OpenAPI 文档去**发现**别的端点，而不是提供「读
/// OpenAPI 文档」这个能力。所以矛盾判据是「cover 落在该域**之下**」（比该域更长
/// 的子路径），而不是「等于该域」。
#[test]
fn exempt_prefixes_are_alive_and_not_covered() {
    let universe = gate_universe(&extract_real_routes());
    for &(prefix, reason) in EXEMPT_PREFIXES {
        assert!(
            !reason.trim().is_empty(),
            "豁免 `{prefix}` 没写理由（理由会腐烂成口头知识）"
        );
        assert!(
            universe
                .iter()
                .any(|p| p.as_str() == prefix || p.starts_with(&format!("{prefix}/"))),
            "僵尸豁免：前缀 `{prefix}`（理由：{reason}）下已无任何真实路由，删掉这条豁免"
        );
    }
    for t in all_tools() {
        for c in t.covers {
            let np = norm_route_path(c);
            for &(prefix, reason) in EXEMPT_PREFIXES {
                assert!(
                        !(np.starts_with(prefix) && np.len() > prefix.len()),
                        "豁免域与 covers 矛盾：工具 {} 覆盖了 `{np}`，而 `{prefix}` 声明整域不给 AI 面（{reason}）",
                        t.name
                    );
            }
        }
    }
}

/// E：写侧欠债台账必须与真实方法集现算结果逐条一致（不多不少）。
#[test]
fn write_side_pending_matches_reality() {
    let real = extract_real_routes();
    let covered = covers_index();
    let read_only: BTreeSet<&'static str> = all_tools()
        .iter()
        .filter(|t| t.risk == RiskLevel::ReadOnly)
        .map(|t| t.name)
        .collect();
    // 路径 → 现算依据（写方法 + owns 它的工具），报错时直接给出，不用人去复算
    let mut computed: BTreeMap<String, String> = BTreeMap::new();
    for p in real.keys().filter(|p| p.starts_with("/api/")) {
        let wv = write_verbs_of(&real, p);
        if wv.is_empty() {
            continue; // 纯只读路径，没有方法级欠债可言
        }
        let owners = match covered.get(p) {
            Some(o) if !o.is_empty() => o,
            // 没有任何工具 owns ⇒ 路径级欠债，归 KNOWN_GAPS 管（A/B 已盯）
            _ => continue,
        };
        if owners.iter().all(|n| read_only.contains(*n)) {
            computed.insert(
                p.clone(),
                format!("write={} owners={}", wv.join("+"), owners.join(",")),
            );
        }
    }
    let listed = ledger_paths(WRITE_SIDE_PENDING);
    let mut unlisted: Vec<String> = Vec::new();
    for (p, why) in &computed {
        if !listed.contains(p.as_str()) {
            unlisted.push(format!("    {p}  [{why}]"));
        }
    }
    let mut stale: Vec<String> = Vec::new();
    for p in &listed {
        if !computed.contains_key(p.as_str()) {
            stale.push(format!("    {p}"));
        }
    }
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "`WRITE_SIDE_PENDING` 与真实方法集分叉（漏记 {} 条 / 过时 {} 条）。\n\
             漏记（该路径有写方法、owns 它的工具却全是 ReadOnly，台账没登记）：\n{}\n\
             过时（台账里还欠着，实际已有非只读工具 owns）：\n{}\n\
             背景：批 2 给某条加了写工具后，必须删掉该条台账行（本测试就是逼着删）。",
        unlisted.len(),
        stale.len(),
        unlisted.join("\n"),
        stale.join("\n")
    );
}

/// 台账卫生：台账内不得重复登记，组不得空、理由不得空，两条台账语义互斥。
/// （旧 `REQUIRED_DOMAINS` 的烂法之一就是同一条径写两遍、谁也不负责。）
#[test]
fn ledgers_are_unique_and_reasoned() {
    for (tag, ledger) in [
        ("KNOWN_GAPS", KNOWN_GAPS),
        ("WRITE_SIDE_PENDING", WRITE_SIDE_PENDING),
    ] {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for &(reason, paths) in ledger {
            assert!(!reason.trim().is_empty(), "{tag} 有一组没写理由/归属批次");
            assert!(
                !paths.is_empty(),
                "{tag} 有一组空路径列表（「{reason}」），删掉它"
            );
            for p in paths {
                let np = norm_route_path(p);
                assert!(seen.insert(np.clone()), "{tag} 重复登记 `{np}`");
            }
        }
    }
    // 语义互斥：`KNOWN_GAPS` 要求「无工具 owns」，`WRITE_SIDE_PENDING` 要求「有工具 owns」。
    let gaps = ledger_paths(KNOWN_GAPS);
    let pending = ledger_paths(WRITE_SIDE_PENDING);
    for p in &gaps {
        assert!(
            !pending.contains(p.as_str()),
            "台账冲突：`{p}` 既算无工具欠债（`KNOWN_GAPS`）又算写侧欠债（`WRITE_SIDE_PENDING`）"
        );
    }
}

#[test]
fn mcp_surface_is_non_high_risk_subset() {
    for t in mcp_tools() {
        assert_ne!(
            t.risk,
            RiskLevel::HighRisk,
            "MCP 不得暴露高危工具 {}",
            t.name
        );
    }
    // MCP tools/list 必须与注册表同源（不允许再养第二份白名单）
    let list = crate::api::admin::mcp::tools_list_result(&[]);
    let names: Vec<&str> = list["tools"]
        .as_array()
        .expect("tools/list 有 tools 数组")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default())
        .collect();
    let expect: Vec<&str> = mcp_tools().map(|t| t.name).collect();
    assert_eq!(
        names, expect,
        "MCP tools/list 与 registry::mcp_tools 不一致（disabled=[] 时）"
    );
}

/// 三面（Agent/MCP/assistant）共用的禁用口径（#35 批1 后半）：列与调同时消失，
/// 且判定只认全等——前缀/大小写变体不得误伤邻近工具。
#[test]
fn disabled_tools_drop_from_visibility_and_admission() {
    let victim = mcp_tools()
        .next()
        .expect("非高危工具面非空")
        .name
        .to_string();
    let disabled = vec![victim.clone()];

    assert!(tool_disabled(&victim, &disabled));
    // 判定不放宽：改一个字符/换大小写都不算命中
    assert!(!tool_disabled(&format!("{victim}_typo"), &disabled));
    assert!(!tool_disabled(&victim.to_uppercase(), &disabled));
    assert!(!tool_disabled("", &disabled));

    let names: Vec<&str> = visible_tools(&disabled).iter().map(|t| t.name).collect();
    assert!(
        !names.contains(&victim.as_str()),
        "禁用后仍在可见集: {victim}"
    );
    assert_eq!(
        visible_tools(&[]).len() - 1,
        names.len(),
        "禁用一条只少一条，其余工具不受影响"
    );
    // 列里没了 ≠ 能调：准入判定与可见集同源
    assert!(find_visible_tool(&victim, &disabled).is_none());
    assert!(find_visible_tool(&victim, &[]).is_some());

    // 高危与禁用是两条独立门槛：高危无需禁用也不给（Agent 面另有审批矩阵）
    let high = all_tools()
        .iter()
        .find(|t| t.risk == RiskLevel::HighRisk)
        .expect("存在高危工具")
        .name
        .to_string();
    assert!(
        find_visible_tool(&high, &[]).is_none(),
        "高危不得进管理面准入"
    );
    assert!(
        visible_tools(&[]).iter().all(|t| t.name != high.as_str()),
        "高危不得出现在可见集"
    );
}

/// 执行入口的禁用门槛（#35 批1 后半，lead 追加的「结构安全」那一刀）：
/// 判定本身要能被绕过列表直接调用时仍拦得住，且文案与 runner 的执行侧兜底
/// 一字不差（同一个拒绝在三个面上有三种说法 = 排障噪音）。
#[test]
fn disabled_reject_blocks_direct_calls_with_runner_wording() {
    let victim = all_tools().first().expect("注册表非空").name.to_string();
    let disabled = vec![victim.clone()];

    let (code, msg) = disabled_reject(&victim, &disabled).expect("禁用必须有拒绝");
    assert_eq!(code, -32601, "禁用沿用 JSON-RPC「方法不可用」同码");
    assert_eq!(
        msg,
        disabled_tool_message(&victim),
        "拒绝文案必须来自单点函数，不许各处各抄一版"
    );
    assert!(
        msg.contains("已被管理员禁用") && msg.contains("disabled_tools"),
        "要同时说清现象与配置项，实际: {msg}"
    );
    // 未禁用的名字绝不误伤（含前缀/大小写变体，判定全等）
    assert!(disabled_reject(&victim, &[]).is_none());
    assert!(disabled_reject(&format!("{victim}_typo"), &disabled).is_none());
    assert!(disabled_reject(&victim.to_uppercase(), &disabled).is_none());
    assert!(disabled_reject("", &disabled).is_none());
}

/// 防空转哨兵：上面那条测的是判定本身，这一条钉「两个执行入口确实调了它」。
///
/// `exec_tool` / `exec_user_tool` 需要 [`AppState`]（单测不构造），所以无法
/// 端到端跑一遍；改为对源码文本做锚定——谁把门槛从入口摘掉（或改名），
/// 这条立刻红，避免「判定函数还在、但没人调」这种约定安全退化的样子。
#[test]
fn exec_entries_are_structurally_gated() {
    let src = include_str!("../mod.rs");
    for entry in ["pub async fn exec_tool(", "pub async fn exec_user_tool("] {
        let at = src
            .find(entry)
            .unwrap_or_else(|| panic!("找不到执行入口签名: {entry}"));
        // 函数体：从签名的 '{' 起按花括号配平取到结尾
        let open = src[at..]
            .find('{')
            .unwrap_or_else(|| panic!("{entry} 没有函数体"));
        let body_start = at + open;
        let mut depth = 0usize;
        let mut body_end = None;
        for (i, c) in src[body_start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        body_end = Some(body_start + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &src[body_start..body_end.unwrap_or(body_start)];
        assert!(
            body.contains("disabled_reject("),
            "{entry} 未调用 disabled_reject——禁用门槛必须住在执行入口里，\
                 不能只靠调用方预筛（#35 批1 后半的结构安全）"
        );
        let gate = body.find("disabled_reject(").unwrap();
        let admin_dispatch = body.find("(tool.exec)(state");
        let user_dispatch = body.find("user_exec(state");
        let call_at = admin_dispatch.or(user_dispatch).unwrap_or(usize::MAX);
        assert!(
            gate < call_at,
            "{entry} 的门槛写在执行体之后 = 已经执行过了"
        );
    }
}

/// #34 用户域工具台账（首发四件套；新增用户域工具必须同步本表）。
const USER_TOOLS: &[&str] = &[
    "aigx_checkin_run",
    "aigx_query_usage",
    "aigx_query_logs",
    "aigx_available_models",
];

/// 防漂移（#34）：用户域工具集 = 声明了 `user_exec` 且非高危的工具，
/// 必须与 [`USER_TOOLS`] 台账逐一对齐；高危工具永不声明 `user_exec`（D2）。
#[test]
fn user_tool_surface_declared_and_capped() {
    for t in all_tools() {
        if t.user_exec.is_some() {
            assert_ne!(
                t.risk,
                RiskLevel::HighRisk,
                "高危工具 {} 不得声明 user_exec（D2 硬封顶）",
                t.name
            );
            assert!(!t.covers.is_empty(), "用户域工具 {} 须声明 covers", t.name);
        }
    }
    for name in USER_TOOLS {
        let t = find_tool(name).unwrap_or_else(|| panic!("台账工具 {name} 不在注册表"));
        assert!(t.user_exec.is_some(), "{name} 未声明 user_exec");
        assert!(t.is_user_exposed(), "{name} 应对终端用户暴露");
    }
    let exposed: Vec<&str> = user_exposed_tools().map(|t| t.name).collect();
    assert_eq!(
        exposed.len(),
        USER_TOOLS.len(),
        "用户域工具集 {:?} 与 USER_TOOLS 台账不一致",
        exposed
    );
}

#[test]
fn tool_defs_valid_and_unique() {
    let mut seen = std::collections::BTreeSet::new();
    for t in all_tools() {
        assert!(t.name.starts_with("aigx_"), "{} 须 aigx_ 前缀", t.name);
        assert!(seen.insert(t.name), "工具名重复: {}", t.name);
        assert!(!t.label.is_empty(), "{} label 不能为空", t.name);
        assert!(!t.description.is_empty(), "{} description 不能为空", t.name);
        let tag = match t.risk {
            RiskLevel::ReadOnly => "只读",
            RiskLevel::LowRisk => "写操作",
            RiskLevel::HighRisk => "高危写",
        };
        assert!(
            t.description.contains(tag),
            "{} 描述未标注风险语义（应含「{tag}」）",
            t.name
        );
        let schema = (t.schema)();
        assert_eq!(schema["type"], "object", "{} schema.type", t.name);
        assert!(
            schema["properties"].is_object(),
            "{} schema.properties",
            t.name
        );
        if !t.target_key.is_empty() {
            assert!(
                schema["properties"].get(t.target_key).is_some(),
                "{} target_key={} 不在 schema.properties",
                t.name,
                t.target_key
            );
        }
        assert!(!t.covers.is_empty(), "{} 须声明 covers", t.name);
    }
}
