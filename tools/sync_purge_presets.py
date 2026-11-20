#!/usr/bin/env python3
"""把 jailbreaks 的 prompt 集合编译成破甲控制台的内置预设目录。

为什么需要这个脚本：预设正文是 53 个 markdown 文件、约 735 KB，手写 Rust
常量既不可能维护，也容易和源目录漂移。脚本把「源目录 → Rust 常量」这一步
变成幂等、可校验的机械操作：

  - 正文复制到 assets/purge-presets/<vendor-slug>/<id>.md
  - src/purge/preset_catalog.rs 用 include_str! 引用这些文件（编译期内联，
    运行时零磁盘依赖）
  - 输出确定性：排序固定、换行统一 LF、不含时间戳，重复运行 diff 为空

用法：
  python tools/sync_purge_presets.py            # 生成/覆盖
  python tools/sync_purge_presets.py --check    # 只校验，不一致 exit 1
  python tools/sync_purge_presets.py --dry-run  # 只打印统计

源目录默认 ../jailbreaks（与本仓库同级），可用 --source 覆盖。
"""

from __future__ import annotations

import argparse
import hashlib
import re
import shutil
import sys
from dataclasses import dataclass
from pathlib import Path

# 仓库根目录（本文件在 <root>/tools/ 下）
ROOT = Path(__file__).resolve().parent.parent

DEFAULT_SOURCE = ROOT.parent / "jailbreaks"
ASSET_DIR = ROOT / "assets" / "purge-presets"
CATALOG_PATH = ROOT / "src" / "purge" / "preset_catalog.rs"

# 标签抽取词表：小写子串匹配，命中即打标，最多 MAX_TAGS 个。
# 只放「形态」类词（文件是什么格式、面向什么场景），不放内容判断词——
# 标签是用来过滤检索的，不是用来评价内容好坏的。
TAG_KEYWORDS: list[tuple[str, str]] = [
    ("jailbreak", "jailbreak"),
    ("operating policy", "policy"),
    ("system prompt", "system-prompt"),
    ("persona", "persona"),
    ("agents.md", "agents-md"),
    ("modelfile", "modelfile"),
    ("ollama", "ollama"),
    ("workspace", "workspace"),
    ("override", "override"),
    ("developer mode", "developer-mode"),
]

MAX_TAGS = 6


@dataclass(frozen=True)
class Preset:
    """一条预设的编译期形态。"""

    id: str
    vendor: str
    model: str
    title: str
    tags: tuple[str, ...]
    body: str

    @property
    def vendor_slug(self) -> str:
        return slugify(self.vendor) or "misc"

    @property
    def asset_rel(self) -> str:
        return f"{self.vendor_slug}/{self.id}.md"

    @property
    def bytes(self) -> int:
        return len(self.body.encode("utf-8"))

    @property
    def sha256(self) -> str:
        return hashlib.sha256(self.body.encode("utf-8")).hexdigest()


def slugify(text: str) -> str:
    """ascii 化：小写、非 [a-z0-9] 折叠成单 hyphen、去首尾 hyphen。

    非 ascii 字符（中文厂商名等）一律折叠掉——id 要能直接当目录名和
    URL 路径段用，不能出现需要转义的字符。
    """
    lowered = text.lower()
    return re.sub(r"-+", "-", re.sub(r"[^a-z0-9]+", "-", lowered)).strip("-")


def read_body(path: Path) -> str:
    """读取正文并统一换行。

    源文件全部是 CRLF；统一成 LF 让 sha256 和 git diff 都稳定，
    不受检出时 autocrlf 设置影响。
    """
    raw = path.read_bytes()
    text = raw.decode("utf-8")
    if text.startswith("\ufeff"):
        text = text[1:]
    return text.replace("\r\n", "\n").replace("\r", "\n")


def extract_tags(body: str) -> tuple[str, ...]:
    """按词表命中抽标签，保持词表顺序（确定性）。"""
    head = body[:4096].lower()
    tags = [tag for needle, tag in TAG_KEYWORDS if needle in head]
    return tuple(tags[:MAX_TAGS])


