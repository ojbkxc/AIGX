#!/usr/bin/env python3
"""alerts.yml / prometheus.yml 静态校验闸门（CI：alerts-lint job 调用）。

校验项：
1. YAML 严格解析：重复键直接报错（历史 alerts.yml 顶层重复 `groups:` 键
   导致 Prometheus 整体拒绝加载、告警体系全灭——本闸门防回归）。
2. 告警规则结构：alerts.yml 顶层只能有 groups；每条规则必须含
   alert + expr；禁止非法字段（历史 on_resolve / target_metric 残片）；
   alert 名全局唯一；`for` 必须为 Prometheus 时长格式。
3. 指标存在性：alerts.yml expr 中引用的指标必须真实存在于
   src/metrics.rs render() 的输出里（导出集合从 metrics.rs 源码派生，
   不手打）。`up` 为 Prometheus 合成指标，单独放行。
4. 交叉一致性：alerts.yml 中 `up{job="X"}` 引用的 job 必须存在于
   prometheus.yml 的 scrape_configs；aigx-backend job 必须配置
   bearer 凭据（鉴权断链防回归）；禁止 postgres:5432 / redis:6379 /
   aigx-frontend 裸端口采集回归。

退出码 0=通过，1=失败（错误逐条打印）。
"""
import re
import sys
from pathlib import Path

import yaml

# Windows 下 Python stdout 默认 GBK，中文输出会乱码；统一为 UTF-8
# （CI 的 Linux 环境本就 UTF-8，这里无害）。
try:
    sys.stdout.reconfigure(encoding="utf-8")
except (AttributeError, OSError):
    pass

REPO = Path(__file__).resolve().parents[2]  # .github/workflows/monitoring_lint.py → repo root


def paths_from_argv() -> tuple[Path, Path, Path]:
    """允许用 argv 覆盖三个目标路径（负向测试用），缺省为仓库内真实路径。"""
    a = Path(sys.argv[1]) if len(sys.argv) > 1 else REPO / "monitoring" / "alerts.yml"
    p = Path(sys.argv[2]) if len(sys.argv) > 2 else REPO / "monitoring" / "prometheus.yml"
    m = Path(sys.argv[3]) if len(sys.argv) > 3 else REPO / "src" / "metrics.rs"
    return a.resolve(), p.resolve(), m.resolve()


ALERTS, PROM, METRICS_RS = paths_from_argv()

# PromQL 函数/关键字白名单：expr 中出现的这些标识符不是指标名
PROMQL_WHITELIST = {
    "sum", "rate", "irate", "increase", "delta", "idelta", "deriv",
    "count", "avg", "max", "min", "stddev", "stdvar", "quantile",
    "histogram_quantile", "quantile_over_time", "last_over_time",
    "present_over_time", "absent", "absent_over_time", "scalar",
    "vector", "clamp", "clamp_max", "clamp_min", "floor", "ceil",
    "round", "changes", "timestamp", "label_replace", "label_join",
    "by", "on", "ignoring", "without", "bool", "and", "or", "unless",
    "offset", "start", "end", "le", "job", "instance",
}

# Prometheus 时长格式（如 5m / 30s / 1h / 1d）
DURATION_RE = re.compile(r"^\d+(s|m|h|d|w|y)$")
# 合法告警名（Prometheus：字母/下划线/冒号开头，字母数字下划线冒点）
ALERT_NAME_RE = re.compile(r"^[a-zA-Z_:][a-zA-Z0-9_:]*$")


class StrictLoader(yaml.SafeLoader):
    """拒绝重复键的 SafeLoader。"""


def _no_dupes(loader, node, deep=False):
    mapping = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            raise yaml.constructor.ConstructorError(
                None, None,
                f"duplicate key {key!r} (line {key_node.start_mark.line + 1})",
                key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


StrictLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _no_dupes
)


def load_strict(path: Path):
    with open(path, encoding="utf-8") as f:
        return yaml.load(f, Loader=StrictLoader)


def exported_metrics() -> set[str]:
    """从 src/metrics.rs render() 源码派生导出的指标集合。

    两类来源：
    - `# TYPE <name>` / `# HELP <name>` 行（summary/histogram 的族名）；
    - 样本行里指标名后紧跟 `{`（如 `aigx_requests_total{{model=...`）。
    只扫 render() 函数体，排除 tests 模块的断言字符串。
    """
    src = METRICS_RS.read_text(encoding="utf-8")
    m = re.search(
        r"pub fn render\(&self\) -> String \{.*?\n    \}", src, re.S
    )
    body = m.group(0) if m else src
    names = set(re.findall(r"# (?:TYPE|HELP) (aigx_[A-Za-z0-9_]+)", body))
    names |= set(re.findall(r"aigx_[A-Za-z0-9_]+(?=\s*\{)", body))
    return names


def metric_refs_in_expr(expr: str) -> set[str]:
    """提取 expr 中引用的指标名（剔除 label matcher / 区间选择器 / PromQL 关键字）。

    注意：必须先剔除 `[5m]` 这类区间选择器，否则 `5m` 里的 `m` 会被
    标识符正则单独切成一个 token，误判为幽灵指标。
    """
    no_labels = re.sub(r"\{[^{}]*\}", "", expr)  # 去掉 {status="error"} 等
    no_ranges = re.sub(r"\[[^\]]*\]", "", no_labels)  # 去掉 [5m] 区间
    refs = set()
    for tok in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", no_ranges):
        if tok not in PROMQL_WHITELIST:
            refs.add(tok)
    return refs


errors: list[str] = []


def check(cond: bool, msg: str) -> None:
    if not cond:
        errors.append(msg)


