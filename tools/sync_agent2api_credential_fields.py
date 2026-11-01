#!/usr/bin/env python3
"""
Agent2API → AIGX 凭证字段模板同步巡检。

`sync_agent2api_providers.py` 管 provider id 覆盖，`sync_agent2api_capabilities.py`
管签到/目录能力。本脚本管**账号字段模板** —— 「上游给某家的账号记录加了字段
（如 trae 的 authClientId），AIGX 的 `Credential.fields` 模板没跟」这一类漂移。

## 上游字段从哪来（按优先级）

1. `Credential::patch_fields()` 里的 `("key", ...)` 字面量 —— 落库键的权威
   来源（trae 等家用它做字段补丁式写回）。
2. `from_payload()` 里 `secret(payload, &["key", ...])` / `identity(...)`
   候选键数组的**第一个** —— qoder 等家没有 patch_fields，落盘键在这里。
3. `pub struct Credential(s)` 的字段 + `#[serde(rename = "...")]` ——
   codearts 等 serde 直存家；无 rename 时保留字段原名（上游落库同名）。
   少数家的凭证结构不在 credentials.rs（如 officeace 的在 `signer.rs`），
   由 `EXTRA_FILES` 指定旁路文件按同一套 struct 规则解析。
4. `to_record_value()` 里 `record.insert("key", ...)` —— 通用键投影
   （accessToken / refreshToken / expiresAt / userId）。被投影的专有字段
   （如 codearts 的 access_key_id）视为「该家主凭证」，不进 fields 模板。

`required` 从 `valid()` 方法体里的 `self.<field>...is_empty()` 检查提取
（字段名经同一家的键归一后匹配）。

## AIGX 侧落点

`src/account_session/registry.rs` 的 `CREDENTIAL_FIELDS` 常量表，
BEGIN/END 标记之间是**生成区**（手工改动会被 --write 冲掉）。

## 用法

    python tools/sync_agent2api_credential_fields.py --upstream /path/to/Agent2API --check
    python tools/sync_agent2api_credential_fields.py --upstream /path/to/Agent2API --write
    python tools/sync_agent2api_credential_fields.py --upstream /path/to/Agent2API --report

    --check   只检测；有差异 exit 1（适合 CI 定期巡检）
    --write   重写 registry.rs 生成区（只动上游能解析到的家，其余原样保留）
    --report  生成 docs/upstream-credential-fields-diff.md

**只比对/生成字段名与用途说明，不拷贝上游实现代码**（上游 LICENSE 为
MIT + 附加使用声明）。
"""

from __future__ import annotations

import argparse
import datetime as _dt
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

UPSTREAM_PROVIDERS = Path(
    "desktop-tauri/src-tauri/server/src/server/core/providers"
)

REGISTRY_RS = REPO_ROOT / "src/account_session/registry.rs"

BEGIN_MARK = "// BEGIN AGENT2API-CREDENTIAL-FIELDS"
END_MARK = "// END AGENT2API-CREDENTIAL-FIELDS"

# AIGX 对上游 id 的归一化别名（与 sync_agent2api_providers.py 保持一致）。
ALIASES = {
    "raccoon": "xiaohuanxiong",
}

# 凭证结构不在 credentials.rs 的家 → 旁路文件（按同一套 struct 规则解析）。
EXTRA_FILES = {
    "officeace": "signer.rs",
}

# 上游通用键的**归一形态** → 不参与「字段模板」比对（这些是 AIGX
# `Credential.secret` / `refresh_secret` / `expires_at` 等一等公民字段承担的，
# 模板只管「额外具名字段」）。
GENERIC_CANON = {"access_token", "refresh_token", "expires_at", "user_id"}

# 账号记录里的非凭证字段（canon 空间）—— 这些是 AIGX `AccountSession`
# 的一等字段（id / base_url / provider / name），不属于 `Credential.fields` 模板。
NON_CREDENTIAL_FIELDS = {"id", "base_url", "provider", "created_at", "updated_at"}


