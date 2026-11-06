#!/usr/bin/env python3
"""
Agent2API → AIGX 账号池供应商数据同步工具。

目的：AIGX 账号池的供应商体系抄自上游 https://github.com/aimod-cc/Agent2API。
两边各自演进时，「上游加了一家、AIGX 忘了加」不会报错，只会表现为导入账号时
「未知 provider」的静默失配。本工具把这类漂移变成**可检测、可一键回写**的差异。

同步的是「数据层」（provider id / 展示名 / 表单配置覆盖），不是能力层代码——
上游 providers/ 目录深度耦合其 AccountStore/GatewayError，无法整目录拷贝；
能力（token 刷新 / 余额 / 签到 / 模型刷新）按 docs/porting-contract.md 逐家移植。

用法：
    python tools/sync_agent2api_providers.py --upstream /path/to/Agent2API --check
    python tools/sync_agent2api_providers.py --upstream /path/to/Agent2API --write
    python tools/sync_agent2api_providers.py --upstream /path/to/Agent2API --report

    --check   只检测漂移；有差异 exit 1（适合发版前 / CI 定期巡检）
    --write   把上游 provider 全集回写进 registry.rs 的 AGENT2API_PROVIDERS 基准表
    --report  生成 docs/upstream-providers-diff.md 漂移报告

上游 LICENSE 为 MIT + 附加使用声明（禁止商用/二次分发牟利/批量账号运营），
本工具只比对标识符与常量，不拷贝上游实现代码。
"""

from __future__ import annotations

import argparse
import datetime as _dt
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# AIGX 对上游 id 的归一化别名（AIGX 用自己的 kind 名）。
# raccoon 在 AIGX 里叫 xiaohuanxiong（小浣熊），见 registry.rs normalize_kind 别名表。
ALIASES = {
    "raccoon": "xiaohuanxiong",
}

# ── 能力边界豁免集（方案 C）─────────────────────────────────────────────
# AIGX 对接 Agent2API 上游采用「方案 C：四层对齐 + 能力边界标注」。
# 能力边界规则：AIGX 无实现的上游入口不自创、不显示、不假装。
#
# 这些 provider 在上游有表单/入口，但 AIGX **有意不实现**——闸门不因此报缺，
# 但会在输出里如实标注豁免原因（能力边界的可执行标注）。
#
# ⚠️ 豁免集只豁免**已知有意不实现**的家。上游新增 provider 时，
# 不在豁免集里的仍会报缺（触发跟进）——豁免不是放宽闸门。
INTENTIONAL_FORM_GAPS: dict[str, str] = {
    "lobsterai": (
        "AIGX 无 lobsterai OAuth web 登录实现（上游靠插件 "
        "providers::lobsterai::oauth）；已有账号走 account_session/import.rs "
        "通用导入，注册表条目与 catalog spec 保留"
    ),
    "qwenwork": (
        "AIGX 无 qwenwork 后端（上游由闭源插件 agent2api-providers 实现，"
        "AIGX 无插件机制）；注册表不加条目"
    ),
    "qwenwork-intl": (
        "AIGX 无 qwenwork-intl 后端（上游国际版已停用，由闭源插件实现）；"
        "注册表不加条目"
    ),
}

# ── 上游解析 ────────────────────────────────────────────────────────────────


