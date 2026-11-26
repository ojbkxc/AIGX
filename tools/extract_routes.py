#!/usr/bin/env python3
"""#46 静态路由清单闸门：真实路由 ↔ OpenAPI 文档 ↔ 豁免表 三方对账。

axum 0.7 没有运行时枚举全部路由的 API，静态提取是唯一能拿到「真实路由表」的
路径。本脚本 grep 路由注册源码得到真实路由，再解析 openapi.rs 里的文档清单
（`real_admin_routes()`）与豁免表（`EXEMPT`），做**双向**断言：

  正向  真实 /api 路由 ⊆ 文档 ∪ 豁免     （新路由必须登记，或明确豁免+理由）
  反向  文档 ⊆ 真实路由                   （文档不许虚构不存在的路由）
  附加  豁免条目必须真实存在、且不被写进文档（豁免不会腐烂成僵尸例外）
  附加  同路径的方法集合两侧一致           （只加 DELETE 忘写文档也会被抓）
  附加  工具 covers ⊆ 管理面真实路由       （#35 批3 B3-1：工具声明的覆盖不得指向
                                          已删/从未存在的路由，也不得指向数据面；
                                          口径 = `gate_universe()`，与单测
                                          `gate_universe` / `tool_covers_point_at_real_routes`
                                          逐字同判据）

任一越界 → 打印差异并 exit 1（CI 闸门）。这补上了单测
`covers_all_real_admin_routes` 管不到的那一段：单测只保证「文档 ↔ 清单」
相等，管不到「清单 ↔ main.rs 真实路由」——两边一起漏登时断言仍绿。
同理，工具 covers 的「必须命中真实路由」原先只有单测
`tool_covers_point_at_real_routes` 在盯，跳过 cargo test 就没人管（批1 的
`/api/keys` 幽灵就是这么活下来的）；本脚本把它升级为 CI 硬闸门。

提取范围：
  - src/main.rs      （含 admin_routes / epay / stripe / openai / anthropic 等 merge 进来的 router）
  - src/agent/api.rs （agent 域独立 router，main.rs 里 .merge(crate::agent::api::router())）
  - ROUTE_FILES 里其余「子模块 router」（plugin_* / tools / maintenance / checkin_* /
    admin/agents.rs 本机 Agent 接入 / admin/aliases.rs A2A 兼容别名）；这些模块的
    `pub fn router()` 都由 main.rs `.merge(...)` 接入 —— 静态提取不逐文件扫就看不见。
  - src/agent/registry/*.rs （工具声明；仅 `mod.rs` / `tests.rs` 除外。见 extract_covers）

用法：
  python tools/extract_routes.py [repo_root]            # 闸门模式（默认，越界 exit 1）
  python tools/extract_routes.py [repo_root] --report   # 人读报告（不 fail）
  python tools/extract_routes.py [repo_root] --json     # 机器可读（不 fail）

退出码：0 = 通过 / 报告模式；1 = 闸门越界；2 = 用法错误（解析不到清单等）。
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

HTTP_METHODS = ("get", "post", "put", "delete", "patch", "head", "options")

ROUTE_FILES = [
    "src/main.rs",
    "src/agent/api.rs",
    # ── 子模块 router（A2A 对齐波 v1.0.63 起）───────────────────────
    # 这些模块的 `pub fn router()` 由 main.rs `.merge(...)` 接进主 Router。
    # 静态提取必须逐文件扫，否则「真实路由」侧会漏掉它们 —— 那会让这些
    # 路由**既不进闸门也不进文档核对**，等于静默失去保护。
    "src/api/admin/plugin_provider_settings.rs",
    "src/api/admin/plugin_icons.rs",
    "src/api/admin/plugin_theme.rs",
    "src/api/admin/plugin_ui.rs",
    "src/api/admin/plugin_login.rs",
    "src/api/admin/plugin_tasks.rs",
    "src/api/admin/plugin_helpers.rs",
    # plugins.rs 的 `router()` 只装**新增**的 startup/settings/reload 三条；
    # 老的平铺 `/api/plugins*` 仍在 main.rs（两边路由不重叠）
    "src/api/admin/plugins.rs",
    # ── 工具服务 / 维护（T7 交付）─────────────────────────────────
    # 之前漏登记：这些 router() 的 25 条 /api 路由既不进「真实」侧也不进
    # 「文档」侧，闸门对它们完全失明（[A]=0 只是假象）。
    "src/api/tool_runtime.rs",
    "src/api/admin/tools.rs",
    "src/api/admin/maintenance.rs",
    "src/api/admin/tool_image_admin.rs",
    # ── 签到台账 / 计划（T6 交付）─────────────────────────────────
    # 同上：10 条 /api 路由（含 campaigns 双路径）此前对闸门失明。
    "src/api/admin/checkin_records.rs",
    "src/api/admin/checkin_schedules.rs",
    # ── 本机 Agent 接入 / A2A 兼容别名（v1.0.63）──────────────────
    # agents.rs：本机 Agent 资源编排（预览 → 应用 → 回滚）的管理面入口，
    #   10 条 /api 路由；不登记就完全在闸门之外。
    # aliases.rs：A2A 独立端点 → AIGX 配置字段的**路径翻译层**（24 条），
    #   同一个 handler、同一个数据源、只是第二个 path。它们必须被扫进
    #   「真实」侧，否则闸门连「别名指向的 handler 还在不在」都无从判断；
    #   至于「要不要写进自描述文档」，由 EXEMPT 逐条表态（见 openapi.rs）。
    "src/api/admin/agents.rs",
    "src/api/admin/aliases.rs",
    # 第三轮合并新增的管理面 router：侧栏布局 / 图片压缩 / 助手配置 / 插件权限。
    # 都是非空 Router，必须登记进白名单，否则闸门完全看不到它们。
    "src/api/admin/sidebar_layout.rs",
    "src/api/admin/image_compress.rs",
    "src/api/admin/assistant_config.rs",
    "src/api/admin/plugin_permissions.rs",
]

# 未登记白名单的说明（由 check_router_coverage() 兜底校验）：
# - `src/tool_services/mod.rs` / `src/maintenance/mod.rs` 也有 `pub fn router()`，
#   但都是空 `Router::new()`（T0 占位入口，真实管理面在 api/admin/tools.rs 与
#   api/admin/maintenance.rs），登记只会让白名单躺两个永空文件。
# - `src/api/admin/account_sessions.rs` 目前**没有** `pub fn router()`（handler 是
#   裸函数，11 条路由在 main.rs 平铺注册），故无需登记。**若将来把路由挪进
#   account_sessions::router()，必须同步加进本白名单**，否则那 11 条账号路由
#   会立刻失明；check_router_coverage() 会自动挡住这种遗漏。

OPENAPI_RS = "src/api/admin/openapi.rs"

# 工具声明：`td(name, label, desc, risk, target_key, covers, schema, exec)`
# —— 第 6 个实参（下标 5）是 `&["/api/..."]`。仅 registry 目录下的子模块，
# 排除 `mod.rs`（`td` 定义处）与 `tests.rs`（闸门测试，不声明工具）。
REGISTRY_DIR = "src/agent/registry"
REGISTRY_SKIP = frozenset({"mod.rs", "tests.rs"})

# 路径字面量：只认以 / 开头的
PATH_RE = re.compile(r'"([^"\\]*)"')
# 方法调用：前一个字符不能是标识符字符（挡住 handle_delete_xxx / myget 之类），
# 允许 `.get(` / `(get(` / `,get(` / `::get(` / 行首 get( 等写法。
METHOD_RE = re.compile(
    r"(?<![A-Za-z0-9_])(get|post|put|delete|patch|head|options)\s*\("
)


def norm_path(p: str) -> str:
    """`:id` / `:id<regex>` → `{id}`（axum 0.7 → OpenAPI 风格）。"""
    p = re.sub(r"(?<=/):([A-Za-z_][A-Za-z0-9_]*)(<[^>]*>)?", r"{\1}", p)
    if p != "/" and p.endswith("/"):
        p = p.rstrip("/")
    return p


def split_args(src: str, start: int) -> tuple[list[str], int]:
    """从 `.route(` 的 `(` 后开始做括号配对，返回顶层实参列表与结束位置。"""
    depth = 1
    i = start
    cur: list[str] = []
    args: list[str] = []
    in_str = False
    in_char = False
    in_line_comment = False
    in_block_comment = 0
    while i < len(src):
        c = src[i]
        nxt = src[i + 1] if i + 1 < len(src) else ""
        if in_line_comment:
            if c == "\n":
                in_line_comment = False
            cur.append(c)
            i += 1
            continue
        if in_block_comment:
            if c == "/" and nxt == "*":
                in_block_comment += 1
                cur.append("  ")
                i += 2
                continue
            if c == "*" and nxt == "/":
                in_block_comment -= 1
                cur.append("  ")
                i += 2
                continue
            cur.append("\n" if c == "\n" else " ")
            i += 1
            continue
        if in_str:
            if c == "\\":
                cur.append(src[i : i + 2])
                i += 2
                continue
            if c == '"':
                in_str = False
            cur.append(c)
            i += 1
            continue
        if in_char:
            if c == "\\":
                i += 2
                continue
            if c == "'":
                in_char = False
            i += 1
            continue
        if c == "/" and nxt == "/":
            in_line_comment = True
            i += 2
            continue
        if c == "/" and nxt == "*":
            in_block_comment = 1
            i += 2
            continue
        if c == '"':
            in_str = True
            cur.append(c)
            i += 1
            continue
        # 字符字面量：仅当 ' 后面紧跟一个 char 再跟 ' （如 'a' / '\n'）
        if c == "'" and re.match(r"'\\(.)'|'[^']'", src[i : i + 5]):
            in_char = True
            i += 1
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                args.append("".join(cur))
                return args, i + 1
        elif c == "," and depth == 1:
            args.append("".join(cur))
            cur = []
            i += 1
            continue
        cur.append(c)
        i += 1
    args.append("".join(cur))
    return args, i


def extract_file(path: Path) -> dict[str, set[str]]:
    src = path.read_text(encoding="utf-8", errors="replace")
    out: dict[str, set[str]] = {}
    for m in re.finditer(r"\.route\s*\(", src):
        args, _ = split_args(src, m.end())
        if not args:
            continue
        raw_path = args[0]
        pm = PATH_RE.search(raw_path)
        if not pm or not pm.group(1).startswith("/"):
            continue
        p = norm_path(pm.group(1))
        handler = args[1] if len(args) > 1 else ""
        methods = {mm.group(1) for mm in METHOD_RE.finditer(handler)}
        if not methods:
            # `any(handler)` / 裸 handler：axum 的 `any()` 匹配**全部**方法。
            # 记作通配符 `?`，[F] 方法比对时跳过（见 collect()），否则会逼文档
            # 写出一个并不存在的 `?` 方法。
            methods = {"?"}
        out.setdefault(p, set()).update(methods)
    return out


def extract_repo(root: Path) -> dict[str, set[str]]:
    total: dict[str, set[str]] = {}
    for rel in ROUTE_FILES:
        f = root / rel
        if not f.exists():
            print(f"WARN: 缺少 {f}", file=sys.stderr)
            continue
        for p, ms in extract_file(f).items():
            total.setdefault(p, set()).update(ms)
    return total


# ── 闸门覆盖自查：所有带 `pub fn router()` 的文件必须在 ROUTE_FILES 里 ──
#
# 为什么需要：ROUTE_FILES 是**白名单**。新增的 `xxx::router()` 若没登记，
# 它的路由既不进「真实」侧（不被扫描）也不进「文档」侧（不会被报缺失）——
# 闸门对它**结构性失明**，而 `[A]=0` 看起来仍是绿的。2026-11-17 实测有
# 6 个文件（35 条 /api 路由）处于这种失明状态。
#
# 两类假阳性需排除：
#   ① 空 router（函数体只有 `Router::new()`，无 `.route(`）——T0 占位入口
#   ② 主文件（main.rs 无 router()，按 `.route()` 直接扫）
_ROUTER_DEF_RE = re.compile(r"pub\s+fn\s+router\s*\(\s*\)\s*->\s*[^{]+\{")
_ROUTE_CALL_RE = re.compile(r"\.route\s*\(")


def _router_body_is_empty(src: str) -> bool:
    m = _ROUTER_DEF_RE.search(src)
    if not m:
        return False
    # 从 `{` 起做括号配对，取函数体，看有没有 `.route(`
    i = src.index("{", m.start())
    depth, j = 0, i
    while j < len(src):
        c = src[j]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                break
        j += 1
    body = src[i : j + 1]
    return not _ROUTE_CALL_RE.search(body)


def check_router_coverage(root: Path) -> list[str]:
    """返回「有非空 router() 但不在 ROUTE_FILES」的相对路径列表。

    注意：**非空即报**，不做「是否真的有 /api 路由」的二次判断。所以看到
    非空差集时，正确处置是**先确认那个文件确实对外暴露路由**，再决定：

    - 真的有路由 → 加进 ROUTE_FILES（意味着承认「该被扫」），然后按 gate
      报出的 [A] 逐条补 openapi + paths（或 EXEMPT）。
    - 只有内部/非 /api 路由（如只服务静态资源） → 仍需加进白名单，让 gate
      自己判定它们不进 `/api` 宇宙（`real_api` 只取 `/api` 前缀）。
    - 是**空** router（`Router::new()` 无 `.route(`） → 已被 `_router_body_is_empty`
      过滤，不会报。若误报，说明该函数体解析出错，先修解析而非加白名单。

    **不要**为了让 gate 变绿而盲目加白名单 —— 那等于放弃对它的保护。
    """
    listed = set(ROUTE_FILES)
    missing: list[str] = []
    for f in sorted((root / "src").rglob("*.rs")):
        try:
            src = f.read_text(encoding="utf-8")
        except Exception:
            continue
        if not _ROUTER_DEF_RE.search(src):
            continue
        rel = f.relative_to(root).as_posix()
        if rel in listed:
            continue
        if _router_body_is_empty(src):
            continue  # 假阳性①：空 router（占位入口）
        missing.append(rel)
    return missing


def _fn_body(src: str, header_re: str) -> str | None:
    """取匹配 header_re（其结尾应为 `{`）的函数体：从 `{` 起括号配对。"""
    m = re.search(header_re, src)
    if not m:
        return None
    depth = 0
    start = m.end() - 1  # 指向 `{`
    i = start
    while i < len(src):
        if src[i] == "{":
            depth += 1
        elif src[i] == "}":
            depth -= 1
            if depth == 0:
                return src[start + 1 : i]
        i += 1
    return None


def extract_declared(root: Path) -> dict[str, set[str]]:
    """解析 openapi.rs 里 `real_admin_routes()` 的 add("path", &["get", ...]) 清单。

    用括号配对而不是纯正则：清单里既有单行 `add("/x", &["get"])`，也有换行写法，
    正则容易漏（漏了会被误报成「漏登」）。
    """
    src = (root / OPENAPI_RS).read_text(encoding="utf-8", errors="replace")
    body = _fn_body(src, r"fn real_admin_routes\(\)[^{]*\{")
    if body is None:
        return {}

    out: dict[str, set[str]] = {}
    for am in re.finditer(r"\badd\s*\(", body):
        args, _ = split_args(body, am.end())
        if not args:
            continue
        pm = PATH_RE.search(args[0])
        if not pm:
            continue
        methods = set(re.findall(r'"([^"]+)"', args[1] if len(args) > 1 else ""))
        out[norm_path(pm.group(1))] = methods
    return out


def extract_exempt(root: Path) -> dict[str, str]:
    """解析 openapi.rs 里 `const EXEMPT: &[(&str, &str)]` 的 (路径, 理由) 表。

    豁免表是「真实存在但有意不写进自描述文档」的路由的**唯一**放行依据，
    每条必须带非空理由；脚本消费它做正向断言（见模块 docstring）。
    """
    src = (root / OPENAPI_RS).read_text(encoding="utf-8", errors="replace")
    m = re.search(r"const\s+EXEMPT\s*:", src)
    if not m:
        return {}
    bracket = src.find("[", src.find("=", m.end()))
    if bracket < 0:
        return {}
    depth = 0
    i = bracket
    while i < len(src):
        if src[i] == "[":
            depth += 1
        elif src[i] == "]":
            depth -= 1
            if depth == 0:
                break
        i += 1
    block = src[bracket + 1 : i]
    out: dict[str, str] = {}
    for tm in re.finditer(r'\(\s*"([^"]*)"\s*,\s*"([^"]*)"\s*,?\s*\)', block):
        out[norm_path(tm.group(1))] = tm.group(2)
    return out


def extract_covers(root: Path) -> list[tuple[str, str]]:
    """抽取工具声明的 covers：返回 [(tool_name, cover_path)]（逐条，不去重）。

    口径与单测 `registry/tests.rs::covers_index()` 相同——**精确路径**命中，
    不做前缀匹配（前缀式匹配正是旧闸门虚报覆盖率的病根）。

    要点（B3-1 踩过的两个坑）：
    - **单条数组**（`&["/api/tokens"]`）没有尾逗号，用「先切实参再抽字符串」
      而不是「按逗号切数组」——后者会漏掉单条（实测漏 87 条）。
    - `td` 只出现在 registry 子模块里，且第一个实参必然是 `"aigx_*"` 字面量；
      以此过滤可天然跳过注释里出现的 `td(...)` 字样（如 stub 文件头注释）。

    找不到 registry 目录时返回 []（调用方按「扫描面缺失」判失败，见 collect）。
    """
    reg = root / REGISTRY_DIR
    out: list[tuple[str, str]] = []
    if not reg.is_dir():
        return out
    for f in sorted(reg.glob("*.rs")):
        if f.name in REGISTRY_SKIP:
            continue
        src = f.read_text(encoding="utf-8", errors="replace")
        for m in re.finditer(r"(?<![A-Za-z0-9_])td\s*\(", src):
            args, _ = split_args(src, m.end())
            if len(args) < 8:
                continue
            nm = PATH_RE.search(args[0])
            if not nm or not nm.group(1).startswith("aigx_"):
                continue
            name = nm.group(1)
            for c in PATH_RE.findall(args[5]):
                if c.startswith("/"):
                    out.append((name, c))
    return out


def gate_universe(real: dict[str, set[str]]) -> set[str]:
    """闸门宇宙 = 管理面（与单测 `registry/tests.rs::gate_universe()` **同判据**）。

    两处必须逐字同判据——本脚本是「cargo test 没跑时」的那道兜底闸，口径一旦
    比单测宽/窄，就会互相打脸（比没有闸门更坏）。

    范围（与单测逐条一致）：`/api*` ∪ `/swagger-ui*`。
    不含数据面 `/v1/*`、`/chat/completions`、`/health`、`/livez`、`/readyz`、
    `/metrics`、`/models*`、`/audio/*`、`/images/*`、`/embeddings`、`/rerank`、
    `/responses`、`/auth/callback-*` 等（现网 35 条）——它们不是「AI 可运维的
    管理面」。含 `/swagger-ui`（故指向它的 covers 合法，不会被误报为幽灵）。
    """
    return {p for p in real if p.startswith("/api") or p.startswith("/swagger-ui")}


def collect(root: Path) -> dict:
    real = extract_repo(root)
    decl = extract_declared(root)
    exempt = extract_exempt(root)
    covers = extract_covers(root)

    real_api = {p: m for p, m in real.items() if p.startswith("/api")}

    undoc = {p: sorted(m) for p, m in real_api.items() if p not in decl and p not in exempt}
    phantom = {p: sorted(m) for p, m in decl.items() if p not in real}
    stale_exempt = sorted(p for p in exempt if p not in real)
    exempt_documented = sorted(p for p in exempt if p in decl)
    blank_reason = sorted(p for p, r in exempt.items() if not r.strip())
    method_mismatch = {
        p: {"real": sorted(real_api[p]), "doc": sorted(decl[p])}
        for p in sorted(set(real_api) & set(decl))
        # 通配符 `?`（axum `any()`）与文档的具体方法集**不可比**：`any` 接受
        # 全部方法，文档只需列出对外承诺的那些。跳过这类路径的方法比对，
        # 否则会逼文档写一个不存在的 `?` 方法。
        if "?" not in real_api[p] and real_api[p] != decl[p]
    }

    # 工具 covers ⊆ 真实路由（口径 = gate_universe，与单测
    # `tool_covers_point_at_real_routes` 用的 `gate_universe` **逐字同判据**：
    # `/api*` ∪ `/swagger-ui*`）。**不是**全量 `real`——后者含 35 条数据面/探针
    # 路由，会让「covers 指向 /v1/chat/completions」这种非法覆盖逃过本闸门
    # （而 cargo test 的 gate C 会红）。本闸门存在的意义正是「cargo test 没跑」
    # 时兜底，口径不能比单测宽。
    universe = gate_universe(real)
    tool_names = {n for n, _ in covers}
    ghost_covers = [
        {"tool": n, "cover": c}
        for n, c in covers
        if norm_path(c) not in universe
    ]
    # 空集哨兵：covers 扫描面若塌成 0（registry 目录改名/解析口径分叉），
    # `ghost_covers` 会因为「空集 ⊆ 任何集合」而恒空 ⇒ 假绿。必须显式判失败。
    # 量级地板取 50（现网 111 条工具），远低于实际值，只在口径崩坏时触发。
    covers_scan_empty = len(tool_names) < 50

    return {
        "real_api_total": len(real_api),
        "declared_total": len(decl),
        "exempt_total": len(exempt),
        "undocumented_real": undoc,
        "phantom_documented": phantom,
        "stale_exempt": stale_exempt,
        "exempt_documented": exempt_documented,
        "exempt_blank_reason": blank_reason,
        "method_mismatch": method_mismatch,
        "tools_total": len(tool_names),
        "covers_total": len(covers),
        "ghost_covers": ghost_covers,
        "covers_scan_empty": covers_scan_empty,
    }


def report_human(res: dict, out=sys.stdout) -> None:
    print(
        f"真实 /api 路由 {res['real_api_total']} 条；文档清单 {res['declared_total']} 条；"
        f"豁免 {res['exempt_total']} 条",
        file=out,
    )
    print(f"[A] 真实有、既未登记也未豁免: {len(res['undocumented_real'])}", file=out)
    for p, ms in sorted(res["undocumented_real"].items()):
        print(f"    {p:60s} {','.join(ms)}", file=out)
    print(f"[B] 文档有、真实无（虚构）: {len(res['phantom_documented'])}", file=out)
    for p, ms in sorted(res["phantom_documented"].items()):
        print(f"    {p:60s} {','.join(ms)}", file=out)
    print(f"[C] 豁免表腐烂（条目在豁免表但真实路由已不存在）: {len(res['stale_exempt'])}", file=out)
    for p in res["stale_exempt"]:
        print(f"    {p}", file=out)
    print(f"[D] 豁免表越界（豁免项被写进了文档）: {len(res['exempt_documented'])}", file=out)
    for p in res["exempt_documented"]:
        print(f"    {p}", file=out)
    print(f"[E] 豁免缺理由: {len(res['exempt_blank_reason'])}", file=out)
    for p in res["exempt_blank_reason"]:
        print(f"    {p}", file=out)
    print(f"[F] 路径同、方法集不同: {len(res['method_mismatch'])}", file=out)
    for p, d in sorted(res["method_mismatch"].items()):
        print(f"    {p:60s} real={','.join(d['real'])} doc={','.join(d['doc'])}", file=out)
    print(
        f"[G] 工具 covers 幽灵（covers 指向不存在的路由）: {len(res['ghost_covers'])}"
        f"   [工具 {res['tools_total']} 条 / covers {res['covers_total']} 条]",
        file=out,
    )
    for g in res["ghost_covers"]:
        print(f"    {g['tool']:36s} → covers {g['cover']}", file=out)


def has_violation(res: dict) -> bool:
    return bool(
        res["undocumented_real"]
        or res["phantom_documented"]
        or res["stale_exempt"]
        or res["exempt_documented"]
        or res["exempt_blank_reason"]
        or res["method_mismatch"]
        or res["ghost_covers"]
        or res["covers_scan_empty"]
    )


def main() -> int:
    # Windows 控制台默认 GBK，输出里的 ⊆/✓ 等字符会 UnicodeEncodeError。
    # CI（Linux/UTF-8）不受影响，这里统一把流切到 UTF-8 + replace 兜底。
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass

    argv = sys.argv[1:]
    as_json = "--json" in argv
    as_report = "--report" in argv
    positional = [a for a in argv if not a.startswith("--")]
    root = Path(positional[0] if positional else ".")

    if not (root / OPENAPI_RS).exists():
        print(
            f"ERROR: 在 {root} 下找不到 {OPENAPI_RS}；请把仓库根目录作为参数传入",
            file=sys.stderr,
        )
        return 2

    res = collect(root)

    # 覆盖自查：有非空 router() 却没登记进 ROUTE_FILES → 闸门对它是失明的。
    # 这是**结构性**缺陷（不是偶发漏登记），所以并入 FAIL 判定。
    uncovered = check_router_coverage(root)
    res["router_coverage_gap"] = uncovered

    if as_json:
        print(json.dumps(res, ensure_ascii=False, indent=2))
        return 0

    report_human(res)

    if uncovered:
        print(
            f"\n[COVERAGE] 有 {len(uncovered)} 个带非空 router() 的文件未登记进 "
            f"ROUTE_FILES，其路由对闸门**完全失明**（既不校验也未登记）：",
            file=sys.stderr,
        )
        for rel in uncovered:
            print(f"    {rel}", file=sys.stderr)
        print(
            "  修法：把上述文件加进 tools/extract_routes.py 的 ROUTE_FILES。\n"
            "        空 router（只有 Router::new()）与 main.rs 不会被计入。",
            file=sys.stderr,
        )

    if as_report:
        return 0

    if has_violation(res) or uncovered:
        msg = (
            "\nROUTE-GATE FAIL：真实路由 / 文档清单 / 豁免表 / 工具 covers 不一致（见上 A–G）。\n"
            "  修法：新路由 → 加进 openapi.rs 的 real_admin_routes() 与 paths_* 文档；\n"
            "        有意不自描述 → 加进 EXEMPT 并写明理由；\n"
            "        文档写了不存在的路由 → 删掉文档条目；\n"
            "        [G] 工具 covers 指向不存在的路由 → covers 改指真实路由（或删该条 covers）。\n"
            "        [G] 若工具数是 0（扫描面塌陷）→ 检查 src/agent/registry/ 是否还在、"
            "td() 解析口径是否与 tests.rs::covers_index() 分叉。"
        )
        print(msg, file=sys.stderr)
        return 1

    print(
        "\nROUTE-GATE OK：真实 ⊆ 文档∪豁免 ✓；文档 ⊆ 真实 ✓；方法与豁免表一致 ✓；"
        "工具 covers ⊆ 真实路由 ✓"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
