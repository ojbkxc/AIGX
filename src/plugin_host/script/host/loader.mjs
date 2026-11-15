// 脚本包加载器 —— AIGX 自研实现。
//
// 职责：把 init_payload.packages 里的每个 npm 风格包目录加载成 provider
// 记录。每个包是一个 ESM 模块；宿主对导出的判定约定（OpenCode v1
// auth.provider 形态，只认接口不认实现）：
//
//   // 包入口导出一个工厂函数或现成对象：
//   export default function createProvider(input, options) {
//     return {
//       auth: { provider: "kiro",            // ← 必须是字符串，包声明的 provider id
//               label: "Kiro" },             //   可选展示名
//       config: { base_url: "https://…",     // ← 可选静态配置（端点/协议/模型表）
//                 upstream: "anthropic",
//                 models: ["claude-…"] },
//       "chat.headers": (input, options) => ({…}),  // ← 可选：对话请求追加头
//       "models.list": async (input, options) => …, // ← 能力方法（键=wire 方法名）
//       …
//     };
//   }
//
// 判定规则：枚举入口模块的全部命名导出（含 default）；函数导出按
// candidate(input, options) 调用取返回值，对象导出直接用；返回值有
// auth.provider 且是**非空字符串**才算一家 provider，否则静默跳过
// （一个包可以同时导出工具函数和多家 provider）。
//
// provider id 冲突加后缀：两个包（或同一包两个导出）声明同一个 id 时，
// 后来者拿到 `<id>-2`、`<id>-3`……原始声明记在 requestedId 里。
//
// 保留集合让位：requestedId 落在 init_payload.reserved（原生 provider +
// 内置订阅 + 非脚本宿主已声明）里的，记录 active=false——**脚本不能换掉
// 能用的家**（与原生包 is_plugin_owned 同立场）；dispatch 层拒绝对
// inactive provider 的调用，网关也不会把流量路由过来（它有同一份集合）。
//
// options 的读取（两级来源，KV 投影优先）：
// 1. `{script_root}/registry.json`（storage.rs 的磁盘投影 = KV 权威状态，
//    运维改配置走 KV 后投影同步刷新）——按包名匹配取 options.values；
// 2. fallback：`{script_root}/options/{package_name}.json` 落盘文件
//    （pkg-installer 安装时写）。
// script_root 从包版本目录回推：pkgDir = {root}/packages/script-{hash}/{ver}，
// 向上三级即 root（storage.rs「宿主用相对路径读同级投影」的约定）。
// 两处都没有 = 无运维配置（不是错误）。

import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

/**
 * 从 package.json 推导入口候选（相对路径列表，按优先级排）。
 *
 * 优先级与 pkg-installer 安装侧**逐字一致**（install 解析 exports["./server"]
 * > main > exports["."] > exports > index.*）——同一份 package.json 两边
 * 必须得出同一个入口：网关侧记录的 entry 用于展示/诊断，宿主侧实际加载，
 * 不一致会出现「管理面显示 A 入口、宿主跑的是 B」的哑谜。
 * （宿主侧多留 module 档兜底：pkg-installer 不认识它时两边仍会收敛到同档。）
 */
function entryCandidates(pkgJson) {
  const out = [];
  const exp = pkgJson.exports;
  // ① exports["./server"]：脚本插件的服务端入口约定档
  if (exp && typeof exp === "object" && !Array.isArray(exp)) {
    const server = exp["./server"];
    if (typeof server === "string") out.push(server);
    else if (server && typeof server === "object" && typeof server.default === "string") {
      out.push(server.default);
    }
  }
  // ② main
  if (typeof pkgJson.main === "string" && pkgJson.main) out.push(pkgJson.main);
  // ③ exports["."]/exports 本体（字符串或 conditions 对象的 default）
  if (typeof exp === "string") {
    out.push(exp);
  } else if (exp && typeof exp === "object" && !Array.isArray(exp)) {
    const dot = exp["."] !== undefined ? exp["."] : exp;
    if (typeof dot === "string") out.push(dot);
    else if (dot && typeof dot === "object" && typeof dot.default === "string") out.push(dot.default);
  }
  // ④ module（install 侧没有这档，仅宿主兜底）
  if (typeof pkgJson.module === "string" && pkgJson.module) out.push(pkgJson.module);
  // ⑤ 约定俗成的入口名
  out.push("index.mjs", "index.js");
  return out;
}