def collect(source: Path) -> list[Preset]:
    """扫描源目录，返回按 (vendor, model) 排序的预设列表。

    id 冲突时追加 -2 / -3 后缀。冲突是真实存在的（不同目录同名文件），
    静默覆盖会丢数据，所以必须显式消歧。
    """
    if not source.is_dir():
        raise SystemExit(f"源目录不存在：{source}")

    seen: dict[str, int] = {}
    presets: list[Preset] = []

    for path in sorted(source.rglob("*.md")):
        if path.name.lower().startswith("readme"):
            continue
        if ".git" in path.parts:
            continue
        rel = path.relative_to(source)
        # 源目录是一级厂商目录；散落在根目录的文件归入 misc。
        vendor = rel.parts[0] if len(rel.parts) > 1 else "misc"
        model = path.stem
        base = slugify(f"{vendor} {model}") or "preset"

        count = seen.get(base, 0) + 1
        seen[base] = count
        preset_id = base if count == 1 else f"{base}-{count}"

        body = read_body(path)
        presets.append(
            Preset(
                id=preset_id,
                vendor=vendor,
                model=model,
                title=model,
                tags=extract_tags(body),
                body=body,
            )
        )

    presets.sort(key=lambda p: (p.vendor.lower(), p.model.lower(), p.id))
    return presets


def rust_str(value: str) -> str:
    """把任意文本转成 Rust 字符串字面量（含引号）。

    只用 debug 转义的最小必要集：反斜杠、双引号、控制字符。
    """
    escaped = (
        value.replace("\\", "\\\\")
        .replace('"', '\\"')
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t")
    )
    return f'"{escaped}"'


def render_catalog(presets: list[Preset], source: Path) -> str:
    """生成 preset_catalog.rs 全文。"""
    try:
        source_display = source.resolve().relative_to(ROOT.parent).as_posix()
    except ValueError:
        source_display = source.as_posix()

    lines = [
        "//! 破甲预设库内置目录（**自动生成，请勿手改**）。",
        "//!",
        f"//! 来源：`{source_display}`（第三方公开的 prompt 研究集合，按厂商目录组织）。",
        "//! 重新生成：`python tools/sync_purge_presets.py`",
        "//! 一致性校验：`python tools/sync_purge_presets.py --check`",
        "//!",
        "//! 正文通过 `include_str!` 在编译期内联，运行时零磁盘依赖；条目按",
        "//! (vendor, model) 排序且 id 唯一，因此生成的数组可以直接二分/线性查找。",
        "",
        "/// 一条内置预设（编译期常量，字段全为 `'static`）。",
        "#[derive(Debug, Clone, Copy)]",
        "pub struct BuiltinPreset {",
        "    /// 稳定 ascii 标识（`<vendor>-<model>`，非字母数字折叠为 `-`）",
        "    pub id: &'static str,",
        "    /// 来源一级目录（厂商）",
        "    pub vendor: &'static str,",
        "    /// 来源文件名（不含扩展名）",
        "    pub model: &'static str,",
        "    /// 展示标题",
        "    pub title: &'static str,",
        "    /// 检索标签",
        "    pub tags: &'static [&'static str],",
        "    /// 正文字节数（UTF-8）",
        "    pub bytes: usize,",
        "    /// 正文 sha256（小写十六进制，用于校验源目录未漂移）",
        "    pub sha256: &'static str,",
        "    /// 正文",
        "    pub body: &'static str,",
        "}",
        "",
        "/// 全部内置预设，按 (vendor, model) 排序，id 唯一。",
        "#[rustfmt::skip]",
        f"pub const BUILTIN_PRESETS: &[BuiltinPreset] = &[",
    ]

    for preset in presets:
        tags = ", ".join(rust_str(tag) for tag in preset.tags)
        lines.extend(
            [
                "    BuiltinPreset {",
                f"        id: {rust_str(preset.id)},",
                f"        vendor: {rust_str(preset.vendor)},",
                f"        model: {rust_str(preset.model)},",
                f"        title: {rust_str(preset.title)},",
                f"        tags: &[{tags}],",
                f"        bytes: {preset.bytes},",
                f"        sha256: {rust_str(preset.sha256)},",
                f'        body: include_str!("../../assets/purge-presets/{preset.asset_rel}"),',
                "    },",
            ]
        )

    lines.append("];")
    lines.append("")
    return "\n".join(lines)


