#!/usr/bin/env python3
"""AIGX 生产「脚本插件批量验证」工具（入库版）。

用途
----
把一份 Magpie 目录（`GET /api/plugins/scripts/catalog` 的 JSON 响应）按批次
真实地「安装 → 启用 → 等宿主稳定 → 采集 → 卸载」，逐包记录加载/路由结果，
用于验证脚本宿主在生产上对目录全量插件（magpie / npm / github）的兼容性。

凭据来源（全部走环境变量，禁止硬编码）
--------------------------------------
    AIGX_SSH_PASS    服务器 SSH 口令（必需）
    AIGX_ADMIN_PASS  网关管理员登录口令（必需）
    AIGX_EMAIL       管理员邮箱（默认 admin@gmail.com）
    AIGX_HOST        SSH 主机（默认 104.223.65.202，可用 --host 覆盖）
    AIGX_SSH_USER    SSH 用户（默认 root）
    AIGX_SSH_PORT    SSH 端口（默认 22）
    AIGX_TOKEN_FILE  登录 token 缓存路径（默认系统临时目录 .aigx_verify_token）

依赖
----
纯标准库 + `paramiko`（SSH）。安装：`pip install paramiko`。
所有管理 API 都经 SSH 在服务器本机 `curl http://127.0.0.1:9527` 调用，
因此本机无需直连生产端口。

批次流程（每批）
----------------
    install(逐包，trusted=true) → enable(逐包) → 轮询宿主稳定(≤ --host-timeout)
    → 采集(id/enabled/providerRouted/providers/error/host.*) → uninstall(逐包)
单包失败不中断整批；失败分类：
    install_failed / enable_failed / load_error / not_routed / host_crashed / timeout
`not_routed` **只信**后端 `providerRouted`：包确实声明了 provider 却未被宿主
接管才记失败；`providers=[]` 的包（目录里很常见）判 ok。

安全闸门
--------
一批内 `host.crashes` 增长 ≥2，或 `host.running=false` 且 `host.lastError` 非空
→ 立即停批、uninstall 本批、打印宿主现场、退出码 2。

输出
----
    --out  每包一行 JSONL（断点/结果；失败项也落盘，便于重试）
    --csv  CSV 汇总（默认与 --out 同名 .csv）
    --resume 只跳过 JSONL 中 `ok=true` 的条目（失败项会重跑）

示例
----
    # 目录来源二选一：
    #   a) 省略 --catalog：自动经管理 API 拉取并写入 --catalog-out（需凭据），
    #   b) 传 --catalog <path>：直接读本地目录 JSON（无需联网）。
    # 手工保存目录（可选，便于离线/--plan-only 不联网；token 由登录获得，
    # 登录口令只从环境变量 $AIGX_ADMIN_PASS 读取，不写进命令/脚本）：
    #   curl -s -H "Authorization: Bearer $TOKEN" \
    #       $AIGX_BASE_URL/api/plugins/scripts/catalog > magpie_catalog.json

    # 看会选中哪些条目：有 --catalog 时纯离线；无 --catalog 且凭据可用时先只读拉目录
    python tools/verify_magpie_plugins.py --source magpie --limit 2 --plan-only

    # 小批真跑（装完自动卸载；--catalog 省略 = 自动拉目录）
    AIGX_SSH_PASS=... AIGX_ADMIN_PASS=... \
    python tools/verify_magpie_plugins.py --source magpie --limit 2 --batch 2

    # 正式全量：magpie → npm → github，批次 8，断点续跑
    python tools/verify_magpie_plugins.py --source magpie --batch 8 --resume
    python tools/verify_magpie_plugins.py --source npm    --batch 8 --resume
    python tools/verify_magpie_plugins.py --source github --batch 8 --resume

    # 只重跑上一轮失败项（读 --out 里 ok=false 的 selector；--include 优先）
    python tools/verify_magpie_plugins.py --retry-failed --batch 8
    python tools/verify_magpie_plugins.py --retry-failed --plan-only   # 只数待重跑项，不联网

    # 只重试指定条目
    python tools/verify_magpie_plugins.py --include "magpie:@magpie-community/foo" --batch 1

    # 验证前先刷新生产目录（异步刷新，等待后重新拉取；需凭据）
    python tools/verify_magpie_plugins.py --refresh --source github --batch 8 --resume
"""