def canon_key(name: str) -> str:
    """键名归一（dedup 用）：camelCase → snake_case，已 snake 的原样。

    同一逻辑字段在上游常以两种形态出现（`patch_fields` 给 `machineId`、
    struct 字段是 `machine_id`）——归一后只算一个字段。
    """
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


@dataclass
class FieldSpec:
    """一个凭证字段（与 registry.rs 的 CredentialField 对应）。"""

    name: str
    required: bool = False
    hint: str = ""


@dataclass
class ProviderFields:
    """一家的字段模板解析结果。"""

    fields: dict[str, FieldSpec] = field(default_factory=dict)
    sources: set[str] = field(default_factory=set)  # 解析来源（报告用）


def snake_to_camel(name: str) -> str:
    parts = name.split("_")
    return parts[0] + "".join(p.capitalize() for p in parts[1:] if p)


def extract_doc_hint(lines: list[str], field_line_idx: int) -> str:
    """取字段上方最近的连续 `///` 注释块的第一行作为用途说明。"""
    i = field_line_idx - 1
    doc: list[str] = []
    while i >= 0:
        stripped = lines[i].strip()
        if stripped.startswith("///"):
            doc.append(stripped[3:].strip())
            i -= 1
            continue
        # serde 属性行跳过（继续向上找注释）
        if stripped.startswith("#["):
            i -= 1
            continue
        break
    if not doc:
        return ""
    first = list(reversed(doc))[0]
    # 压缩空白、去掉破折号前缀，限长 60（hint 只是管理面提示）
    first = re.sub(r"\s+", " ", first).strip(" —-")
    return first[:60]


def parse_struct_fields(text: str) -> tuple[dict[str, str], dict[str, str], bool]:
    """解析 `pub struct ...Credential(s)` 的字段。

    返回 (字段名 → 落库键名, 字段名 → hint, struct 是否带 Serialize derive)。
    落库键名规则：`#[serde(rename = "x")]` 优先；无 rename 保留字段原名
    （上游如 codearts 落库就是 snake_case 原名）。
    **字段原名只在 struct 带 `Serialize` 时可信为落库键**（serde 直存家）；
    否则 struct 只作 required/hint 的载体（trae/officeace 等家的落库键在
    patch_fields / pick 候选里）。
    """
    key_of: dict[str, str] = {}
    hint_of: dict[str, str] = {}
    # derive 行与 struct 头之间允许夹着文档注释/属性
    m = re.search(
        r"#\[derive\((?P<derives>[^)]*)\)\][\s\S]{0,200}?"
        r"pub struct\s+(?P<name>[A-Za-z]*Credentials?)\s*\{(?P<body>.*?)\n\}",
        text,
        re.S,
    )
    if not m:
        return key_of, hint_of, False
    serializable = "Serialize" in m.group("derives")
    body = m.group("body")
    lines = body.split("\n")
    pending_rename: str | None = None
    for idx, line in enumerate(lines):
        stripped = line.strip()
        rename_m = re.match(r'#\[serde\(rename\s*=\s*"([^"]+)"\)\]', stripped)
        if rename_m:
            pending_rename = rename_m.group(1)
            continue
        field_m = re.match(r"pub\s+([a-z][a-z0-9_]*)\s*:\s*([A-Za-z0-9_<>, ]+)", stripped)
        if field_m:
            fname = field_m.group(1)
            # 只收字符串字段（Region 这类枚举不是落库凭证字段）
            ftype = field_m.group(2).strip()
            if not ftype.startswith("String") and not ftype.startswith("Option<String>"):
                pending_rename = None
                continue
            key_of[fname] = pending_rename or fname
            hint_of[fname] = extract_doc_hint(lines, idx)
            pending_rename = None
    return key_of, hint_of, serializable


def parse_patch_fields(text: str) -> list[str]:
    """`patch_fields()` 里的落库键字面量（最权威）。

    函数体边界取「下一个同缩进 fn 或 impl 收尾」——函数内嵌的 if 块
    （`fields.push(("key", ...))`）不能在截取时被截掉。
    """
    m = re.search(
        r"fn patch_fields\(&self\)[\s\S]*?(?=\n    (?:pub )?fn |\n\})",
        text,
    )
    if not m:
        return []
    return re.findall(r'\("([A-Za-z][A-Za-z0-9_]*?)"\s*,', m.group(0))