/** 解析包入口的绝对路径；找不到返回 null。 */
async function resolveEntry(pkgDir) {
  let pkgJson = {};
  try {
    pkgJson = JSON.parse(await readFile(path.join(pkgDir, "package.json"), "utf8"));
  } catch {
    // 没有 package.json 的裸目录：只试兜底入口
  }
  for (const rel of entryCandidates(pkgJson)) {
    const base = path.resolve(pkgDir, rel);
    for (const candidate of expandExtensions(base)) {
      try {
        const st = await stat(candidate);
        if (st.isFile()) return candidate;
      } catch {
        // 试下一个
      }
    }
  }
  return null;
}

/** 入口路径可能不带扩展名：依次补 .mjs / .js / .ts 试。 */
function expandExtensions(base) {
  const ext = path.extname(base);
  if (ext) return [base];
  return [base + ".mjs", base + ".js", base + ".ts", base];
}

/**
 * 读包的运维配置（两级来源，registry.json 投影优先）。
 *
 * scriptRoot 从包版本目录回推：pkgDir = {root}/packages/script-{hash}/{ver}
 * → 向上三级即 root。registry.json 是 storage.rs 的磁盘投影（KV 权威状态，
 * 运维改配置走 KV 后同步刷新），按包名匹配取 `options.values`；投影没有
 * 该包或没落盘时 fallback 到 `options/{name}.json` 落盘文件。两级都缺 = {}。
 */
async function readOptions(pkgDir, pkgName) {
  if (!pkgName) return {};
  // 从 {root}/packages/{dir}/{ver} 回推 {root}
  const scriptRoot = path.resolve(pkgDir, "..", "..", "..");
  // 1. registry.json 投影（KV 权威状态的宿主侧视图）
  try {
    const projection = JSON.parse(await readFile(path.join(scriptRoot, "registry.json"), "utf8"));
    const entries = Array.isArray(projection?.packages) ? projection.packages : [];
    const pkg = entries.find((p) => p && p.id === pkgName);
    const values = pkg?.options?.values;
    if (isPlainObject(values)) return values;
  } catch {
    // 投影缺失/坏文件/版本不符：走落盘 fallback（不是错误）
  }
  // 2. 落盘 options 文件（pkg-installer 安装时写）
  const optionsPath = path.join(scriptRoot, "options", `${pkgName}.json`);
  try {
    const parsed = JSON.parse(await readFile(optionsPath, "utf8"));
    return isPlainObject(parsed) ? parsed : {};
  } catch {
    return {};
  }
}