import argparse
import base64
import csv
import hashlib
import json
import os
import re
import shlex
import sys
import tempfile
import time
from datetime import datetime

try:
    import paramiko
except Exception as _exc:  # noqa: BLE001
    paramiko = None
    _PARAMIKO_ERR = _exc

DEFAULT_HOST = "104.223.65.202"

# 运行期配置（main 内填充）。所有 API 调用经 SSH 在服务器本机 curl。
CFG = {
    "host": DEFAULT_HOST,
    "port": 22,
    "user": "root",
    "base": "http://127.0.0.1:9527",
    "token": None,
}

_MSYS_DRIVE = re.compile(r"^/([A-Za-z])/")


# ── 参数 / 路径 ──────────────────────────────────────────────────────
def normalize_path(path):
    """Windows 下把 MSYS 风格 `/d/tmp/x` 规范成 `D:/tmp/x`（仅对盘符形态）。"""
    if not path or os.name != "nt":
        return path
    match = _MSYS_DRIVE.match(path)
    if match:
        return f"{match.group(1).upper()}:/" + path[3:]
    return path


def split_csv(value):
    return {item.strip() for item in (value or "").split(",") if item.strip()}


def has_creds():
    return bool(os.environ.get("AIGX_SSH_PASS") and os.environ.get("AIGX_ADMIN_PASS"))


# ── SSH + ALTCHA 登录 ────────────────────────────────────────────────
def connect():
    if paramiko is None:
        sys.exit(f"缺少依赖 paramiko：{_PARAMIKO_ERR}（pip install paramiko）")
    ssh_pass = os.environ.get("AIGX_SSH_PASS")
    if not ssh_pass:
        sys.exit("missing env AIGX_SSH_PASS")
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(
        CFG["host"], CFG["port"], CFG["user"], ssh_pass,
        timeout=20, banner_timeout=20, auth_timeout=20,
    )
    return client


def run(cli, cmd, timeout):
    _in, out, err = cli.exec_command(cmd, timeout=timeout)
    rc = out.channel.recv_exit_status()
    return rc, out.read().decode("utf-8", "replace"), err.read().decode("utf-8", "replace")


def curl_json(cli, method, path, body=None, timeout=180):
    parts = ["curl", "-sS", "--max-time", str(timeout), "-X", method, CFG["base"] + path]
    if CFG["token"]:
        parts += ["-H", f"Authorization: Bearer {CFG['token']}"]
    if body is not None:
        parts += ["-H", "Content-Type: application/json",
                  "--data-raw", json.dumps(body, ensure_ascii=False)]
    command = " ".join(shlex.quote(p) for p in parts)
    rc, out, err = run(cli, command, timeout=timeout + 20)
    if rc != 0:
        raise RuntimeError(f"curl rc={rc}: {err.strip()}")
    try:
        return json.loads(out)
    except Exception:  # noqa: BLE001
        raise RuntimeError(f"non-json response: {out[:500]}{err[:200]}")


def solve_altcha(challenge):
    salt, target, max_number = challenge["salt"], challenge["challenge"], int(challenge["maxnumber"])
    for number in range(max_number + 1):
        if hashlib.sha256(f"{salt}{number}".encode()).hexdigest() == target:
            payload = {
                "algorithm": "SHA-256",
                "challenge": target,
                "number": number,
                "salt": salt,
                "signature": challenge["signature"],
            }
            return base64.b64encode(json.dumps(payload).encode()).decode()
    raise RuntimeError("altcha: no solution in maxnumber range")


def login(cli):
    admin_pass = os.environ.get("AIGX_ADMIN_PASS")
    if not admin_pass:
        sys.exit("missing env AIGX_ADMIN_PASS")
    email = os.environ.get("AIGX_EMAIL", "admin@gmail.com")
    challenge = curl_json(cli, "GET", "/api/auth/captcha")
    body = {"email": email, "password": admin_pass, "captcha": solve_altcha(challenge)}
    res = curl_json(cli, "POST", "/api/auth/login", body)
    if not res.get("success"):
        raise RuntimeError(f"login failed: {res}")
    data = res["data"]
    if data.get("require_2fa"):
        raise RuntimeError("admin account requires 2FA; not supported by this tool")
    token = data["token"]
    token_file = os.environ.get(
        "AIGX_TOKEN_FILE", os.path.join(tempfile.gettempdir(), ".aigx_verify_token")
    )
    try:
        with open(token_file, "w", encoding="utf-8") as f:
            f.write(token)
    except OSError:
        pass
    print(f"login ok email={data.get('email')} role={data.get('role')}")
    return token