def parse_payload_key_candidates(text: str) -> list[str]:
    """`from_payload()` / `from_record()` 里 `secret/identity/pick/text`
    候选键数组的**第一个**（主键名），以及 `text_either(a, b, "key")`
    单键形式（trae 的 uid/nickname/enterpriseId 从嵌套 account{} 读）。"""
    keys: list[str] = []
    for m in re.finditer(
        r"(?:secret|identity|pick|text)\(\s*(?:payload|record)\s*,\s*&\[([^\]]+)\]",
        text,
    ):
        candidates = re.findall(r'"([^"]+)"', m.group(1))
        if candidates:
            keys.append(candidates[0])
    for m in re.finditer(r'text_either\([^)]*?"([^"]+)"\)', text):
        keys.append(m.group(1))
    # expiresAt 族：`["expiresAt", "expires", ...]` 直接数组
    for m in re.finditer(r'\["(expires[A-Za-z]*)"[^\]]*\]', text):
        keys.append(m.group(1))
    return keys


def parse_json_projection_keys(text: str) -> list[str]:
    """`to_value()` / `to_record_value()` 内 `json!` / Map 字面量的字符串键
    （落盘形状的直接证据，accio 等手写 json! 投影的家）。"""
    keys: list[str] = []
    for m in re.finditer(
        r"fn (?:to_value|to_record_value)\(&self\)[\s\S]*?(?=\n    (?:pub )?fn |\n\})",
        text,
    ):
        keys += re.findall(r'^\s*"([A-Za-z][A-Za-z0-9_]*)"\s*:', m.group(0), re.M)
    return keys


def parse_record_inserts(text: str) -> list[str]:
    """`to_record_value()` 里 `record.insert("key", ...)` 的键（通用键投影）。"""
    return re.findall(r'record\.insert\("([A-Za-z][A-Za-z0-9_]*)"', text)


def parse_generic_projection_fields(text: str) -> set[str]:
    """`to_record_value()` 里被投影成**通用键**的 struct 字段名。

    codearts 这类家把专有字段投影进通用键
    （`record.insert("accessToken", ... self.access_key_id ...)`）——
    该专有字段即「这家的主凭证」，对 AIGX 是 `Credential.secret` 承担，
    不应出现在 fields 模板里。
    """
    out: set[str] = set()
    for m in re.finditer(
        r'record\.insert\("(?:accessToken|refreshToken|expiresAt|userId)"'
        r"[^;]*?self\.([a-z][a-z0-9_]*)",
        text,
    ):
        out.add(m.group(1))
    return out


def parse_valid_required(text: str) -> set[str]:
    """`valid()` 里被检查非空的 struct 字段名（snake_case）。"""
    m = re.search(r"fn valid\(&self\)\s*->\s*bool\s*\{(.*?)\}", text, re.S)
    if not m:
        return set()
    return set(re.findall(r"self\.([a-z][a-z0-9_]*)\.[^\n]*?is_empty\(\)", m.group(1)))


