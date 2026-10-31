#!/usr/bin/env python3
"""
Agent2API → AIGX 能力层同步巡检（签到名单 / 模型目录）。

`tools/sync_agent2api_providers.py` 同步的是**数据层**（provider id / 展示名 /
表单配置）。本脚本同步**能力层** —— 「上游给某家加了签到 / 加了模型目录，
AIGX 忘了跟」这一类漂移。

能力层比数据层更难察觉：provider id 还在、账号也能加，但点「同步模型」报
「不支持」或签到中心里没有这家 —— 表现为「功能缺失」而不是报错。

## 检测三项

1. **签到名单**：上游 `core/auto_checkin.rs` 的 `CHECKIN_PROVIDERS`
   vs AIGX `src/checkin/claim.rs` 的 `CHECKIN_PROVIDERS`。
   差集 = 上游新增的签到家（AIGX 缺）/ AIGX 多出来的（上游已删）。
2. **目录实现家**：上游有 `providers/<家>/models.rs` 的家
   vs AIGX `src/account_session/catalog.rs` 的 `spec_for` 覆盖的家。
   上游有而 AIGX 没有 = 「模型目录还没接」。
3. **目录端点**：上游 `<家>/models.rs` 里出现的模型路径字面量
   vs AIGX `spec_for` 里该家用的 `path`。端点变了却没跟 = 「拉目录 404」。

## 用法

    python tools/sync_agent2api_capabilities.py --upstream /path/to/Agent2API --check
    python tools/sync_agent2api_capabilities.py --upstream /path/to/Agent2API --report

    --check   只检测；有差异 exit 1（适合 CI 定期巡检）
    --report  生成 docs/upstream-capabilities-diff.md

**只比对标识符与端点常量，不拷贝上游实现代码**（上游 LICENSE 为 MIT + 附加
使用声明）。差异的处理方式是人工按 `docs/agent2api-porting.md` 的契约逐家移植。
"""

from __future__ import annotations

import argparse
import datetime as _dt
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# AIGX 对上游 id 的归一化别名（与 sync_agent2api_providers.py 保持一致）。
ALIASES = {
    "raccoon": "xiaohuanxiong",
}

# 上游 providers 目录的相对位置。
UPSTREAM_PROVIDERS = Path(
    "desktop-tauri/src-tauri/server/src/server/core/providers"
)
UPSTREAM_AUTO_CHECKIN = Path(
    "desktop-tauri/src-tauri/server/src/server/core/auto_checkin.rs"
)


def parse_upstream_checkin_providers(upstream: Path) -> list[str]:
    """上游 `CHECKIN_PROVIDERS` → id 列表。"""
    path = upstream / UPSTREAM_AUTO_CHECKIN
    if not path.exists():
        return []
    text = path.read_text(encoding="utf-8", errors="replace")
    m = re.search(
        r"pub const CHECKIN_PROVIDERS:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\];", text, re.S
    )
    if not m:
        return []
    return re.findall(r'"([^"]+)"', m.group(1))


def parse_upstream_catalog_providers(upstream: Path) -> set[str]:
    """上游有 `providers/<家>/models.rs` 的家名集合。"""
    base = upstream / UPSTREAM_PROVIDERS
    if not base.is_dir():
        return set()
    return {p.parent.name for p in base.glob("*/models.rs")}


def parse_upstream_catalog_paths(upstream: Path) -> dict[str, set[str]]:
    """上游各家 models.rs 里的目录端点 → {家: {路径…}}。

    **只认「常量定义」与「文档注释里明确写成端点的路径」**：
    - `const MODELS_PATH: &str = "/api/v1/models";` 这类常量是最可靠的来源；
    - 文档注释里的 `POST /api/agent/maas/model-types` 也算（有些家只在
      注释里写了端点，没有常量）；
    - **请求体字段名不算**（如 catpaw 里的 `model_params`）—— 那是 JSON
      字段，不是端点，抓进来会造成假阳性。
    """
    base = upstream / UPSTREAM_PROVIDERS
    out: dict[str, set[str]] = {}
    if not base.is_dir():
        return out
    for models_rs in base.glob("*/models.rs"):
        text = models_rs.read_text(encoding="utf-8", errors="replace")
        paths: set[str] = set()
        # 1) 常量定义：`const XXX_PATH: &str = "/xxx/yyy";`
        #    只认 PATH/ENDPOINT 结尾的常量 —— `BASE_URL`/`DEFAULT_BASE_URL`
        #    是站点根不是端点，抓进来会造成假阳性。
        for m in re.finditer(
            r"const\s+[A-Z0-9_]*(?:PATH|ENDPOINT)[A-Z0-9_]*\s*:\s*&\s*str\s*=\s*\"([^\"]+)\"",
            text,
        ):
            paths.add(m.group(1))
        # 2) `format!("{BASE}/path?query")` 里的字面量路径（最可靠）
        for m in re.finditer(
            r"\{[A-Z_]+\}?(/[A-Za-z0-9_./-]+(?:/[A-Za-z0-9_.*{}-]+)*)", text
        ):
            paths.add(m.group(1))
        # 3) 文档注释里的端点（`POST /xxx` / `GET /xxx`）
        #    注释可能跨行断在路径中间（`GET /wenchain/genflowpro/` 换行接
        #    `model/list`），所以先把连续的注释行拼起来再匹配。
        comment_lines: list[str] = []
        for line in text.split("\n"):
            stripped = line.lstrip()
            if stripped.startswith("//"):
                comment_lines.append(stripped[2:].strip())
                continue
            if comment_lines:
                joined = " ".join(comment_lines)
                for m in re.finditer(r"\b(?:GET|POST|PUT)\s+(/[A-Za-z0-9_./-]+)", joined):
                    paths.add(m.group(1))
                comment_lines = []
        if paths:
            out[models_rs.parent.name] = paths
    return out


