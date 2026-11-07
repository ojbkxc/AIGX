# tools/bench — AIGX 性能基准 harness

零依赖（node >= 18）。配套验收报告：`_a2a-port-recon/15-audit.md`（基线数字、回归判据、A/B 流程的权威出处）。

## 组件

| 脚本 | 作用 |
|---|---|
| `mock-upstream.mjs` | openai_compat 形态 mock 上游；非流式 256B 固定响应、流式 16 chunk SSE；`MOCK_DELAY_MS` 注入上游延迟 |
| `bootstrap.mjs` | 引导一次全新实例：解 ALTCHA PoW → 登录 → 改密 → 建渠道 → 建**固定** API key（可复现） |
| `make-channels.mjs` | 建 N 个同构渠道（选路规模探针，`CHANNELS=N`） |
| `bench.mjs` | 压测：`--mode nonstream\|stream --conc C --n N`；输出 RPS / P50/P90/P99 / TTFB / err / RSS 峰值 |

## 复现步骤

```bash
# 1. mock 上游（绑本机公网 IPv6 GUA —— SSRF 守卫拒内网地址，GUA 放行且流量不出机）
MOCK_HOST=<公网IPv6> node mock-upstream.mjs

# 2. 网关（Release 产物；数据目录隔离到临时目录）
AIGX_SERVER__DATA_DIR=/tmp/aigx-bench/data AIGX_SERVER__PORT=18800 \
  AIGX_SERVER__HOST=127.0.0.1 ./aigx.exe > server.log 2>&1 &

# 3. 引导 + 渠道
UPSTREAM_HOST=<公网IPv6> node bootstrap.mjs            # 1 渠道
UPSTREAM_HOST=<公网IPv6> CHANNELS=31 node make-channels.mjs  # 选路规模探针

# 4. 基线矩阵（3 轮取中位）
node bench.mjs --mode nonstream --conc 1  --n 800
node bench.mjs --mode nonstream --conc 8  --n 3000
node bench.mjs --mode nonstream --conc 32 --n 4000
node bench.mjs --mode stream    --conc 8  --n 1500
```

## 已知坑（复现必读）

1. **`dirs::home_dir()` 不吃 USERPROFILE 环境变量**（known-folder API）——数据隔离必须用 `AIGX_SERVER__DATA_DIR`，否则会连到真实 `~/.aigx/data`。
2. 配置文件路径不可重定向（无 CLI/env 覆盖）；`PUT /api/settings` 会**持久化写回** `~/.aigx/config.toml`——不要用 settings API 开 `allow_internal_upstream` 来绕 SSRF 守卫，用 GUA 方案。
3. ALTCHA challenge **一次性消费**：每次登录现解新题（脚本已处理）。
4. `tasklist /FI` 在 git-bash 会被路径转换吃掉，脚本内经 `execSync`（cmd.exe）调用无碍。

## route_replay 快照 A/B（#12 落地后）

1. build.yml 绿 run 下载 `aigx-windows-amd64` artifact 换 exe；
2. `AIGX_SERVER__DATA_DIR` 全新库 → bootstrap → `CHANNELS=31 make-channels`；
3. `route_replay=off`（默认）跑矩阵 → `PUT /api/settings {"route_replay":true}` → 同库态同批次重跑；
4. 判据：ON−OFF 的 RPS 差 ≤5% 且 P50 差 ≤1ms → 「无可测回归」；超限复查快照构造路径（是否 O(候选数)、有无逐候选字符串格式化、是否搭 `RequestLogStore::add` 既有 put 便车）。

## 回归判据（摘要）

同机同 mock、全新库、warmup 50、每档 3 轮取中位；非流式 c8：RPS ≥ 基线−10% 报警 / −20% 打回；P50 ≤ +50% 报警 / +100% 打回；RSS ≤ +20% 报警。完整表见 15-audit.md A.3。