def parse_upstream_provider(cred_rs: Path) -> ProviderFields:
    """解析一家的 credentials.rs → 字段模板。"""
    text = cred_rs.read_text(encoding="utf-8", errors="replace")
    result = ProviderFields()

    struct_keys, struct_hints, struct_serde = parse_struct_fields(text)
    patch_keys = parse_patch_fields(text)
    payload_keys = parse_payload_key_candidates(text)
    record_keys = parse_record_inserts(text)
    json_keys = parse_json_projection_keys(text)
    required_fields = parse_valid_required(text)
    # 被投影成通用键的专有字段（codearts 的 access_key_id → accessToken）——
    # 对 AIGX 是 secret/refresh_secret 承担，从 fields 模板剔除。
    generic_carriers = parse_generic_projection_fields(text)

    # struct 字段原名只在「serde 直存家」（带 Serialize derive）可信为
    # 落库键；其余家（trae/officeace/qoder）落库键以 patch_fields /
    # pick 候选为准，struct 字段只作 required/hint 的载体。
    struct_direct_keys = list(struct_keys.values()) if struct_serde else []

    # 同一逻辑字段可能以 camel/snake 两形态出现（patch_fields 给 camel、
    # struct 字段是 snake）——按 canon 归一去重，展示名取**优先级最高**
    # 的来源形态：patch_fields > from_payload/pick > struct > json!/record。
    ordered: dict[str, tuple[str, int]] = {}  # canon → (展示名, 优先级)
    for priority, keys in (
        (0, patch_keys),
        (1, payload_keys),
        (2, struct_direct_keys),
        (3, json_keys + record_keys),
    ):
        for key in keys:
            canon = canon_key(key)
            if canon not in ordered:
                ordered[canon] = (key, priority)

    for canon, (display, _priority) in ordered.items():
        if canon in GENERIC_CANON or canon in generic_carriers:
            continue  # 通用键（或其投影载体）由 AIGX Credential.secret / refresh_secret 承担
        if canon in NON_CREDENTIAL_FIELDS:
            continue  # id / base_url 等是 AccountSession 一等字段，不是 Credential.fields
        spec = FieldSpec(name=display)
        # required：struct 字段被 valid() 检查 → 归一后匹配
        if canon in required_fields:
            spec.required = True
        # hint：struct 字段注释（字段名 canon 化后匹配）
        for fname, fkey in struct_keys.items():
            if canon_key(fkey) == canon and struct_hints.get(fname):
                spec.hint = struct_hints[fname]
                break
        result.fields[canon] = spec

    if patch_keys:
        result.sources.add("patch_fields")
    if payload_keys:
        result.sources.add("from_payload")
    if struct_keys:
        result.sources.add("struct")
    if record_keys:
        result.sources.add("to_record_value")
    return result


def parse_upstream(upstream: Path) -> dict[str, ProviderFields]:
    """上游全部 provider 的字段模板 → {家(归一后): ProviderFields}。"""
    base = upstream / UPSTREAM_PROVIDERS
    out: dict[str, ProviderFields] = {}
    if not base.is_dir():
        return out
    for cred_rs in sorted(base.glob("*/credentials.rs")):
        provider = ALIASES.get(cred_rs.parent.name, cred_rs.parent.name)
        parsed = parse_upstream_provider(cred_rs)
        if parsed.fields:
            out[provider] = parsed
    # 凭证结构旁路（officeace/signer.rs 等）
    for provider, filename in EXTRA_FILES.items():
        alt = base / provider / filename
        if provider in out or not alt.exists():
            continue
        parsed = parse_upstream_provider(alt)
        if parsed.fields:
            out[provider] = parsed
    return out


def parse_aigx_registry(text: str) -> dict[str, list[FieldSpec]]:
    """解析 registry.rs 生成区里现有的 CREDENTIAL_FIELDS 表。

    兼容紧凑形态（--write 直接产出）与 rustfmt 展开形态（入库形态）：
    `("kind", &[...])` 与 `(\n    "kind",\n    &[...],\n)` 都认。
    """
    m = re.search(
        re.escape(BEGIN_MARK) + r"(.*?)" + re.escape(END_MARK), text, re.S
    )
    if not m:
        return {}
    block = m.group(1)
    out: dict[str, list[FieldSpec]] = {}
    # 逐条匹配：("kind", &[ CredentialField { ... }, ... ])（允许换行与尾逗号）
    for entry in re.finditer(
        r'\(\s*"([a-z0-9_-]+)"\s*,\s*&\[(.*?)\]\s*,?\s*\)', block, re.S
    ):
        kind = entry.group(1)
        specs: list[FieldSpec] = []
        for fm in re.finditer(
            r'CredentialField\s*\{\s*name:\s*"([^"]+)"\s*,\s*required:\s*(true|false)\s*,\s*hint:\s*"((?:[^"\\]|\\.)*)"\s*,?\s*\}',
            entry.group(2),
        ):
            specs.append(
                FieldSpec(
                    name=fm.group(1),
                    required=fm.group(2) == "true",
                    hint=fm.group(3).replace('\\"', '"'),
                )
            )
        out[kind] = specs
    return out