function isPlainObject(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function asString(v) {
  return typeof v === "string" && v.length > 0 ? v : undefined;
}

function asStringArray(v) {
  return Array.isArray(v) ? v.filter((x) => typeof x === "string") : undefined;
}

/**
 * 加载全部脚本包。
 *
 * @param init init 载荷（packages/reserved/claimed/catalog_path，snake_case）
 * @param io  { log(level, message), startup(message) } —— 启动阶段的对外上报
 * @returns {{ providers: Map<string, object>, loaded: object[], failed: object[] }}
 */
export async function loadPackages(init, io) {
  const reserved = new Set(Array.isArray(init.reserved) ? init.reserved : []);
  const packages = Array.isArray(init.packages) ? init.packages : [];
  /** id → provider 记录（冲突后缀后的 id 是键）。 */
  const providers = new Map();
  /** 已占用的 provider id（含 requestedId 与加缀后的 id）。 */
  const usedIds = new Set();
  const loaded = [];
  const failed = [];

  for (const pkgDir of packages) {
    let meta = { name: undefined, version: undefined };
    try {
      meta = await readPackageMeta(pkgDir);
    } catch {
      // 读不到 package.json 照样往下走（裸目录也能有 index.mjs）
    }
    const options = await readOptions(pkgDir, meta.name);
    try {
      const entryAbs = await resolveEntry(pkgDir);
      if (!entryAbs) {
        failed.push({ pkg: meta.name || pkgDir, reason: "no entry resolved" });
        io.log("warn", `包 ${meta.name || pkgDir} 推导不出入口，跳过`);
        continue;
      }
      const mod = await import(pathToFileURL(entryAbs).href);
      // 工厂入参：input = 包上下文 + 日志口；options = 运维配置
      const input = {
        pkgDir,
        pkgName: meta.name ?? null,
        pkgVersion: meta.version ?? null,
        catalogPath: init.catalog_path ?? null,
        log: io.log,
      };
      let found = 0;
      for (const name of Object.keys(mod)) {
        const candidate = mod[name];
        let hooks;
        try {
          hooks = typeof candidate === "function" ? await candidate(input, options) : candidate;
        } catch (err) {
          io.log("warn", `包 ${meta.name || pkgDir} 导出 ${name} 工厂抛错：${err?.message ?? err}`);
          continue;
        }
        if (!isPlainObject(hooks)) continue;
        // 硬性判据：auth.provider 必须是非空字符串
        const requestedId = asString(hooks?.auth?.provider);
        if (!requestedId) continue;
        // id 冲突加后缀：-2、-3……（原始声明保留在 requestedId）
        let id = requestedId;
        let n = 2;
        while (usedIds.has(id)) id = `${requestedId}-${n++}`;
        usedIds.add(id);
        usedIds.add(requestedId);
        const config = isPlainObject(hooks.config) ? hooks.config : {};
        const record = {
          id,
          requestedId,
          exportName: name,
          pkgDir,
          pkgName: meta.name ?? null,
          label: asString(hooks.auth.label) ?? asString(hooks.label) ?? name,
          upstream: asString(config.upstream) ?? asString(hooks.auth.upstream) ?? "chat",
          models: asStringArray(config.models) ?? asStringArray(hooks.auth.models) ?? [],
          config,
          chatHeaders:
            typeof hooks["chat.headers"] === "function" || isPlainObject(hooks["chat.headers"])
              ? hooks["chat.headers"]
              : null,
          options,
          hooks,
          // 保留集合让位：脚本对 reserved 里的家没有接管权
          active: !reserved.has(requestedId),
        };
        providers.set(id, record);
        found += 1;
        if (!record.active) {
          io.log(
            "info",
            `provider ${requestedId} 在保留集合中，脚本侧让位（${id} 不接管）`,
          );
        }
      }
      if (found === 0) {
        io.log("warn", `包 ${meta.name || pkgDir} 没有发现 auth.provider 导出`);
      }
      loaded.push({ pkg: meta.name || pkgDir, providers: found });
    } catch (err) {
      failed.push({ pkg: meta.name || pkgDir, reason: err?.message ?? String(err) });
      io.log("warn", `加载包 ${meta.name || pkgDir} 失败：${err?.message ?? err}`);
    }
  }

  return { providers, loaded, failed };
}

/** 读 package.json 的 name/version（坏文件不抛，返回空名）。 */
async function readPackageMeta(pkgDir) {
  try {
    const pkgJson = JSON.parse(await readFile(path.join(pkgDir, "package.json"), "utf8"));
    return {
      name: typeof pkgJson.name === "string" && pkgJson.name ? pkgJson.name : undefined,
      version: typeof pkgJson.version === "string" ? pkgJson.version : undefined,
    };
  } catch {
    return { name: undefined, version: undefined };
  }
}