def parse_aigx_checkin_providers(claim_rs: Path) -> list[str]:
    """AIGX `src/checkin/claim.rs` 的 `CHECKIN_PROVIDERS` → id 列表。"""
    if not claim_rs.exists():
        return []
    text = claim_rs.read_text(encoding="utf-8", errors="replace")
    m = re.search(
        r"pub const CHECKIN_PROVIDERS:\s*&\[&str\]\s*=\s*&\[(.*?)\];", text, re.S
    )
    if not m:
        return []
    return re.findall(r'"([^"]+)"', m.group(1))


def parse_aigx_catalog_specs(catalog_rs: Path) -> dict[str, str]:
    """AIGX `src/account_session/catalog.rs` 的 `spec_for` → {家: 目录路径}。

    家名用 spec 匹配分支里的字符串字面量（`"qoder" => Some(...)` 等），
    路径取同一分支里的 `path: "..."`。
    """
    if not catalog_rs.exists():
        return {}
    text = catalog_rs.read_text(encoding="utf-8", errors="replace")
    m = re.search(r"fn spec_for\(provider: &str\) -> Option<CatalogSpec> \{(.*?)\n\}\n", text, re.S)
    if not m:
        return {}
    body = m.group(1)
    out: dict[str, str] = {}
    # 逐行扫描：分支头给出 kind 字面量，`path:` 行给出该家的目录路径。
    current_kinds: list[str] = []
    in_spec = False
    for line in body.split("\n"):
        stripped = line.strip()
        # 分支头：`"qoder" => Some(CatalogSpec {` / `"a" | "b" => Some(CatalogSpec {`
        # 匹配 `"a" =>` / `"a" | "b" =>` / `"a" | "b" | "c" =>`（任意段数）
        head = re.match(r'^(?P<kinds>(?:"[a-z0-9_-]+"\s*\|\s*)*"[a-z0-9_-]+")\s*=>\s*Some\(CatalogSpec\s*\{', stripped)
        if head:
            current_kinds = re.findall(r'"([a-z0-9_-]+)"', head.group("kinds"))
            in_spec = True
            continue
        if in_spec:
            path_m = re.match(r'^path:\s*"([^"]*)"', stripped)
            if path_m:
                for kind in current_kinds:
                    out.setdefault(kind, path_m.group(1))
                in_spec = False
                current_kinds = []
    return out


def compute_capability_drift(
    upstream_checkin: list[str],
    aigx_checkin: list[str],
    upstream_catalog: set[str],
    aigx_catalog: dict[str, str],
    upstream_paths: dict[str, set[str]],
) -> dict:
    up_checkin = {ALIASES.get(k, k) for k in upstream_checkin}
    ai_checkin = set(aigx_checkin)

    up_catalog = {ALIASES.get(k, k) for k in upstream_catalog}
    ai_catalog = set(aigx_catalog)

    # 端点比对：把 AIGX 的完整路径拆成 path 部分与 query 部分
    endpoint_diff: list[tuple[str, str, str]] = []
    for kind in sorted(up_catalog & ai_catalog):
        aigx_path = aigx_catalog[kind]
        up_paths = upstream_paths.get(kind, set())
        if not up_paths:
            continue
        # AIGX 路径里去掉 query
        aigx_base = aigx_path.split("?")[0]
        # 上游路径里若有一条与 AIGX 的 base 匹配（含前缀关系），视为一致
        matched = any(
            aigx_base.endswith(p) or p.endswith(aigx_base) or aigx_base in p
            for p in up_paths
        )
        if not matched:
            endpoint_diff.append((kind, aigx_path, ", ".join(sorted(up_paths))))

    return {
        # 上游有签到、AIGX 没有
        "checkin_missing": sorted(up_checkin - ai_checkin),
        # AIGX 有签到、上游已删（或改名）
        "checkin_extra": sorted(ai_checkin - up_checkin),
        # 上游有模型目录实现、AIGX 没接
        "catalog_missing": sorted(up_catalog - ai_catalog),
        # 目录端点不一致
        "catalog_endpoint_diff": endpoint_diff,
    }


