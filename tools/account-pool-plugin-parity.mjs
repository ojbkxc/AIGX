#!/usr/bin/env node
// 账号池插件面（plugin_host/wasm）漂移检查：AIGX vs 上游 Agent2API。
//
// 为什么需要它：上游每发一个 beta，wasm 宿主的能力表 / 方法臂 / 模块集合都会变；
// 人工比对 300+ 文件每次都要重来一遍，而且「能力表说支持、代码里没实现」这类
// 虚报只有静态对齐才看得出来。本脚本把三件事固化成可重跑的检查：
//
//   1. 方法臂：上游 services.rs 有、AIGX 没有（以及反过来）的 host call 方法名；
//   2. 能力表：CAPABILITIES（可声明）与 SERVED_CAPABILITIES（真服务）两侧差集，
//      并单独标出「AIGX 声明为已服务、但代码里没有任何 permission()/分派」的虚报；
//   3. 模块集合：上游有、AIGX 没有的 wasm 子模块（按相对路径）。
//
// 用法：
//   node tools/account-pool-plugin-parity.mjs                 # 人类可读报告
//   node tools/account-pool-plugin-parity.mjs --json          # 机器可读（CI 存档）
//   node tools/account-pool-plugin-parity.mjs --check         # 与基线比对，出现「新漂移」退出码 1
//   node tools/account-pool-plugin-parity.mjs --write-baseline # 把当前差集写成基线（人工确认后）
//   node tools/account-pool-plugin-parity.mjs --a2a <dir> --aigx <dir>
//
// 基线（baseline）的意义：现存缺口是**已知并已分类**的（见
// docs/ACCOUNT-POOL-PLUGIN-PARITY.md）。--check 只对「基线之外的新增漂移」报红，
// 否则旧的已知缺口会让 CI 永远红，谁都不会再看它。
//
// 说明：脚本只读文件、不做任何网络或 git 操作；上游路径默认按本机布局，
// 换机器用 --a2a 覆盖。

import { readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { join, relative, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const AIGX_DEFAULT = resolve(HERE, '..');

function parseArgs(argv) {
  const options = {
    a2a: 'D:\\GitHub\\Agent2API\\desktop-tauri\\src-tauri\\server\\src\\server\\core\\plugin_host',
    aigx: join(AIGX_DEFAULT, 'src', 'plugin_host'),
    json: false,
    check: false,
    writeBaseline: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--json') options.json = true;
    else if (arg === '--check') options.check = true;
    else if (arg === '--write-baseline') options.writeBaseline = true;
    else if (arg === '--a2a') options.a2a = argv[++index];
    else if (arg === '--aigx') options.aigx = argv[++index];
    else if (arg === '--help' || arg === '-h') {
      console.log(
        '用法: node tools/account-pool-plugin-parity.mjs [--json] [--check] [--write-baseline] [--a2a DIR] [--aigx DIR]',
      );
      process.exit(0);
    } else {
      console.error(`未知参数: ${arg}`);
      process.exit(2);
    }
  }
  return options;
}

const BASELINE_PATH = join(HERE, 'account-pool-plugin-parity.baseline.json');

function readBaseline() {
  if (!exists(BASELINE_PATH)) return null;
  try {
    return JSON.parse(readText(BASELINE_PATH));
  } catch (error) {
    console.error(`基线文件损坏: ${BASELINE_PATH}: ${error.message}`);
    return null;
  }
}

function readText(path) {
  return readFileSync(path, 'utf8');
}

function exists(path) {
  try {
    statSync(path);
    return true;
  } catch {
    return false;
  }
}

function walkRust(root) {
  const found = [];
  const visit = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) visit(full);
      else if (entry.isFile() && entry.name.endsWith('.rs')) found.push(full);
    }
  };
  if (exists(root)) visit(root);
  return found;
}