def render_field(spec: FieldSpec) -> list[str]:
    hint = spec.hint.replace("\\", "\\\\").replace('"', '\\"')
    required = "true" if spec.required else "false"
    # 与 rustfmt 对长表的实际展开形态一致（--write 产物即入库形态）
    return [
        "            CredentialField {",
        f'                name: "{spec.name}",',
        f"                required: {required},",
        f'                hint: "{hint}",',
        "            },",
    ]


def render_block(
    upstream: dict[str, ProviderFields],
    existing: dict[str, list[FieldSpec]],
) -> tuple[str, list[str]]:
    """生成 BEGIN/END 之间的新内容。

    只重写上游能解析到的家；AIGX 有声明但上游没解析到的**原样保留**
    （返回 kept 列表提示人工核对）。输出即 rustfmt 展开形态。
    """
    lines: list[str] = []
    lines.append("pub const CREDENTIAL_FIELDS: &[(&str, &[CredentialField])] = &[")
    kept: list[str] = []
    for kind in sorted(set(upstream) | set(existing)):
        if kind in upstream:
            fields = sorted(upstream[kind].fields.values(), key=lambda f: f.name)
        else:
            fields = existing[kind]
            kept.append(kind)
        if not fields:
            continue
        lines.append("    (")
        lines.append(f'        "{kind}",')
        lines.append("        &[")
        for spec in fields:
            lines.extend(render_field(spec))
        lines.append("        ],")
        lines.append("    ),")
    lines.append("];")
    return "\n".join(lines), kept


def compute_drift(
    upstream: dict[str, ProviderFields],
    existing: dict[str, list[FieldSpec]],
) -> dict:
    """上游 vs AIGX 的字段模板漂移（比对在 canon 归一空间进行）。"""
    missing_providers: list[str] = []  # 上游有模板、AIGX 没声明的家
    field_diffs: dict[str, dict] = {}  # 双方都有的家：字段级差集
    for kind in sorted(upstream):
        up_canons = set(upstream[kind].fields)
        if kind not in existing:
            if up_canons:
                missing_providers.append(kind)
            continue
        ai_canons = {canon_key(f.name) for f in existing[kind]}
        # 报告里用上游玩耍名（patch_fields 权威形态）展示缺失字段
        missing = sorted(
            upstream[kind].fields[c].name for c in up_canons - ai_canons
        )
        extra = sorted(
            f.name for f in existing[kind] if canon_key(f.name) not in up_canons
        )
        if missing or extra:
            field_diffs[kind] = {"missing": missing, "extra": extra}
    extra_providers = sorted(set(existing) - set(upstream))
    return {
        "missing_providers": missing_providers,
        "extra_providers": extra_providers,
        "field_diffs": field_diffs,
    }


def write_report(path: Path, drift: dict, upstream: dict[str, ProviderFields]) -> None:
    now = _dt.datetime.now().strftime("%Y-%m-%d %H:%M")
    lines = [
        "# Agent2API → AIGX 凭证字段模板漂移报告",
        "",
        f"生成时间：{now}",
        "",
        "本报告由 `tools/sync_agent2api_credential_fields.py --report` 生成。",
        "字段模板 = 各家账号记录里除通用键（accessToken/refreshToken/expiresAt/userId）",
        "外的具名凭证字段（AIGX `Credential.fields` 应与上游落库键对齐）。",
        "",
        f"上游可解析模板：{len(upstream)} 家。",
        "",
        "## AIGX 缺模板的家",
        "",
    ]
    lines += [f"- `{k}`" for k in drift["missing_providers"]] or ["（无）"]
    lines += ["", "## 字段级差异", ""]
    if drift["field_diffs"]:
        lines.append("| 家 | AIGX 缺字段 | AIGX 多字段 |")
        lines.append("| --- | --- | --- |")
        for kind, diff in drift["field_diffs"].items():
            lines.append(
                f"| {kind} | {', '.join(diff['missing']) or '—'} | "
                f"{', '.join(diff['extra']) or '—'} |"
            )
    else:
        lines.append("（无）")
    lines += [
        "",
        "## AIGX 有声明但上游未解析到的家",
        "",
        "这些家在上游没有 `providers/<家>/credentials.rs`（或解析失败）——",
        "原样保留，请人工核对是否为 AIGX 独有能力。",
        "",
    ]
    lines += [f"- `{k}`" for k in drift["extra_providers"]] or ["（无）"]
    lines += [
        "",
        "## 处理方式",
        "",
        "运行 `--write` 让生成区与上游对齐（保留上游解析不到的家）；",
        "通用键映射与转发/续期链路的字段消费在 `account_session` 各模块，",
        "不属于本工具重写范围。",
        "",
    ]
    # eol=lf（.gitattributes）：Windows 上 write_text 默认把 \n 转 \r\n，
    # 显式 newline="\n" 避免报告文件 eol 抖动。
    path.write_text("\n".join(lines), encoding="utf-8", newline="\n")


