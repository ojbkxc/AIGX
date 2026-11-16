// 脚本宿主入口 —— AIGX 自研实现（上游 AGPL，仅按协议契约独立书写）。
//
// 启动参数：`bun run --no-env-file <host_dir>/main.mjs`，cwd = host_dir
// （由 src/plugin_host/script/host_rs.rs 组装并 spawn）。
//
// 生命周期：
// 1. 立即启动 stdin 读循环（wire.mjs）——网关的第一帧一定是 init 请求；
// 2. init：读 init 载荷（snake_case 四键：packages/reserved/claimed/
//    catalog_path，契约见 types.rs；请求形状 {"protocol":N,"init":{…}}，
//    兼容平铺在 params 上的旧形状）→ 加载全部脚本包 → 回
//    {"providers":[{id,label,upstream,models}]}；
// 3. 之后所有请求交给 dispatch（provider.mjs），credit/shutdown 在
//    wire/main 两层就近处理；
// 4. 网关发 shutdown（或关 stdin=EOF）→ 应答后退出。
//
// 启动阶段上报：包加载失败走 id=0 的 log 通知（网关 tracing 直收）；
// 装载摘要以 {"id":0,"event":"startup","message":…} 事件帧发出。

import { Wire } from "./wire.mjs";
import { loadPackages } from "./loader.mjs";
import { createDispatcher } from "./provider.mjs";
import { classifyError, describeError, ErrorCodes, ProviderError } from "./errors.mjs";

/** 本宿主实现的协议版本（与 plugin_host/rpc.rs 的 PROTOCOL_VERSION 对表）。 */
const HOST_PROTOCOL = 1;

const wire = new Wire();
/** init 完成前的请求一律按 local_precondition 拒绝。 */
const state = { dispatch: null };

/** 启动阶段的对外上报口（loader 用）。 */
function makeIO() {
  return {
    log: (level, message) => wire.notifyLog(level, message),
    startup: (message) => wire.sendStartupEvent(message),
  };
}

/** init 握手：加载全部包 → 建 dispatch 表 → 回 providers。 */
async function handleInit(frame) {
  const params = frame.params && typeof frame.params === "object" ? frame.params : {};
  // 协议版本对表（rpc.rs 的 PROTOCOL_VERSION）：不一致拒绝加载，别带着
  // 未知语义的载荷硬跑。缺省（老网关的平铺形状）不做校验。
  if (typeof params.protocol === "number" && params.protocol !== HOST_PROTOCOL) {
    throw new ProviderError(
      ErrorCodes.LocalPrecondition,
      `protocol mismatch: gateway=${params.protocol}, host=${HOST_PROTOCOL}`,
    );
  }
  // types.rs 契约：载荷在 params.init（snake_case）；平铺形状兼容读取
  const init = params.init && typeof params.init === "object" ? params.init : params;
  const io = makeIO();
  const { providers, loaded, failed } = await loadPackages(init, io);
  state.dispatch = createDispatcher({ wire, registry: providers });
  const all = [...providers.values()];
  // providers / identities 只报**实际接管**的家（active=true）：
  // 让位给 reserved 的记录不进入路由表——网关按这份名单建 owners，
  // 报了 inactive 会让数据面把请求路由到一个明确拒绝的 provider。
  const records = all.filter((r) => r.active);
  const list = records.map((r) => ({
    id: r.id,
    label: r.label,
    upstream: r.upstream,
    models: [...r.models],
  }));
  // identities：声明 id（hooks.auth.provider 原名）→ 实际接管 id（冲突加缀后）。
  // 键是包名（pkgId，与 pkg-installer 的 identities 落库键对齐）；无包名的
  // 本地裸目录跳过——网关无从建立 pkgId 映射。
  const identities = {};
  for (const r of records) {
    if (!r.pkgName) continue;
    if (!isPlainObject(identities[r.pkgName])) identities[r.pkgName] = {};
    identities[r.pkgName][r.requestedId] = r.id;
  }
  io.startup(
    `packages=${loaded.length} failed=${failed.length} providers=${all.length} active=${records.length}`,
  );
  await wire.reply(frame.id, { providers: list, identities });
}

function isPlainObject(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

wire.onRequest = async (frame) => {
  try {
    if (frame.method === "init") {
      await handleInit(frame);
      return;
    }
    if (!state.dispatch) {
      await wire.replyError(frame.id, {
        message: "script host not initialized (init not completed)",
        code: ErrorCodes.LocalPrecondition,
      });
      return;
    }
    await state.dispatch(frame).then((out) => {
      // dispatcher 对 shutdown 回 result 并返回 {shutdown:true}：
      // 给 stdout 一点 flush 时间（writeLine 已 await drain，短暂延迟兜底）
      if (out && typeof out === "object" && out.shutdown === true) {
        setTimeout(() => process.exit(0), 50);
      }
    });
  } catch (err) {
    // dispatch 内部已兜底；这里是最后一道防线（例如 init 抛错）
    await wire.replyError(frame.id, classifyError(err));
  }
};

wire.onNotify = (frame) => {
  // 网关通知：打到 stderr 做本地诊断（不回帧）
  const params = frame.params && typeof frame.params === "object" ? frame.params : {};
  const text = params.message || frame.message || "";
  if (text) console.error(`[gateway] ${text}`);
};

wire.onEvent = (frame) => {
  // 入站事件帧今天没有语义（流式事件只从宿主流向网关）；留观察口
  console.error(`[gateway event] id=${frame.id} event=${frame.event ?? ""}`);
};

// 进程级兜底：回调里逃逸的异步错误不能悄悄杀死宿主
process.on("unhandledRejection", (err) => {
  console.error(`[script-host] unhandledRejection: ${describeError(err)}`);
});
process.on("uncaughtException", (err) => {
  console.error(`[script-host] uncaughtException: ${describeError(err)}`);
  process.exit(1);
});

wire.start();