// 去掉行注释，避免把注释里的 "a.b.v1" 当成真实条目。
function stripLineComments(text) {
  return text
    .split('\n')
    .map((line) => {
      const index = line.indexOf('//');
      return index >= 0 ? line.slice(0, index) : line;
    })
    .join('\n');
}

// 方法名可能出现在两处：match 分派臂，或 `pub const X: &str = "a.b"` 常量表。
// 只扫 services.rs 会漏掉写在自己模块里的方法（login.*、tasks.* 等），
// 从而把「已实现」误报成「缺失」。
function matchArms(text) {
  const methods = new Set();
  const pattern = /^\s*((?:"[a-z0-9_.\-]+"\s*\|\s*)*"[a-z0-9_.\-]+")\s*=>/gm;
  let match;
  while ((match = pattern.exec(text)) !== null) {
    for (const literal of match[1].matchAll(/"([a-z0-9_.\-]+)"/g)) methods.add(literal[1]);
  }
  const constant =
    /pub(?:\([^)]*\))?\s+const\s+[A-Z0-9_]+:\s*&(?:'static\s+)?str\s*=\s*"([a-z0-9_.\-]+)"/g;
  while ((match = constant.exec(text)) !== null) methods.add(match[1]);
  // 有的模块用 `if method == "x"` 或 `matches!(method, "x" | "y")` 分派，
  // 不用 match 臂——只认 `=>` 会把它们误报成缺失。
  const equals = /method\s*==\s*"([a-z0-9_.\-]+)"/g;
  while ((match = equals.exec(text)) !== null) methods.add(match[1]);
  const matchesMacro = /matches!\(\s*[a-z_.]+\s*,\s*([^)]*)\)/g;
  while ((match = matchesMacro.exec(text)) !== null) {
    for (const literal of match[1].matchAll(/"([a-z0-9_.\-]+)"/g)) methods.add(literal[1]);
  }
  return methods;
}

// 全部非 manifest 的 wasm 代码里出现的方法名集合。
//
// 只保留「带点的命名空间式」字面量，并排除能力 id（`*.v1`）：
// 各家的内部状态机也用 match 分派（`"done" =>`、`"task-started" =>`），
// 不过滤会把状态字面量算成方法名。
function collectMethods(files, manifestPath) {
  const methods = new Set();
  for (const file of files) {
    if (file === manifestPath) continue;
    for (const method of matchArms(readText(file))) {
      if (!method.includes('.') || method.endsWith('.v1')) continue;
      // 版本号（"2.0.2"）与域名（"monkeycode-ai.com"）不是方法名。
      // 域名只按常见 TLD 判定——别把 `login.context` 这种以短词结尾的方法名吃掉。
      if (/^\d/.test(method)) continue;
      if (/^[a-z0-9-]+(\.[a-z0-9-]+)*\.(com|cn|net|org|io|ai|dev|app|co|me)$/.test(method)) continue;
      methods.add(method);
    }
  }
  return methods;
}

// 从 `pub const NAME: &[&str] = &[` 到匹配的 `];` 之间取字符串条目。
function constStringArray(text, name) {
  const start = text.indexOf(`pub const ${name}`);
  if (start < 0) return null;
  const open = text.indexOf('&[', start);
  if (open < 0) return null;
  const end = text.indexOf('];', open);
  if (end < 0) return null;
  const body = stripLineComments(text.slice(open + 2, end));
  return new Set([...body.matchAll(/"([a-z0-9_.\-]+)"/g)].map((entry) => entry[1]));
}

function diff(a, b) {
  const onlyA = [...a].filter((item) => !b.has(item)).sort();
  const onlyB = [...b].filter((item) => !a.has(item)).sort();
  return { onlyA, onlyB };
}