def main() -> int:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser(description="Agent2API → AIGX 凭证字段模板同步巡检")
    ap.add_argument("--upstream", required=True, help="Agent2API 本地克隆目录")
    ap.add_argument("--check", action="store_true", help="只检测漂移，有差异 exit 1")
    ap.add_argument("--write", action="store_true", help="重写 registry.rs 生成区")
    ap.add_argument(
        "--report",
        action="store_true",
        help="生成 docs/upstream-credential-fields-diff.md",
    )
    ap.add_argument(
        "--dump", action="store_true", help="打印解析到的上游字段模板明细后退出"
    )
    args = ap.parse_args()

    upstream_dir = Path(args.upstream)
    if not upstream_dir.is_dir():
        raise SystemExit(f"上游目录不存在：{upstream_dir}")

    upstream = parse_upstream(upstream_dir)

    if args.dump:
        for kind in sorted(upstream):
            parsed = upstream[kind]
            print(f"=== {kind}（{','.join(sorted(parsed.sources))}）===")
            for spec in sorted(parsed.fields.values(), key=lambda f: f.name):
                mark = "R" if spec.required else " "
                print(f"  {mark} {spec.name}  {spec.hint[:50]}")
        return 0
    text = REGISTRY_RS.read_text(encoding="utf-8")
    existing = parse_aigx_registry(text)

    print(
        f"上游字段模板 {len(upstream)} 家（{sum(len(p.fields) for p in upstream.values())} 个字段）；"
        f"AIGX 声明 {len(existing)} 家"
    )

    drift = compute_drift(upstream, existing)
    for kind in drift["missing_providers"]:
        up = upstream[kind]
        print(f"[模板缺] {kind}（{len(up.fields)} 字段，来源 {','.join(sorted(up.sources))}）")
    for kind, diff in drift["field_diffs"].items():
        if diff["missing"]:
            print(f"[字段缺] {kind}: {', '.join(diff['missing'])}")
        if diff["extra"]:
            print(f"[字段多] {kind}: {', '.join(diff['extra'])}")
    for kind in drift["extra_providers"]:
        print(f"[保留] {kind}（上游未解析到 credentials.rs，原样保留）")

    if args.write:
        m = re.search(
            re.escape(BEGIN_MARK) + r"(.*?)" + re.escape(END_MARK), text, re.S
        )
        if not m:
            raise SystemExit(
                f"{REGISTRY_RS} 里找不到 {BEGIN_MARK} / {END_MARK} 标记，--write 中止"
            )
        block, kept = render_block(upstream, existing)
        new_text = text[: m.start(1)] + "\n" + block + "\n" + text[m.end(1):]
        REGISTRY_RS.write_text(new_text, encoding="utf-8", newline="\n")
        print(
            f"\n已重写生成区（{len(upstream)} 家按上游对拍"
            + (f"，保留 {len(kept)} 家：{', '.join(kept)}" if kept else "")
            + f"）→ {REGISTRY_RS}"
        )

    if args.report:
        report_path = REPO_ROOT / "docs/upstream-credential-fields-diff.md"
        write_report(report_path, drift, upstream)
        print(f"\n已生成字段模板漂移报告 → {report_path}")

    dirty = bool(drift["missing_providers"] or drift["field_diffs"])
    if args.check and dirty:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