def parse_upstream_providers(upstream: Path) -> dict[str, str]:
    """解析上游 core/providers/mod.rs 的 PROVIDERS 注册表 → {id: label}。"""
    mod_rs = (
        upstream
        / "desktop-tauri/src-tauri/server/src/server/core/providers/mod.rs"
    )
    if not mod_rs.is_file():
        # 兼容克隆下来目录名不同：再兜一层 server/src/server/core/providers/mod.rs
        candidates = list(upstream.glob("**/core/providers/mod.rs"))
        if not candidates:
            raise SystemExit(f"找不到上游 providers/mod.rs：{mod_rs}")
        mod_rs = candidates[0]
    text = mod_rs.read_text(encoding="utf-8")
    # 只取 PROVIDERS 常量体内的条目（避免匹配注释里的示例）
    m = re.search(r"pub const PROVIDERS:\s*&\[ProviderMeta\]\s*=\s*&\[(.*?)\];", text, re.S)
    if not m:
        raise SystemExit(f"{mod_rs} 里没解析到 PROVIDERS 注册表")
    body = m.group(1)
    return {
        mid: label
        for mid, label in re.findall(
            r'ProviderMeta\s*\{\s*id:\s*"([^"]+)"\s*,\s*label:\s*"([^"]+)"', body
        )
    }


def parse_upstream_form_providers(upstream: Path) -> set[str]:
    """解析上游 add-account-configs.ts 的表单 provider id 集合。"""
    candidates = list(upstream.glob("**/add-account-configs.ts"))
    if not candidates:
        return set()
    text = candidates[0].read_text(encoding="utf-8")
    return set(re.findall(r"provider:\s*'([^']+)'", text)) | set(
        re.findall(r'provider:\s*"([^"]+)"', text)
    )


# ── AIGX 侧解析 ────────────────────────────────────────────────────────────


def parse_aigx_baseline(registry_rs: Path) -> list[str]:
    """registry.rs 的 AGENT2API_PROVIDERS 基准表 → id 列表。"""
    text = registry_rs.read_text(encoding="utf-8")
    m = re.search(r"pub const AGENT2API_PROVIDERS:\s*&\[&str\]\s*=\s*&\[(.*?)\];", text, re.S)
    if not m:
        raise SystemExit(f"{registry_rs} 里没解析到 AGENT2API_PROVIDERS")
    return re.findall(r'"([^"]+)"', m.group(1))


def parse_aigx_session_kinds(registry_rs: Path) -> set[str]:
    """registry.rs 的 SESSION_PROVIDERS 登录态 kind 集合。"""
    text = registry_rs.read_text(encoding="utf-8")
    m = re.search(r"pub const SESSION_PROVIDERS:\s*&\[\(&str, SessionMeta\)\]\s*=\s*&\[(.*?)\n\];", text, re.S)
    if not m:
        raise SystemExit(f"{registry_rs} 里没解析到 SESSION_PROVIDERS")
    # 只抓条目元组 `("kind", SessionMeta {`，不误伤 SessionMeta 内部的字符串常量
    return set(re.findall(r'\(\s*"([^"]+)"\s*,\s*SessionMeta', m.group(1)))


def parse_aigx_form_providers(ts_path: Path) -> set[str]:
    """frontend/src/lib/addAccountConfigs.ts 的表单 provider id 集合。

    两种形态都要抓：
    - `provider: 'xxx'` 字面量（含工厂调用参数）
    - `SESSION_ADD_CONFIGS` 的对象键（`'cline-free': clineForm(...)` 工厂生成项）
    """
    if not ts_path.is_file():
        return set()
    text = ts_path.read_text(encoding="utf-8")
    ids = set(re.findall(r"provider:\s*'([^']+)'", text)) | set(
        re.findall(r'provider:\s*"([^"]+)"', text)
    )
    key_re = re.compile(r"(?m)^  '?([a-z0-9][a-z0-9_-]*)'?:")
    # 两个容器都要抓：主字面量 + Object.assign 补的拆家/地区伴生项
    for pat in (
        r"SESSION_ADD_CONFIGS[^=]*=\s*\{\n(.*?)\n\};",
        r"Object\.assign\(\s*SESSION_ADD_CONFIGS\s*,\s*\{\n(.*?)\n\}\);",
    ):
        for m in re.finditer(pat, text, re.S):
            ids |= set(key_re.findall(m.group(1)))
    return ids


# ── 漂移计算 ────────────────────────────────────────────────────────────────