def write_report(path: Path, drift: dict, upstream_checkin: list[str]) -> None:
    now = _dt.datetime.now().strftime("%Y-%m-%d %H:%M")
    lines = [
        "# Agent2API → AIGX 能力层漂移报告",
        "",
        f"生成时间：{now}",
        "",
        "本报告由 `tools/sync_agent2api_capabilities.py --report` 生成。",
        "能力层 = 签到名单 / 模型目录实现 / 目录端点。",
        "",
        "## 签到名单",
        "",
        f"上游：{', '.join(upstream_checkin) or '（无）'}",
        "",
        f"**AIGX 缺签到**：{', '.join(drift['checkin_missing']) or '（无）'}",
        "",
        f"**AIGX 多签到**：{', '.join(drift['checkin_extra']) or '（无）'}",
        "",
        "## 模型目录",
        "",
        f"**AIGX 缺目录实现**：{', '.join(drift['catalog_missing']) or '（无）'}",
        "",
        "**目录端点不一致**：",
        "",
    ]
    if drift["catalog_endpoint_diff"]:
        lines.append("| 家 | AIGX 现用 | 上游 models.rs 里的路径 |")
        lines.append("| --- | --- | --- |")
        for kind, aigx_path, up_paths in drift["catalog_endpoint_diff"]:
            lines.append(f"| {kind} | `{aigx_path}` | `{up_paths}` |")
    else:
        lines.append("（无）")
    lines += [
        "",
        "## 处理方式",
        "",
        "差异不自动回写 —— 能力层改动要按 `docs/agent2api-porting.md` 的契约逐家移植",
        "（签名 / 风控头 / 响应形态），不能用脚本盲改。",
        "",
    ]
    path.write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description="Agent2API → AIGX 能力层同步巡检")
    ap.add_argument("--upstream", required=True, help="Agent2API 本地克隆目录")
    ap.add_argument("--check", action="store_true", help="只检测漂移，有差异 exit 1")
    ap.add_argument("--report", action="store_true", help="生成 docs/upstream-capabilities-diff.md")
    args = ap.parse_args()

    upstream = Path(args.upstream)
    if not upstream.is_dir():
        raise SystemExit(f"上游目录不存在：{upstream}")

    claim_rs = REPO_ROOT / "src/checkin/claim.rs"
    catalog_rs = REPO_ROOT / "src/account_session/catalog.rs"

    upstream_checkin = parse_upstream_checkin_providers(upstream)
    upstream_catalog = parse_upstream_catalog_providers(upstream)
    upstream_paths = parse_upstream_catalog_paths(upstream)
    aigx_checkin = parse_aigx_checkin_providers(claim_rs)
    aigx_catalog = parse_aigx_catalog_specs(catalog_rs)

    drift = compute_capability_drift(
        upstream_checkin, aigx_checkin, upstream_catalog, aigx_catalog, upstream_paths
    )

    print(
        f"上游签到 {len(upstream_checkin)} 家 / 目录实现 {len(upstream_catalog)} 家；"
        f"AIGX 签到 {len(aigx_checkin)} 家 / 目录 {len(aigx_catalog)} 家"
    )
    if drift["checkin_missing"]:
        print(f"\n[签到缺] {', '.join(drift['checkin_missing'])}")
    if drift["checkin_extra"]:
        print(f"\n[签到多] {', '.join(drift['checkin_extra'])}（上游已删，确认后可移除）")
    if drift["catalog_missing"]:
        print(f"\n[目录缺] {', '.join(drift['catalog_missing'])}")
    for kind, aigx_path, up_paths in drift["catalog_endpoint_diff"]:
        print(f"\n[端点不一致] {kind}: AIGX={aigx_path} 上游={up_paths}")

    if args.report:
        report_path = REPO_ROOT / "docs/upstream-capabilities-diff.md"
        write_report(report_path, drift, upstream_checkin)
        print(f"\n已生成能力层漂移报告 → {report_path}")

    dirty = bool(
        drift["checkin_missing"]
        or drift["checkin_extra"]
        or drift["catalog_missing"]
        or drift["catalog_endpoint_diff"]
    )
    if args.check and dirty:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
