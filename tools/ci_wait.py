#!/usr/bin/env python3
"""轮询 AIGX CI 构建 workflow run 状态，完成后输出 artifact 下载信息。"""
import json
import re
import sys
import time
import urllib.request

REPO = "ojbkxc/AIGX"
CFG = open(r"D:\GitHub\AIGX\.git\config", encoding="utf-8").read()
TOKEN = re.search(r"https://([A-Za-z0-9_]+)@github.com", CFG).group(1)

TARGET_COMMIT = sys.argv[1] if len(sys.argv) > 1 else None

def api(url):
    req = urllib.request.Request(url, headers={
        "Authorization": f"token {TOKEN}",
        "Accept": "application/vnd.github+json",
        "User-Agent": "aigx-deploy",
    })
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read().decode())

deadline = time.time() + 1500  # 最长等 25 分钟
target_run = None
while time.time() < deadline:
    runs = api(f"https://api.github.com/repos/{REPO}/actions/runs?per_page=6")["workflow_runs"]
    for run in runs:
        if run["status"] != "completed":
            if TARGET_COMMIT and run["head_sha"].startswith(TARGET_COMMIT):
                target_run = run
                print(f"[{time.strftime('%H:%M:%S')}] run {run['id']} {run['name']} {run['status']}/{run['conclusion']} ...", flush=True)
                break
            if not TARGET_COMMIT:
                target_run = run
                break
    else:
        if target_run is None:
            print(f"[{time.strftime('%H:%M:%S')}] 尚无进行中的 run，等待 ...", flush=True)
            time.sleep(30)
            continue
    run = target_run
    if run["status"] == "completed":
        print(f"DONE {run['id']} conclusion={run['conclusion']}")
        if run["conclusion"] == "success":
            arts = api(f"https://api.github.com/repos/{REPO}/actions/runs/{run['id']}/artifacts")
            for a in arts["artifacts"]:
                print(f"ARTIFACT {a['name']} id={a['id']} size={a['size_in_bytes']}")
        sys.exit(0 if run["conclusion"] == "success" else 1)
    time.sleep(30)

print("TIMEOUT")
sys.exit(2)