def cached_token(cli):
    token_file = os.environ.get(
        "AIGX_TOKEN_FILE", os.path.join(tempfile.gettempdir(), ".aigx_verify_token")
    )
    try:
        with open(token_file, encoding="utf-8") as f:
            token = f.read().strip()
        CFG["token"] = token
        probe = curl_json(cli, "GET", "/api/plugins/scripts")
        if probe.get("success"):
            return token
    except Exception:  # noqa: BLE001
        pass
    return login(cli)


# ── API 封装 ─────────────────────────────────────────────────────────
def api(cli, method, path, body=None, timeout=180):
    try:
        return curl_json(cli, method, path, body, timeout=timeout)
    except Exception as ex:  # noqa: BLE001
        return {"success": False, "error": f"{type(ex).__name__}: {ex}"}


def get_state(cli):
    resp = api(cli, "GET", "/api/plugins/scripts")
    data = resp.get("data") or {}
    return {"raw": resp, "host": data.get("host") or {}, "packages": data.get("packages") or []}


# ── 目录 / 断点 ──────────────────────────────────────────────────────
def read_catalog_doc(path):
    """读本地目录 JSON；FileNotFoundError / JSONDecodeError 由调用方友好处理。"""
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def extract_entries(doc):
    if isinstance(doc, list):
        return doc
    if isinstance(doc, dict) and isinstance(doc.get("entries"), list):
        return doc["entries"]
    if isinstance(doc, dict) and isinstance((doc.get("data") or {}).get("entries"), list):
        return doc["data"]["entries"]
    raise SystemExit("无法识别目录 JSON 结构（既不是数组，也没有 entries）")


def filter_entries(entries, source, limit, include, exclude):
    out = []
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        if source != "all" and entry.get("source") != source:
            continue
        if entry.get("installable") is False:
            continue
        selector = (entry.get("selector") or "").strip()
        name = entry.get("name") or ""
        if not selector:
            continue
        if include and selector not in include and name not in include:
            continue
        if exclude and (selector in exclude or name in exclude):
            continue
        out.append(entry)
    if limit and limit > 0:
        out = out[:limit]
    return out


def load_entries(path, source, limit, include, exclude):
    return filter_entries(extract_entries(read_catalog_doc(path)), source, limit, include, exclude)


def fetch_catalog(cli):
    """经 SSH + 管理 API 拉取目录（只读；返回完整响应对象）。"""
    resp = api(cli, "GET", "/api/plugins/scripts/catalog", timeout=120)
    data = resp.get("data") or {}
    if not resp.get("success") or not isinstance(data.get("entries"), list):
        raise RuntimeError("拉取目录失败：" + str(resp.get("error") or resp)[:500])
    return resp


def write_catalog(path, resp):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(resp, f, ensure_ascii=False)


def load_failed(path):
    """--retry-failed：收集 JSONL 里 ok=false 的 selector 集合。"""
    failed = set()
    if not os.path.exists(path):
        return failed
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception:  # noqa: BLE001
                continue
            if rec.get("ok") is False and rec.get("selector"):
                failed.add(rec["selector"])
    return failed


def load_done(path):
    """--resume：只收集已成功（ok=true）的 selector。"""
    done = set()
    if not os.path.exists(path):
        return done
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except Exception:  # noqa: BLE001
                continue
            if rec.get("ok") and rec.get("selector"):
                done.add(rec["selector"])
    return done


def read_all_records(path):
    out = []
    if not os.path.exists(path):
        return out
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except Exception:  # noqa: BLE001
                continue
    return out


def write_record(path, rec):
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        f.flush()


def new_rec(entry, batch_idx):
    return {
        "ts": datetime.now().isoformat(timespec="seconds"),
        "batch": batch_idx,
        "name": entry.get("name"),
        "selector": entry.get("selector"),
        "source": entry.get("source"),
        "publisher": entry.get("publisher"),
        "version": entry.get("version"),
        "declared_providers_catalog": entry.get("providers") or [],
        "id": None,
        "install_ok": None,
        "install_error": None,
        "enable_ok": None,
        "enable_error": None,
        "present": None,
        "enabled": None,
        "providerRouted": None,
        "providers": [],
        "package_error": None,
        "host": None,
        "host_crashes_delta": None,
        "type": None,
        "ok": False,
        "removed_ok": None,
        "teardown_error": None,
    }