def write_outputs(presets: list[Preset], catalog: str, dry_run: bool) -> int:
    """写出资产与目录文件，返回写入/差异计数。"""
    changed = 0

    if not dry_run and ASSET_DIR.exists():
        # 全量重建：删掉不再来自源目录的残留，否则 --check 会一直报不一致。
        shutil.rmtree(ASSET_DIR)
    if not dry_run:
        ASSET_DIR.mkdir(parents=True, exist_ok=True)

    for preset in presets:
        target = ASSET_DIR / preset.asset_rel
        payload = preset.body.encode("utf-8")
        if not target.exists() or target.read_bytes() != payload:
            changed += 1
            if not dry_run:
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(payload)

    current = CATALOG_PATH.read_text(encoding="utf-8") if CATALOG_PATH.exists() else ""
    if current != catalog:
        changed += 1
        if not dry_run:
            CATALOG_PATH.parent.mkdir(parents=True, exist_ok=True)
            CATALOG_PATH.write_text(catalog, encoding="utf-8", newline="\n")

    return changed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--check", action="store_true", help="只校验，不一致时 exit 1")
    parser.add_argument("--dry-run", action="store_true", help="只打印统计，不写盘")
    args = parser.parse_args()

    presets = collect(args.source)
    catalog = render_catalog(presets, args.source)
    total_bytes = sum(p.bytes for p in presets)

    ids = [p.id for p in presets]
    if len(ids) != len(set(ids)):
        dupes = sorted({i for i in ids if ids.count(i) > 1})
        raise SystemExit(f"id 冲突未消歧：{dupes}")

    if args.check:
        # 校验模式必须和真实写出走同一套比对逻辑，否则「校验通过」没有意义。
        staged = render_catalog(presets, args.source)
        current = CATALOG_PATH.read_text(encoding="utf-8") if CATALOG_PATH.exists() else ""
        problems: list[str] = []
        if current != staged:
            problems.append("src/purge/preset_catalog.rs 与源目录不一致")
        for preset in presets:
            target = ASSET_DIR / preset.asset_rel
            if not target.exists():
                problems.append(f"缺少资产：{preset.asset_rel}")
            elif target.read_bytes() != preset.body.encode("utf-8"):
                problems.append(f"资产内容不一致：{preset.asset_rel}")
        expected = {p.asset_rel for p in presets}
        if ASSET_DIR.exists():
            for stray in ASSET_DIR.rglob("*.md"):
                if stray.relative_to(ASSET_DIR).as_posix() not in expected:
                    problems.append(f"多余资产：{stray.relative_to(ASSET_DIR).as_posix()}")
        if problems:
            print(f"不一致（{len(problems)} 处）：", file=sys.stderr)
            for item in problems[:20]:
                print(f"  - {item}", file=sys.stderr)
            return 1
        print(f"一致：{len(presets)} 条，{total_bytes} 字节")
        return 0

    changed = write_outputs(presets, catalog, args.dry_run)
    mode = "dry-run" if args.dry_run else "写入"
    print(f"{mode}：{len(presets)} 条，{total_bytes} 字节，变更 {changed} 处")
    if args.dry_run:
        vendors: dict[str, int] = {}
        for preset in presets:
            vendors[preset.vendor] = vendors.get(preset.vendor, 0) + 1
        for vendor, count in sorted(vendors.items()):
            print(f"  {vendor}: {count}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
