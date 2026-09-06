#!/usr/bin/env python3
"""new-api → AIGX 配置同步助手

从天翼云 new-api（MySQL）导出渠道/分组/定价，写入 AIGX 网关。
凭据走环境变量，禁止硬编码（本文件入 git）。

用法：
  AIGX_TARGET=http://host:9527 AIGX_ADMIN=admin AIGX_PASS=xxx \
  NEWAPI_HOST=104.223.65.202 NEWAPI_SSH_PORT=10122 \
  NEWAPI_SSH_USER=root NEWAPI_SSH_PASS=xxx \
  python scripts/newapi_sync.py

已执行记录（2026-09-06）：渠道 23 条、分组 vip=0.9/svip=0.8、
ModelPrice 73 条、ModelRatio 273 条（输入 $0.002/1K 基准 × 倍率，
output 再乘 CompletionRatio）。
"""
import csv
import io
import json
import os
import sys

import paramiko
import requests

NEWAPI_SSH_HOST = os.environ.get("NEWAPI_HOST", "104.223.65.202")
NEWAPI_SSH_PORT = int(os.environ.get("NEWAPI_SSH_PORT", "10122"))
NEWAPI_SSH_USER = os.environ.get("NEWAPI_SSH_USER", "root")
NEWAPI_SSH_PASS = os.environ["NEWAPI_SSH_PASS"]
NEWAPI_DB_USER = os.environ.get("NEWAPI_DB_USER", "tenant_agent")
NEWAPI_DB_NAME = os.environ.get("NEWAPI_DB_NAME", "tenant_agent")

AIGX = os.environ.get("AIGX_TARGET", "http://104.223.65.202:9527")
AIGX_ADMIN = os.environ.get("AIGX_ADMIN", "admin")
AIGX_PASS = os.environ["AIGX_PASS"]

# new-api channel type → AIGX channel_type
TYPE_MAP = {1: "openai_compatible", 14: "anthropic", 24: "gemini",
            43: "openai_compatible", 39: "cloudflare"}
# type=41 是 Vertex AI（service account 凭据），AIGX 暂不支持 → 跳过


def main() -> int:
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(NEWAPI_SSH_HOST, port=NEWAPI_SSH_PORT,
                   username=NEWAPI_SSH_USER, password=NEWAPI_SSH_PASS, timeout=20)

    def run(cmd: str, timeout: int = 60) -> str:
        _, stdout, _ = client.exec_command(cmd, timeout=timeout)
        return stdout.read().decode(errors="replace")

    db_pass = run(f"grep DB_PASSWORD /www/wwwroot/newapi/.env").strip().split("=", 1)[1]

    # ── 渠道 ──
    sql = ("SELECT id, name, type, status, base_url, models, `key`, priority, weight, `group` "
           "FROM channels")
    out = run(f"mysql -u {NEWAPI_DB_USER} -p'{db_pass}' {NEWAPI_DB_NAME} --batch -e '{sql}' 2>/dev/null")
    lines = out.splitlines()
    rows = list(csv.reader(io.StringIO("\n".join(lines[1:])), delimiter="\t"))

    # ── 倍率 ──
    ratios: dict[str, dict] = {}
    for key in ("ModelRatio", "ModelPrice", "CompletionRatio", "GroupRatio"):
        raw = run(f"mysql -u {NEWAPI_DB_USER} -p'{db_pass}' {NEWAPI_DB_NAME} --batch -N -e "
                  f"\"SELECT REPLACE(REPLACE(`value`, CHAR(10), ''), CHAR(13), '') "
                  f"FROM options WHERE `key`='{key}'\" 2>/dev/null").strip()
        try:
            ratios[key] = json.loads(raw)
        except json.JSONDecodeError:
            ratios[key] = {}

    client.close()

    # ── 写入 AIGX ──
    s = requests.Session()
    r = s.post(f"{AIGX}/api/auth/login",
               json={"email": AIGX_ADMIN, "password": AIGX_PASS}, timeout=15)
    s.headers["Authorization"] = f"Bearer {r.json()['data']['token']}"

    r = s.get(f"{AIGX}/api/channels", timeout=15)
    existing = {c["name"] for c in r.json()["data"]}

    added = skipped = 0
    for row in rows:
        if len(row) < 10:
            continue
        (cid, name, ctype, status, base_url, models, key, priority, weight, group) = row
        aigx_type = TYPE_MAP.get(int(ctype))
        if aigx_type is None:
            skipped += 1
            continue
        if name in existing:
            continue
        r = s.post(f"{AIGX}/api/channels", json={
            "name": name, "channel_type": aigx_type,
            "base_url": base_url or "", "api_key": key,
            "models": [m.strip() for m in models.split(",") if m.strip()] if models else [],
            "priority": int(priority or 0), "weight": max(int(weight or 0), 1),
            "status": "enabled" if status == "1" else "disabled",
        }, timeout=30)
        added += 1 if r.status_code == 200 else 0

    # 分组倍率
    for gname, gratio in ratios.get("GroupRatio", {}).items():
        if isinstance(gratio, (int, float)):
            s.put(f"{AIGX}/api/groups/{gname}", json={"name": gname, "ratio": float(gratio)}, timeout=15)

    # 定价：ModelPrice（按次）+ ModelRatio（token，$0.002/1K 基准）
    for m, p in ratios.get("ModelPrice", {}).items():
        s.post(f"{AIGX}/api/prices", json={
            "model_name": str(m), "price_type": "count", "input_price": float(p)}, timeout=15)
    base_price = 0.002
    for m, ratio in ratios.get("ModelRatio", {}).items():
        comp = ratios.get("CompletionRatio", {}).get(m, 1.0)
        s.post(f"{AIGX}/api/prices", json={
            "model_name": str(m), "price_type": "token",
            "input_price": round(float(ratio) * base_price, 6),
            "output_price": round(float(ratio) * base_price * float(comp), 6)}, timeout=15)

    print(f"渠道新增 {added}（跳过 {skipped}），分组 {len(ratios.get('GroupRatio', {}))}，"
          f"定价 ModelPrice {len(ratios.get('ModelPrice', {}))} + ModelRatio {len(ratios.get('ModelRatio', {}))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