def classify_package(pkg, wait_timed_out, declared):
    """单个已启用包的结果分类（纯函数）。

    - pkg 为 None：timeout（等宿主超时）否则 load_error；
    - error → load_error；enabled=false → enable_failed；
    - 路由只信后端 `providerRouted`：包确实声明了 provider（declared 非空）
      却没被接管才记 not_routed；声明为空（providers=[]）判 ok。
    """
    if pkg is None:
        return ("timeout" if wait_timed_out else "load_error", False)
    if pkg.get("error"):
        return ("load_error", False)
    if not pkg.get("enabled"):
        return ("enable_failed", False)
    if declared and not pkg.get("providerRouted"):
        return ("not_routed", False)
    return ("ok", True)


# ── 单批处理 ─────────────────────────────────────────────────────────
def process_batch(cli, batch_idx, entries, args):
    base = get_state(cli)
    base_host = base["host"]
    base_packages = int(base_host.get("packages") or 0)
    base_crashes = int(base_host.get("crashes") or 0)

    records = []
    installed = []
    enabled = []

    # 1) 安装
    for entry in entries:
        rec = new_rec(entry, batch_idx)
        resp = api(cli, "POST", "/api/plugins/scripts/install",
                   {"selector": entry["selector"], "trusted": True},
                   timeout=args.install_timeout)
        if resp.get("success"):
            pkg = (resp.get("data") or {}).get("package") or {}
            rec["install_ok"] = True
            rec["id"] = pkg.get("id") or entry.get("name")
            rec["version"] = pkg.get("version") or rec["version"]
            installed.append((entry, rec))
        else:
            rec["install_ok"] = False
            rec["install_error"] = str(resp.get("error") or resp)[:2000]
            rec["type"] = "install_failed"
            records.append(rec)

    # 2) 启用（no-op if already enabled；响应带 host 可作稳定首样本）
    enable_host = None
    for entry, rec in installed:
        resp = api(cli, "POST", "/api/plugins/scripts/change",
                   {"id": rec["id"], "action": "enable", "enabled": True},
                   timeout=args.enable_timeout)
        if resp.get("success"):
            rec["enable_ok"] = True
            enabled.append((entry, rec))
            hint = (resp.get("data") or {}).get("host")
            if isinstance(hint, dict) and hint:
                enable_host = hint
        else:
            rec["enable_ok"] = False
            rec["enable_error"] = str(resp.get("error") or resp)[:2000]
            rec["type"] = "enable_failed"
            records.append(rec)

    # 3) 等宿主稳定 + 安全闸门
    expected = base_packages + len(enabled)
    trip = None
    deadline = time.monotonic() + args.host_timeout
    stable_hits = 0
    last_host = base_host
    consec_fail = 0
    if isinstance(enable_host, dict) and enable_host:
        last_host = enable_host
        if bool(enable_host.get("running")) and int(enable_host.get("packages") or 0) >= expected:
            stable_hits = 1
    while True:
        state = get_state(cli)
        host = state["host"]
        last_host = host
        if not state["raw"].get("success"):
            consec_fail += 1
            if consec_fail >= 3:
                raise RuntimeError("轮询宿主状态连续失败："
                                   + str(state["raw"].get("error") or state["raw"])[:500])
        else:
            consec_fail = 0
        running = bool(host.get("running"))
        crashes = int(host.get("crashes") or 0)
        delta = crashes - base_crashes
        last_error = host.get("lastError") or host.get("last_error")
        if delta >= 2:
            trip = ("host_crashed", f"host.crashes 本批增长 {delta}（≥2）", host)
            break
        if (not running) and last_error:
            trip = ("host_crashed", f"宿主停摆（running=false）且 lastError={last_error}", host)
            break
        if running and int(host.get("packages") or 0) >= expected:
            stable_hits += 1
            if stable_hits >= 2:
                break
        else:
            stable_hits = 0
        if time.monotonic() >= deadline:
            break
        time.sleep(args.poll)

    wait_timed_out = trip is None and not (
        bool(last_host.get("running"))
        and int(last_host.get("packages") or 0) >= expected
    )

    # 4) 采集（一次全量快照；管理 API 失败直接报错而非误判 timeout）
    snap = get_state(cli)
    if not snap["raw"].get("success"):
        raise RuntimeError("GET /api/plugins/scripts 失败，无法采集宿主状态："
                           + str(snap["raw"].get("error") or snap["raw"])[:500])
    host = snap["host"] or last_host
    by_id = {p.get("id"): p for p in snap["packages"]}
    final_crashes = int(host.get("crashes") or 0)
    for entry, rec in enabled:
        if trip is not None:
            rec["type"] = "host_crashed"
            rec["ok"] = False
        else:
            pkg = by_id.get(rec["id"])
            if pkg is not None:
                rec["present"] = True
                rec["enabled"] = bool(pkg.get("enabled"))
                rec["providerRouted"] = bool(pkg.get("providerRouted"))
                rec["providers"] = [pp.get("id") for pp in (pkg.get("providers") or [])]
                rec["package_error"] = pkg.get("error")
            else:
                rec["present"] = False
            rec["type"], rec["ok"] = classify_package(
                pkg, wait_timed_out, rec.get("providers") or []
            )
        rec["host"] = {
            "running": host.get("running"),
            "packages": host.get("packages"),
            "providers": host.get("providers"),
            "crashes": final_crashes,
            "lastError": host.get("lastError") or host.get("last_error"),
        }
        rec["host_crashes_delta"] = final_crashes - base_crashes
        records.append(rec)

    # 5) 清理：直接 uninstall（内部先停用再删目录，省一次宿主重建）
    for _entry, rec in installed:
        resp = api(cli, "POST", "/api/plugins/scripts/uninstall",
                   {"id": rec["id"]}, timeout=args.enable_timeout)
        rec["removed_ok"] = bool(resp.get("success"))
        if not resp.get("success"):
            rec["teardown_error"] = str(resp.get("error") or resp)[:1000]

    return records, trip