function collect(root, label, problems) {
  const wasm = join(root, 'wasm');
  const services = join(wasm, 'services.rs');
  const manifest = join(wasm, 'manifest.rs');
  if (!exists(services) || !exists(manifest)) {
    problems.push(`${label}: 找不到 wasm/services.rs 或 wasm/manifest.rs（root=${root}）`);
    return null;
  }
  const files = walkRust(wasm);
  const manifestPath = manifest;
  const allText = files
    .filter((file) => file !== manifestPath)
    .map((file) => stripLineComments(readText(file)))
    .join('\n');
  return {
    label,
    root,
    files: files.map((file) => relative(wasm, file).split('\\').join('/')).sort(),
    lines: files.reduce((sum, file) => sum + readText(file).split('\n').length, 0),
    methods: collectMethods(files, manifestPath),
    capabilities: constStringArray(readText(manifest), 'CAPABILITIES') ?? new Set(),
    served: constStringArray(readText(manifest), 'SERVED_CAPABILITIES') ?? new Set(),
    // 能力在代码里被引用的方式不止 permission()：还有 manifest.allows(...) 与
    // 各自子系统里的常量比对。所以这里统计「代码里出现过的 v1 字面量」，
    // 只有完全没出现的才算可疑多报。
    referenced: new Set(
      [...allText.matchAll(/"([a-z0-9_.\-]+\.v1)"/g)].map((match) => match[1]),
    ),
  };
}

// 「已服务但代码里完全没出现」——不必然是虚报（握手/错误形状类除外），
// 但要人工过一眼；因此这里只做清单，不计入漂移。
function unguarded(candidate, allowlist) {
  return [...candidate.served]
    .filter((capability) => !candidate.referenced.has(capability))
    .filter((capability) => !allowlist.includes(capability))
    .sort();
}

const HANDLED_WITHOUT_PERMISSION = ['errors.v1', 'settings.schema.v1'];

