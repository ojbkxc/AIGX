#!/usr/bin/env bash
# tools/preflight.sh — push 前的本地预检。
#
# 背景：本仓库由多个 AI 会话并行编辑，近 200 个 commit 里 17 次
# fix(build) / fix(clippy)，绝大多数本可在 push 前拦住。本机没装
# 完整 Rust 工具链（无法 cargo build/check），但 rustfmt 通常可用；
# 前端有完整 Node 工具链，能跑 tsc + vite build。
#
# 用法：
#   bash tools/preflight.sh        # 全部预检
#   bash tools/preflight.sh --fast # 跳过 vite build（最快）
#
# 退出码：0 = 全过，非 0 = 至少一项失败（按输出逐项修）。

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

FAST=0
[[ "${1:-}" == "--fast" ]] && FAST=1

FAILED=()
note() { printf "\033[1;34m==>\033[0m %s\n" "$*"; }
ok()   { printf "\033[1;32mOK\033[0m   %s\n" "$*"; }
bad()  { printf "\033[1;31mFAIL\033[0m %s\n" "$*"; FAILED+=("$*"); }

# ── 1. 工作区卫生：只 add 自己改的文件（AGENTS.md 硬约束 5）──
note "工作区状态"
git status --short | head -40
echo

# ── 2. Rust fmt ──
note "cargo fmt --all -- --check"
if command -v cargo >/dev/null 2>&1; then
  if cargo fmt --all -- --check >/tmp/_preflight_fmt.log 2>&1; then
    ok "fmt"
  else
    bad "fmt"
    grep -E "^Diff in" /tmp/_preflight_fmt.log | sort -u | head -10
  fi
else
  echo "  (cargo 不在 PATH，跳过 —— CI 会兜底)"
fi
echo

# ── 3. 前端 tsc ──
note "frontend tsc --noEmit"
if [[ -d frontend/node_modules ]]; then
  if (cd frontend && ./node_modules/.bin/tsc --noEmit >/tmp/_preflight_tsc.log 2>&1); then
    ok "tsc"
  else
    bad "tsc"
    tail -20 /tmp/_preflight_tsc.log
  fi
else
  echo "  (frontend/node_modules 不存在，先 npm install)"
  bad "tsc (no node_modules)"
fi
echo

# ── 4. 前端 build（--fast 跳过）──
if (( FAST == 0 )); then
  note "frontend vite build"
  if [[ -d frontend/node_modules ]]; then
    if (cd frontend && npm run build >/tmp/_preflight_vite.log 2>&1); then
      ok "vite build"
    else
      bad "vite build"
      tail -20 /tmp/_preflight_vite.log
    fi
  fi
  echo
fi

# ── 5. 凭证/PAT 泄露自检 ──
note "暂存区是否含可疑字符串"
if git diff --cached -U0 | grep -iE "ghp_[A-Za-z0-9]{20,}|password.*=.*['\"]|secret.*=.*['\"]|Bearer\s+[A-Za-z0-9._-]{20,}" >/tmp/_preflight_leak.log 2>&1; then
  bad "暂存区疑似含敏感串"
  head -10 /tmp/_preflight_leak.log
else
  ok "无明显敏感串"
fi
echo

# ── 总结 ──
if (( ${#FAILED[@]} == 0 )); then
  printf "\033[1;32m✓ preflight 全过，可以 push\033[0m\n"
  exit 0
else
  printf "\033[1;31m✗ preflight 失败 %d 项\033[0m\n" "${#FAILED[@]}"
  for f in "${FAILED[@]}"; do
    printf "  - %s\n" "$f"
  done
  exit 1
fi