def write_csv(path, records):
    cols = [
        "ts", "batch", "name", "selector", "source", "publisher", "version", "id",
        "type", "ok", "install_ok", "enable_ok", "present", "enabled", "providerRouted",
        "declared_providers_catalog", "providers", "package_error", "host_crashes_delta",
        "removed_ok", "install_error", "enable_error", "teardown_error",
    ]
    with open(path, "w", newline="", encoding="utf-8-sig") as f:
        writer = csv.DictWriter(f, fieldnames=cols)
        writer.writeheader()
        for rec in records:
            row = {}
            for col in cols:
                value = rec.get(col)
                if isinstance(value, list):
                    value = ";".join(str(v) for v in value)
                row[col] = value
            writer.writerow(row)


def print_summary(records):
    total = len(records)
    install_ok = sum(1 for r in records if r.get("install_ok"))
    enable_ok = sum(1 for r in records if r.get("enable_ok"))
    loaded = sum(1 for r in records if r.get("present") and r.get("enabled") and not r.get("package_error"))
    routed = sum(1 for r in records if r.get("providerRouted"))
    ok = sum(1 for r in records if r.get("ok"))
    print("\n===== 汇总 =====")
    print(f"总数={total}  安装成功={install_ok}  启用成功={enable_ok}")
    print(f"加载成功={loaded}  已路由={routed}  完全通过(ok)={ok}")
    known = ["ok", "install_failed", "enable_failed", "load_error",
             "not_routed", "host_crashed", "timeout"]
    counts = {}
    for rec in records:
        counts[rec.get("type") or "unknown"] = counts.get(rec.get("type") or "unknown", 0) + 1
    for key in known:
        if counts.get(key):
            print(f"  {key}: {counts[key]}")
    for key in sorted(counts):
        if key not in known:
            print(f"  {key}: {counts[key]}")