def compute_drift(
    upstream_ids: dict[str, str],
    baseline: list[str],
    session_kinds: set[str],
    form_ids: set[str],
    upstream_form_ids: set[str],
) -> dict:
    # 基准表 AGENT2API_PROVIDERS 逐字保留上游 id（raccoon 不归一），直接对原 id
    want = set(upstream_ids)
    have = set(baseline)

    missing = sorted(want - have)  # 上游有、基准表没有 → 要补
    extra = sorted(have - want)  # 基准表有、上游没了 → 留着但提示

    # 登录态 kind 覆盖：AIGX 用自己的 kind 名（raccoon → xiaohuanxiong），经别名对齐
    session_missing = sorted(
        i for i in want if ALIASES.get(i, i) not in session_kinds
    )

    # 表单覆盖：上游有表单的 provider 在 AIGX 的 addAccountConfigs 里也应有（同样过别名）。
    # 能力边界豁免集（INTENTIONAL_FORM_GAPS）里的家有意不实现——单列出来标注原因，
    # 不进 form_missing（不触发 exit 1）。
    form_missing = sorted(
        i for i in upstream_form_ids
        if ALIASES.get(i, i) not in form_ids
        and i not in INTENTIONAL_FORM_GAPS
    )
    form_exempted = sorted(
        i for i in upstream_form_ids
        if i in INTENTIONAL_FORM_GAPS
        and ALIASES.get(i, i) not in form_ids
    )

    return {
        "missing": missing,
        "extra": extra,
        "session_missing": session_missing,
        "form_missing": form_missing,
        "form_exempted": form_exempted,
    }


def write_baseline(registry_rs: Path, current: list[str], missing: list[str]) -> None:
    """保守回写 AGENT2API_PROVIDERS：保持现有顺序，缺项追加到末尾，多余项不静默删除。"""
    text = registry_rs.read_text(encoding="utf-8")
    merged = list(current) + [i for i in missing if i not in current]
    lines = ",\n".join(f'    "{i}"' for i in merged)
    new_block = f"pub const AGENT2API_PROVIDERS: &[&str] = &[\n{lines},\n];"
    out, n = re.subn(
        r"pub const AGENT2API_PROVIDERS:\s*&\[&str\]\s*=\s*&\[.*?\];",
        new_block,
        text,
        count=1,
        flags=re.S,
    )
    if n != 1:
        raise SystemExit("回写 AGENT2API_PROVIDERS 失败：正则没匹配到唯一块")
    registry_rs.write_text(out, encoding="utf-8", newline="\n")


def write_report(path: Path, drift: dict, upstream_ids: dict[str, str]) -> None:
    today = _dt.date.today().isoformat()
    lines = [
        "# Agent2API → AIGX 供应商漂移报告",
        "",
        f"生成时间：{today}",
        "",
        "由 `tools/sync_agent2api_providers.py --report` 生成，勿手改。",
        "",
        "## 基准表缺条目（上游新增，需补 `AGENT2API_PROVIDERS`）",
        "",
    ]
    lines += [f"- `{i}`（{upstream_ids.get(i, '?')}）" for i in drift["missing"]] or ["- （无）"]
    lines += ["", "## 基准表多余条目（上游已删，确认后可移除）", ""]
    lines += [f"- `{i}`" for i in drift["extra"]] or ["- （无）"]
    lines += ["", "## 登录态条目缺失（SESSION_PROVIDERS 未覆盖）", ""]
    lines += [f"- `{i}`" for i in drift["session_missing"]] or ["- （无）"]
    lines += ["", "## 添加账号表单缺失（addAccountConfigs 未覆盖）", ""]
    lines += [f"- `{i}`" for i in drift["form_missing"]] or ["- （无）"]
    lines += ["", "## 表单豁免（能力边界，有意不实现）", ""]
    if drift["form_exempted"]:
        for i in drift["form_exempted"]:
            lines.append(f"- `{i}`：{INTENTIONAL_FORM_GAPS[i]}")
    else:
        lines.append("- （无）")
    lines += [
        "",
        "## 处理指引",
        "",
        "1. `--write` 回写基准表（有测试断言覆盖，缺家会红）。",
        "2. 登录态条目缺失的，在 `registry.rs` 的 `SESSION_PROVIDERS` 补 `SessionMeta`，",
        "   并确认 `src/account_login/` 有对应登录实现。",
        "3. 表单缺失的，按上游 `add-account-configs.ts` 把字段收敛进",
        "   `frontend/src/lib/addAccountConfigs.ts`（数据照抄，渲染不用抄）。",
        "4. 能力层（refresh / balance / checkin / models）按 `docs/porting-contract.md` 移植。",
        "5. 表单豁免集（`INTENTIONAL_FORM_GAPS`）是方案 C 能力边界的可执行标注——",
        "   AIGX 无实现的上游入口不自创、不显示。新增豁免需在脚本里登记原因。",
        "",
    ]
    path.write_text("\n".join(lines), encoding="utf-8", newline="\n")