def main() -> int:
    # ---- 1. alerts.yml 严格解析 + 结构校验 ----
    try:
        doc = load_strict(ALERTS)
    except yaml.YAMLError as e:
        print(f"[FAIL] alerts.yml YAML 解析失败: {e}")
        return 1
    check(isinstance(doc, dict), f"alerts.yml 顶层必须是 mapping，实际: {type(doc)}")
    if not isinstance(doc, dict):
        return report()
    check(
        set(doc.keys()) == {"groups"},
        f"alerts.yml 顶层键必须只有 groups，实际: {sorted(doc.keys())}",
    )

    alert_names: set[str] = set()
    job_refs: set[str] = set()
    exprs: list[tuple[str, str]] = []
    for gi, group in enumerate(doc.get("groups", []) or []):
        check(isinstance(group, dict), f"groups[{gi}] 必须是 mapping")
        if not isinstance(group, dict):
            continue
        check("name" in group, f"groups[{gi}] 缺 name")
        rules = group.get("rules")
        check(
            isinstance(rules, list) and len(rules) > 0,
            f"groups[{gi}]({group.get('name')}) rules 必须是非空列表",
        )
        for ri, rule in enumerate(rules or []):
            where = f"groups[{gi}]({group.get('name')}).rules[{ri}]"
            check(isinstance(rule, dict), f"{where} 必须是 mapping")
            if not isinstance(rule, dict):
                continue
            illegal = set(rule.keys()) - {"alert", "expr", "for", "labels", "annotations"}
            check(not illegal, f"{where} 含非法字段: {sorted(illegal)}")
            name = rule.get("alert")
            check(
                isinstance(name, str) and bool(ALERT_NAME_RE.match(name)),
                f"{where} alert 名缺失或非法: {name!r}",
            )
            if isinstance(name, str) and name:
                check(name not in alert_names, f"告警名重复: {name}")
                alert_names.add(name)
            check(
                isinstance(rule.get("expr"), str) and rule.get("expr").strip(),
                f"{where} 缺非空 expr",
            )
            if isinstance(rule.get("expr"), str):
                exprs.append((name or "?", rule["expr"]))
            for_val = rule.get("for")
            if for_val is not None:
                check(
                    isinstance(for_val, str) and bool(DURATION_RE.match(for_val)),
                    f"{where} for 时长格式非法: {for_val!r}",
                )
            for m in re.finditer(r'up\{job="([^"]+)"\}', rule.get("expr") or ""):
                job_refs.add(m.group(1))

    # ---- 2. prometheus.yml 严格解析 + 交叉校验 ----
    try:
        prom = load_strict(PROM)
    except yaml.YAMLError as e:
        print(f"[FAIL] prometheus.yml YAML 解析失败: {e}")
        return 1
    check(isinstance(prom, dict), f"prometheus.yml 顶层必须是 mapping")
    job_names: set[str] = set()
    raw_targets: list[str] = []
    if isinstance(prom, dict):
        check("scrape_configs" in prom, "prometheus.yml 缺 scrape_configs")
        for j in prom.get("scrape_configs", []) or []:
            check(isinstance(j, dict), "scrape_configs 元素必须是 mapping")
            if not isinstance(j, dict):
                continue
            jname = j.get("job_name")
            check(isinstance(jname, str) and bool(jname), f"scrape job 缺 job_name: {j}")
            if isinstance(jname, str):
                check(jname not in job_names, f"scrape job_name 重复: {jname}")
                job_names.add(jname)
            targets = [
                str(t)
                for sc in j.get("static_configs", []) or []
                for t in (sc.get("targets") or [])
            ]
            raw_targets.extend(targets)
            if any("aigx-backend" in t for t in targets):
                # 抓 AIGX /metrics 的 job 必须带 bearer 凭据（无凭据必 401）；
                # prometheus 自监控 / node_exporter 等其它目标不需要。
                check(
                    "bearer_token" in j or "bearer_token_file" in j,
                    f"job {jname} 抓 aigx-backend 但未配置 bearer 凭据（/metrics 必 401）",
                )
        for t in raw_targets:
            check(
                ":5432" not in t,
                f"禁止 postgres 裸端口采集（必败→up=0 误报）: {t}",
            )
            check(
                ":6379" not in t,
                f"禁止 redis 裸端口采集（必败→up=0 误报）: {t}",
            )
            check(
                "aigx-frontend" not in t,
                f"禁止抓 SPA 前端 /metrics（返回 HTML 必败）: {t}",
            )
        for r in job_refs:
            check(
                r in job_names,
                f"alerts.yml 引用 up{{job=\"{r}\"}} 但 prometheus.yml 无该 scrape job",
            )

    # ---- 3. 指标存在性：expr 引用 vs metrics.rs 实际导出 ----
    if METRICS_RS.exists():
        exported = exported_metrics()
        for alert, expr in exprs:
            for ref in metric_refs_in_expr(expr):
                if ref == "up":
                    continue  # Prometheus 合成指标
                check(
                    ref in exported,
                    f"告警 {alert} 引用了 /metrics 不存在的指标: {ref}",
                )
    else:
        errors.append(f"找不到 {METRICS_RS}（指标导出集合的派生来源）")

    return report(exported_metrics() if METRICS_RS.exists() else set())


def report(exported=None) -> int:
    if exported is not None:
        print(f"[info] metrics.rs 导出指标 {len(exported)} 个: {sorted(exported)}")
    if errors:
        print(f"[FAIL] {len(errors)} 项校验失败:")
        for e in errors:
            print(f"  - {e}")
        return 1
    print("[OK] alerts.yml / prometheus.yml 静态校验全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
