#!/usr/bin/env python3
"""AIGX 版本一致性闸门：六处版本号必须与 VERSION 文件一致。

版本号必须六处一致：VERSION / Cargo.toml / Cargo.lock(aigx 包) /
frontend/package.json / frontend/package-lock.json / static/version.json
—— 否则线上「当前版本」显示与前端、更新判定、构建产物全部错位。

由 build.yml（create-release job）与 release.yml（发布闸门）共用同一
实现，避免两份校验逻辑漂移。在仓库根目录运行：`python3 verify-version.py`。
"""
import json
import re
import sys

version = open("VERSION", encoding="utf-8").read().strip()
errors = []


def check(name, actual):
    if actual != version:
        errors.append(f"{name} = {actual!r} != VERSION = {version!r}")


m = re.search(r'^version\s*=\s*"([^"]+)"', open("Cargo.toml", encoding="utf-8").read(), re.M)
check("Cargo.toml", m.group(1) if m else "MISSING")

lock = open("Cargo.lock", encoding="utf-8").read()
m = re.search(r'\[\[package\]\]\nname = "aigx"\nversion = "([^"]+)"', lock)
check("Cargo.lock(aigx)", m.group(1) if m else "MISSING")

check("package.json", json.load(open("frontend/package.json", encoding="utf-8"))["version"])
_pl = json.load(open("frontend/package-lock.json", encoding="utf-8"))
check("package-lock.json", _pl["version"])
check('package-lock.json packages[""]', _pl["packages"][""]["version"])
check("static/version.json", json.load(open("static/version.json", encoding="utf-8"))["version"])

print(f"VERSION={version}")
if errors:
    for e in errors:
        print(f"::error::{e}")
    print(
        "::error::版本号不一致，请统一六处（VERSION/Cargo.toml/Cargo.lock/"
        "package.json/package-lock.json/static/version.json）"
    )
    sys.exit(1)
print("六处版本号一致")