function main() {
  const options = parseArgs(process.argv.slice(2));
  const problems = [];
  const upstream = collect(options.a2a, 'A2A', problems);
  const ours = collect(options.aigx, 'AIGX', problems);
  if (!upstream || !ours) {
    for (const problem of problems) console.error(problem);
    process.exit(2);
  }

  const methods = diff(upstream.methods, ours.methods);
  const capabilities = diff(upstream.capabilities, ours.capabilities);
  // 上游没有独立的「已服务」表时不做差集（否则会把 AIGX 的整张表误报成多报）。
  const served = upstream.served.size > 0 ? diff(upstream.served, ours.served) : null;
  const files = diff(new Set(upstream.files), new Set(ours.files));

  const report = {
    upstream: {
      label: upstream.label,
      root: upstream.root,
      files: upstream.files.length,
      lines: upstream.lines,
      methods: upstream.methods.size,
      declared: upstream.capabilities.size,
      served: upstream.served.size,
    },
    aigx: {
      label: ours.label,
      root: ours.root,
      files: ours.files.length,
      lines: ours.lines,
      methods: ours.methods.size,
      declared: ours.capabilities.size,
      served: ours.served.size,
    },
    gaps: {
      methodsOnlyUpstream: methods.onlyA,
      methodsOnlyAigx: methods.onlyB,
      capabilitiesOnlyUpstream: capabilities.onlyA,
      capabilitiesOnlyAigx: capabilities.onlyB,
      servedOnlyUpstream: served ? served.onlyA : null,
      servedOnlyAigx: served ? served.onlyB : null,
      modulesOnlyUpstream: files.onlyA,
      modulesOnlyAigx: files.onlyB,
      servedWithoutGuard: unguarded(ours, HANDLED_WITHOUT_PERMISSION),
    },
  };

  if (options.json) {
    console.log(JSON.stringify(report, null, 2));
  } else {
    const row = (name, left, right) => `| ${name} | ${left} | ${right} |`;
    console.log(`# 插件宿主漂移报告（${new Date().toISOString()}）\n`);
    console.log(`上游: ${upstream.root}`);
    console.log(`AIGX: ${ours.root}\n`);
    console.log('| 指标 | 上游 | AIGX |');
    console.log('|---|---:|---:|');
    console.log(row('wasm 文件数', upstream.files.length, ours.files.length));
    console.log(row('wasm 行数', upstream.lines, ours.lines));
    console.log(row('方法臂', upstream.methods.size, ours.methods.size));
    console.log(row('可声明能力', upstream.capabilities.size, ours.capabilities.size));
    console.log(row('已服务能力', upstream.served.size, ours.served.size));
    console.log('');
    const list = (title, items, hint) => {
      console.log(`## ${title}（${items.length}）`);
      if (!items.length) console.log('- 无');
      else for (const item of items) console.log(`- ${item}`);
      if (hint && items.length) console.log(`\n> ${hint}`);
      console.log('');
    };
    list('上游有、AIGX 没实现的方法', methods.onlyA, '要么补实现，要么在文档里记为「不适用/延后」并写理由。');
    list('AIGX 有、上游没有的方法', methods.onlyB, 'AIGX 自加的方法不在上游契约里，升级时会被上游覆盖风险。');
    list('可声明能力差集：仅上游', capabilities.onlyA);
    list('可声明能力差集：仅 AIGX', capabilities.onlyB);
    list('已服务能力差集：仅上游', served ? served.onlyA : []);
    list(
      '已服务能力差集：仅 AIGX（＝多报）',
      served ? served.onlyB : [],
      served ? '多报会让插件以为宿主支持，实际拿到 NO_METHOD。' : '上游没有独立的已服务表，跳过比对。',
    );
    list('模块仅上游有', files.onlyA);
    list('模块仅 AIGX 有', files.onlyB);
    list('已服务但代码里没有 permission() 闸门', report.gaps.servedWithoutGuard, '握手/错误形状类可忽略，其余需人工确认。');
  }

  // ── 基线：只对「新增漂移」报红，存量缺口是已知并已分类的 ──────────────
  // 能力差集必须进基线判定：2026-11-24 v3.5.1 的 login.sms.alicom4.v1
  // 漏报就是因为它不在 baselineShape 里——上游新增能力时 --check 依旧绿。
  // modules 差集体量大且随上游重构波动（已分类文档化），维持不进 check。
  const baselineShape = {
    methodsOnlyUpstream: report.gaps.methodsOnlyUpstream,
    methodsOnlyAigx: report.gaps.methodsOnlyAigx,
    capabilitiesOnlyUpstream: report.gaps.capabilitiesOnlyUpstream,
    capabilitiesOnlyAigx: report.gaps.capabilitiesOnlyAigx,
    servedOnlyUpstream: report.gaps.servedOnlyUpstream ?? [],
    servedOnlyAigx: report.gaps.servedOnlyAigx ?? [],
    servedWithoutGuard: report.gaps.servedWithoutGuard,
  };

  if (options.writeBaseline) {
    writeFileSync(
      BASELINE_PATH,
      `${JSON.stringify(
        {
          // 基线时间只用于人读；判定不看它。
          generatedAt: new Date().toISOString(),
          upstreamRoot: report.upstream.root,
          note: '已知并已分类的缺口。--check 只对基线之外的漂移报红。',
          ...baselineShape,
        },
        null,
        2,
      )}\n`,
      'utf8',
    );
    console.log(`已写入基线: ${BASELINE_PATH}`);
    return;
  }

  if (options.check) {
    const baseline = readBaseline();
    if (!baseline) {
      console.error(`缺少基线: ${BASELINE_PATH}（先人工确认差集，再 --write-baseline）`);
      process.exit(2);
    }
    const unexpected = [];
    for (const key of Object.keys(baselineShape)) {
      const known = new Set(baseline[key] ?? []);
      for (const item of baselineShape[key]) {
        if (!known.has(item)) unexpected.push(`${key}: ${item}`);
      }
    }
    if (unexpected.length) {
      console.error('检测到基线之外的新漂移：');
      for (const item of unexpected) console.error(`- ${item}`);
      process.exit(1);
    }
    console.log('无新增漂移（与基线一致）。');
  }
}

main();