def main() -> int:
    # Windows 控制台默认 GBK，输出中文差异时避免编码炸
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description="Agent2API → AIGX 供应商数据同步")
    ap.add_argument("--upstream", required=True, help="Agent2API 本地克隆目录")
    ap.add_argument("--check", action="store_true", help="只检测漂移，有差异 exit 1")
    ap.add_argument("--write", action="store_true", help="回写 AGENT2API_PROVIDERS 基准表")
    ap.add_argument("--report", action="store_true", help="生成 docs/upstream-providers-diff.md")
    args = ap.parse_args()

    upstream = Path(args.upstream)
    if not upstream.is_dir():
        raise SystemExit(f"上游目录不存在：{upstream}")

    registry_rs = REPO_ROOT / "src/account_session/registry.rs"
    add_cfg_ts = REPO_ROOT / "frontend/src/lib/addAccountConfigs.ts"

    upstream_ids = parse_upstream_providers(upstream)
    upstream_form_ids = parse_upstream_form_providers(upstream)
    baseline = parse_aigx_baseline(registry_rs)
    session_kinds = parse_aigx_session_kinds(registry_rs)
    form_ids = parse_aigx_form_providers(add_cfg_ts)

    drift = compute_drift(upstream_ids, baseline, session_kinds, form_ids, upstream_form_ids)

    print(f"上游 provider：{len(upstream_ids)} 家；AIGX 基准表：{len(baseline)} 条；登录态条目：{len(session_kinds)}；表单覆盖：{len(form_ids)}")

    dirty = bool(drift["missing"] or drift["extra"] or drift["session_missing"] or drift["form_missing"])

    if drift["missing"]:
        print(f"\n[基准表缺] {', '.join(drift['missing'])}")
    if drift["extra"]:
        print(f"\n[基准表多] {', '.join(drift['extra'])}（上游已无，确认后可移除）")
    if drift["session_missing"]:
        print(f"\n[登录态缺] {', '.join(drift['session_missing'])}")
    if drift["form_missing"]:
        print(f"\n[表单缺] {', '.join(drift['form_missing'])}")
    if drift["form_exempted"]:
        print(f"\n[表单豁免] {', '.join(drift['form_exempted'])}（能力边界，有意不实现）")
        for provider in drift["form_exempted"]:
            print(f"  - {provider}：{INTENTIONAL_FORM_GAPS[provider]}")

    if args.write:
        write_baseline(registry_rs, baseline, drift["missing"])
        print(f"\n已回写 AGENT2API_PROVIDERS（追加 {len(drift['missing'])} 条）→ {registry_rs}")

    if args.report:
        report_path = REPO_ROOT / "docs/upstream-providers-diff.md"
        write_report(report_path, drift, upstream_ids)
        print(f"已生成漂移报告 → {report_path}")

    if args.check and dirty:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