def main():
    parser = argparse.ArgumentParser(
        description="AIGX 生产脚本插件批量验证（install→enable→采样→uninstall）"
    )
    parser.add_argument("--catalog", default=None,
                        help="本地目录 JSON；省略时自动经管理 API 拉取并写入 --catalog-out")
    parser.add_argument("--catalog-out", default="magpie_catalog.json",
                        help="自动拉取目录时的保存路径（默认 magpie_catalog.json）")
    parser.add_argument("--source", choices=["magpie", "npm", "github", "all"], default="all")
    parser.add_argument("--batch", type=int, default=8)
    parser.add_argument("--limit", type=int, default=0, help="0 = 不限制")
    parser.add_argument("--include", default="", help="逗号分隔 selector/name，只验证这些条目")
    parser.add_argument("--exclude", default="", help="逗号分隔 selector/name，跳过这些条目")
    parser.add_argument("--retry-failed", action="store_true",
                        help="只重跑 --out JSONL 里 ok=false 的条目（--include 优先）")
    parser.add_argument("--out", default="magpie_verify.jsonl", help="JSONL 断点/结果文件")
    parser.add_argument("--csv", default=None, help="CSV 报告（默认与 --out 同名 .csv）")
    parser.add_argument("--resume", action="store_true", help="只跳过 JSONL 中 ok=true 的条目")
    parser.add_argument("--refresh", action="store_true",
                        help="验证前先 POST /api/plugins/scripts/refresh 刷新目录并等待重拉（需凭据；忽略本地 --catalog）")
    parser.add_argument("--refresh-wait", type=float, default=10.0,
                        help="--refresh 触发后等待后台刷新完成的秒数（默认 10）")
    parser.add_argument("--host-timeout", type=float, default=120.0)
    parser.add_argument("--poll", type=float, default=3.0)
    parser.add_argument("--install-timeout", type=int, default=300)
    parser.add_argument("--enable-timeout", type=int, default=180)
    parser.add_argument("--plan-only", action="store_true",
                        help="只打印计划，绝不安装/启停；缺 --catalog 且凭据可用时会先只读拉取目录")
    parser.add_argument("--host", default=os.environ.get("AIGX_HOST", DEFAULT_HOST))
    parser.add_argument("--ssh-user", default=os.environ.get("AIGX_SSH_USER", "root"))
    parser.add_argument("--ssh-port", type=int, default=int(os.environ.get("AIGX_SSH_PORT", "22")))
    parser.add_argument("--base-url", default=os.environ.get("AIGX_BASE_URL", "http://127.0.0.1:9527"))
    args = parser.parse_args()

    # 运行期配置（连接前就绪：connect/curl 都读 CFG）
    CFG.update(host=args.host, port=args.ssh_port, user=args.ssh_user,
               base=args.base_url.rstrip("/"))

    # MSYS 路径规范化（Windows）
    if args.catalog:
        args.catalog = normalize_path(args.catalog)
    args.catalog_out = normalize_path(args.catalog_out)
    args.out = normalize_path(args.out)
    if args.csv:
        args.csv = normalize_path(args.csv)

    include = split_csv(args.include)
    exclude = split_csv(args.exclude)
    failed = load_failed(args.out) if args.retry_failed else set()
    # --include 优先于 --retry-failed；两者都空 = 全量。
    selection = include if include else (failed if args.retry_failed else set())

    # --retry-failed + --plan-only 且无本地目录：直接按 JSONL 统计，不联网。
    if args.retry_failed and args.plan_only and not args.catalog and not args.refresh:
        print(f"--retry-failed：JSONL={args.out} 待重跑(ok=false)={len(failed)}")
        for selector in sorted(failed)[:50]:
            print(f"  {selector}")
        if not failed:
            print("没有 ok=false 的条目。")
        return

    cli = None

    def resolve_catalog():
        nonlocal cli
        if args.refresh:
            if args.catalog:
                print(f"提示：--refresh 与 --catalog 同时给出，忽略本地目录 {args.catalog}，改为刷新后重新拉取。",
                      file=sys.stderr)
            if not has_creds():
                print("错误：--refresh 需要凭据（AIGX_SSH_PASS/AIGX_ADMIN_PASS）。", file=sys.stderr)
                sys.exit(2)
            cli = connect()
            CFG["token"] = cached_token(cli)
            resp = api(cli, "POST", "/api/plugins/scripts/refresh", {})
            if not resp.get("success"):
                raise RuntimeError("刷新目录失败：" + str(resp.get("error") or resp)[:500])
            print(f"已触发目录刷新，等待 {args.refresh_wait:g}s 后重新拉取…")
            time.sleep(args.refresh_wait)
            doc = fetch_catalog(cli)
            entries = (doc.get("data") or {}).get("entries") or []
            write_catalog(args.catalog_out, doc)
            print(f"已刷新目录并写入 {args.catalog_out}（{len(entries)} 条）")
            return doc
        if args.catalog:
            try:
                return read_catalog_doc(args.catalog)
            except FileNotFoundError:
                print(
                    f"错误：目录文件不存在：{args.catalog}\n"
                    "可省略 --catalog 让工具自动拉取（需凭据），或提供有效的本地目录 JSON。",
                    file=sys.stderr,
                )
                sys.exit(2)
            except json.JSONDecodeError as ex:
                print(f"错误：目录文件不是合法 JSON：{args.catalog}（{ex}）", file=sys.stderr)
                sys.exit(2)
        if not has_creds():
            print(
                "未提供 --catalog，且缺少凭据（AIGX_SSH_PASS/AIGX_ADMIN_PASS），无法自动拉取目录。\n"
                "请设置凭据后重试，或用 --catalog 指定本地目录 JSON。",
                file=sys.stderr,
            )
            sys.exit(2)
        cli = connect()
        CFG["token"] = cached_token(cli)
        resp = fetch_catalog(cli)
        write_catalog(args.catalog_out, resp)
        print(f"已自动拉取目录并写入 {args.catalog_out}")
        return resp

    doc = resolve_catalog()
    entries = filter_entries(extract_entries(doc), args.source, args.limit, selection, exclude)

    if args.plan_only:
        label = args.catalog or args.catalog_out
        suffix = "（--retry-failed）" if args.retry_failed and not include else ""
        print(f"catalog={label} source={args.source} 选中={len(entries)} "
              f"batch={args.batch}{suffix}")
        for entry in entries[:50]:
            print(f"  {entry.get('source'):12} {entry.get('selector')}")
        if len(entries) > 50:
            print(f"  ... 其余 {len(entries) - 50} 条")
        if not entries:
            print("没有匹配条目。")
        if cli is not None:
            try:
                cli.close()
            except Exception:  # noqa: BLE001
                pass
        return

    if args.resume:
        done = load_done(args.out)
        before = len(entries)
        entries = [e for e in entries if e["selector"] not in done]
        print(f"--resume：已成功跳过 {before - len(entries)}，剩余 {len(entries)}")
    if not entries:
        print("没有待处理条目。")
        if cli is not None:
            try:
                cli.close()
            except Exception:  # noqa: BLE001
                pass
        return

    csv_path = args.csv or (os.path.splitext(args.out)[0] + ".csv")
    batch_size = max(1, args.batch)

    if cli is None:
        cli = connect()
    tripped = None
    try:
        if not CFG["token"]:
            CFG["token"] = cached_token(cli)
        base = get_state(cli)["host"]
        print(f"登录完成 host={CFG['host']} base={CFG['base']}")
        print(f"宿主基线：running={base.get('running')} packages={base.get('packages')} "
              f"crashes={base.get('crashes')} providers={len(base.get('providers') or [])}")
        print(f"条目={len(entries)} 每批={batch_size} source={args.source} out={args.out}")
        for idx in range(0, len(entries), batch_size):
            chunk = entries[idx:idx + batch_size]
            batch_no = idx // batch_size + 1
            print(f"\n--- 批次 {batch_no}（{len(chunk)} 个） ---")
            records, trip = process_batch(cli, batch_no, chunk, args)
            for rec in records:
                write_record(args.out, rec)
                print(f"  [{rec.get('type'):13}] {rec.get('name')} id={rec.get('id')} "
                      f"routed={rec.get('providerRouted')} providers={rec.get('providers')} "
                      f"removed={rec.get('removed_ok')} "
                      f"err={rec.get('package_error') or rec.get('install_error') or rec.get('enable_error') or ''}")
            if trip is not None:
                kind, detail, host = trip
                print("\n!!! 安全闸门触发 !!!")
                print(f"类型：{kind}")
                print(f"详情：{detail}")
                print(f"宿主现场：{json.dumps(host, ensure_ascii=False)}")
                tripped = (kind, detail, host)
                break
    finally:
        try:
            cli.close()
        except Exception:  # noqa: BLE001
            pass

    all_records = read_all_records(args.out)
    write_csv(csv_path, all_records)
    print_summary(all_records)
    print(f"\nJSONL: {args.out}\nCSV:   {csv_path}")
    if tripped:
        print("结果：因安全闸门提前终止（本批已 uninstall）。")
        sys.exit(2)


if __name__ == "__main__":
    main()
