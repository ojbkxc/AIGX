// provider 能力面 dispatch —— AIGX 自研实现。
//
// 职责：把网关请求（约 30 个 provider 能力方法）分发到脚本 hooks，并
// 内置 conversation 的流式默认管线。
//
// ## dispatch 方法表（24 个 provider 能力方法 + 传输层 2 个）
//
// 能力方法按「wire 方法名 = hooks 上的键名」直查（hooks["models.list"] 等），
// 不发明第二套命名映射。钩子签名统一 hooks[method](input, options)：
// input = 网关请求的整个 params（conversation 形状见 bridge/plugin.rs：
// {provider, account, body}），options = 包运维配置。
//
// 特例：
// - conversation —— 优先用脚本自己的 hooks.conversation(input, options, stream)，
//   stream = {head, chunk, end, error} 绑定本次 RPC id 的发送器；脚本没用
//   stream 就按普通应答回。脚本没实现时走本文件的默认管线（见下）。
// - models.list / models.meta —— 无钩子时用包声明的静态模型表兜底。
//
// ## 默认 conversation 管线（refresh 凭证 → 协议选择 → 请求体转换 RPC →
//    fetch 上游 → head → SSE 分块经网关转换 → finish → credit 归还）
//
// 1. 凭证：store.get 回调读账号（account 空 = 取队首）；hooks["token.ensure"]
//    存在则先跑（可返回 {secret} 刷新，宿主用 store.update CAS 落回）；
// 2. 端点与头：config.base_url + 默认路径（协议各异）→ 上游协议非 chat 时
//    先调 script.protocol.request 让网关把 OpenAI 请求体转成上游协议；
// 3. fetch 走 script.transport.request（网关出口代理，带流式 session）；
// 4. 非 2xx：按网关 take_head 的约定推 head(状态码) → 错误体字节 → end，
//    **不**回 error 应答（错误体让网关按上游错误形状解析）；
// 5. 2xx：head 帧 → 循环 script.transport.read 取上游原始字节 → 非_chat
//    协议经 script.protocol.chunk 转成 OpenAI SSE 字节 → chunk 帧发出；
// 6. 收尾：script.protocol.finish 冲刷转换器尾部 → transport.close → end 帧；
// 7. credit：chunk 发送窗口耗尽自动停发等网关 credit 帧（wire 层闭环）。
//
// ## 宿主 → 网关回调方法（偶数 id，script-core 的 #6 实现）
//
// - store.get / store.list / store.update / settings.get / settings.set /
//   store.ledger_get / store.ledger_put —— 既有 callbacks.rs 方法面
// - script.transport.request {method,url,headers,body_b64?,stream} →
//   {status,headers,session?}（stream=true 时带 session）
// - script.transport.read {session} → {chunk_b64?,done}（阻塞到下一块或 EOF）
// - script.transport.close {session} → {}
// - script.protocol.request {protocol,body} → {body?,path?,headers?}
// - script.protocol.chunk {protocol,stream,data(b64)} → {data(b64),done?}
//   （stream = 转换器会话键，网关并发 ≤128）
// - script.protocol.finish {stream} → {data?}（转换器尾部冲刷）

import { classifyError, ErrorCodes, precondition, unsupported } from "./errors.mjs";

/** 宿主 → 网关回调方法名（与 callbacks.rs / #6 的 transport/protocol 对齐）。 */
export const CALLBACK = Object.freeze({
  storeGet: "store.get",
  storeList: "store.list",
  storeUpdate: "store.update",
  settingsGet: "settings.get",
  settingsSet: "settings.set",
  ledgerGet: "store.ledger_get",
  ledgerPut: "store.ledger_put",
  transportRequest: "script.transport.request",
  transportRead: "script.transport.read",
  transportClose: "script.transport.close",
  protocolRequest: "script.protocol.request",
  protocolChunk: "script.protocol.chunk",
  protocolFinish: "script.protocol.finish",
});

/**
 * dispatch 方法表（provider 能力面）。conversation 走特例管线；
 * 其余方法无钩子时按默认实现或 unsupported 处理。
 */
export const DISPATCH_METHODS = Object.freeze([
  "conversation",
  "models.list",
  "models.refresh",
  "models.meta",
  "token.ensure",
  "token.refresh",
  "token.expiring",
  "usage.query",
  "checkin.claim",
  "account.normalize",
  "account.view",
  "account.identity",
  "login.start",
  "login.poll",
  "login.callback",
  "login.cancel",
  "admin.action",
  "classify",
  "is_error_response",
  "refusal_markers",
  "retry_advice",
  "error.policy",
  "cooldown_models",
  "reasoning_patch",
]);

/** conversation 处理器的「已流式」标记：dispatcher 据此跳过 result 应答。 */
export const STREAMED = Symbol("streamed");

const b64 = (bytes) => Buffer.from(bytes).toString("base64");
const unb64 = (s) => Buffer.from(String(s ?? ""), "base64");

function isPlainObject(v) {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

function joinUrl(base, p) {
  return `${String(base).replace(/\/+$/, "")}/${String(p).replace(/^\/+/, "")}`;
}

/** 各上游协议的默认 chat 路径（config.chat_path 可覆盖）。 */
function defaultChatPath(upstream, model) {
  switch (upstream) {
    case "anthropic":
      return "v1/messages";
    case "responses":
      return "v1/responses";
    case "gemini":
      return `v1beta/models/${encodeURIComponent(model || "default")}:streamGenerateContent?alt=sse`;
    default:
      return "v1/chat/completions";
  }
}

/** 各上游协议的默认鉴权头（转换器/脚本头可再覆盖）。 */
function defaultAuthHeaders(upstream, secret) {
  if (!secret) return {};
  switch (upstream) {
    case "anthropic":
      return { "x-api-key": secret, "anthropic-version": "2023-06-01" };
    case "gemini":
      return { "x-goog-api-key": secret };
    default:
      return { authorization: `Bearer ${secret}` };
  }
}

/** 构造 provider 记录查找 + 单方法分发器。 */
export function createDispatcher({ wire, registry }) {
  /**
   * 分发一个网关请求帧。约定（防双重应答）：
   * - 处理器 throw → dispatch 回 error 应答（head 未发的失败通道）；
   * - 处理器返回 STREAMED → 帧已按流式语义自己收尾，dispatcher 不再应答。
   */
  return async function dispatch(frame) {
    const { id, method, params } = frame;
    try {
      if (method === "shutdown") {
        await wire.reply(id, null);
        return { shutdown: true };
      }
      if (!DISPATCH_METHODS.includes(method)) {
        throw unsupported(`method '${method}' not supported by script host`);
      }
      const paramsObj = isPlainObject(params) ? params : {};
      const providerId = typeof paramsObj.provider === "string" ? paramsObj.provider : "";
      if (!providerId) {
        throw precondition(`method '${method}' requires params.provider`);
      }
      const record = registry.get(providerId);
      if (!record) {
        throw precondition(`unknown provider '${providerId}'`);
      }
      if (!record.active) {
        throw precondition(
          `provider '${record.requestedId}' is reserved; script does not take over`,
        );
      }
      if (method === "conversation") {
        const out = await runConversation(wire, record, paramsObj, id);
        if (out !== STREAMED) await wire.reply(id, out ?? null);
        return;
      }
      const hook = record.hooks[method];
      if (typeof hook === "function") {
        const result = await hook(paramsObj, record.options);
        await wire.reply(id, result === undefined ? null : result);
        return;
      }
      // 无钩子的默认实现：静态模型表兜底，其余一律 unsupported
      if (method === "models.list" || method === "models.meta") {
        await wire.reply(id, { models: [...record.models] });
        return;
      }
      throw unsupported(`provider '${record.requestedId}' does not implement '${method}'`);
    } catch (err) {
      await wire.replyError(id, classifyError(err));
    }
  };
}

// ── conversation ─────────────────────────────────────────────────

/**
 * conversation 入口：脚本自定义钩子优先，否则默认管线。
 * 返回 STREAMED（已按流式收尾）或普通结果值（未用 stream 时的应答载荷）。
 */
async function runConversation(wire, record, params, rpcId) {
  const hook = record.hooks.conversation;
  if (typeof hook === "function") {
    // 脚本全权实现：给它绑好本次 RPC id 的发送器
    const streamState = { used: false, ended: false };
    const stream = makeStream(wire, rpcId, streamState);
    try {
      const result = await hook(params, record.options, stream);
      if (streamState.used) {
        if (!streamState.ended) await wire.sendEnd(rpcId);
        return STREAMED;
      }
      return result === undefined ? null : result;
    } catch (err) {
      if (streamState.used) {
        // head 已发：流错误帧终结（不能同时回 error 应答——双应答毁协议）
        await wire.sendStreamError(rpcId, classifyError(err));
        return STREAMED;
      }
      throw err;
    }
  }
  return runDefaultConversation(wire, record, params, rpcId);
}

/** 给脚本钩子的流式发送器（绑定当前请求的 RPC id）。 */
function makeStream(wire, rpcId, state) {
  return {
    head: async (status, headers, upstream) => {
      state.used = true;
      await wire.sendHead(rpcId, status, headers, upstream);
    },
    chunk: async (bytes) => {
      state.used = true;
      await wire.sendChunk(rpcId, bytes);
    },
    end: async () => {
      state.used = true;
      state.ended = true;
      await wire.sendEnd(rpcId);
    },
    error: async (err) => {
      state.used = true;
      state.ended = true;
      await wire.sendStreamError(rpcId, classifyError(err));
    },
  };
}

/**
 * 默认 conversation 管线。
 * 错误通道：head 未发 → throw（dispatcher 回 error 应答）；
 * head 已发 → 流错误帧 + STREAMED（绝不双应答）。
 */
async function runDefaultConversation(wire, record, params, rpcId) {
  const body = isPlainObject(params.body) ? params.body : {};
  const account = typeof params.account === "string" ? params.account : "";
  const upstream = record.upstream || "chat";
  let headSent = false;
  let session = null;

  try {
    // 1. 凭证：读账号（account 空 = 该 provider 队首）
    let secret = "";
    const cred = await wire.call(CALLBACK.storeGet, { provider: record.requestedId, account });
    if (isPlainObject(cred)) secret = typeof cred.secret === "string" ? cred.secret : "";

    // refresh 凭证：token.ensure 钩子可返回 {secret} 刷新（CAS 写回防覆盖）
    const ensureHook = record.hooks["token.ensure"];
    if (typeof ensureHook === "function" && secret) {
      const refreshed = await ensureHook(
        { account, secret, public: isPlainObject(cred) ? cred.public : null, body },
        record.options,
      );
      const nextSecret =
        isPlainObject(refreshed) && typeof refreshed.secret === "string" ? refreshed.secret : "";
      if (nextSecret && nextSecret !== secret) {
        const upd = await wire.call(CALLBACK.storeUpdate, {
          account,
          expect: secret,
          secret: nextSecret,
        });
        if (isPlainObject(upd) && upd.updated !== false) secret = nextSecret;
      }
    }

    // 2. 端点与请求体：非 chat 协议先让网关转换请求体
    const cfg = isPlainObject(record.config) ? record.config : {};
    const baseUrl = typeof cfg.base_url === "string" && cfg.base_url
      ? cfg.base_url
      : typeof cfg.baseUrl === "string"
        ? cfg.baseUrl
        : "";
    if (!baseUrl) {
      throw precondition(`provider '${record.requestedId}' config missing base_url`);
    }
    let outBody = body;
    let url = joinUrl(baseUrl, defaultChatPath(upstream, body.model));
    let headers = {
      "content-type": "application/json",
      accept: "text/event-stream",
      ...defaultAuthHeaders(upstream, secret),
    };
    if (upstream !== "chat") {
      const conv = await wire.call(CALLBACK.protocolRequest, { protocol: upstream, body });
      if (!isPlainObject(conv)) {
        throw unsupported(`protocol converter for '${upstream}' unavailable`);
      }
      if (isPlainObject(conv.body)) outBody = conv.body;
      if (typeof conv.path === "string" && conv.path) url = joinUrl(baseUrl, conv.path);
      if (isPlainObject(conv.headers)) headers = { ...headers, ...conv.headers };
    }

    // 脚本的 chat.headers 最后说话（函数或静态对象均可）
    if (record.chatHeaders !== null) {
      const extra =
        typeof record.chatHeaders === "function"
          ? await record.chatHeaders({ body: outBody, secret, upstream }, record.options)
          : record.chatHeaders;
      if (isPlainObject(extra)) {
        for (const [k, v] of Object.entries(extra)) {
          if (typeof v === "string" || typeof v === "number") headers[k.toLowerCase()] = String(v);
        }
      }
    }

    // 3. fetch 上游（经网关出口代理；body 以 b64 过线）
    const resp = await wire.call(CALLBACK.transportRequest, {
      method: "POST",
      url,
      headers,
      body_b64: b64(JSON.stringify(outBody)),
      stream: true,
    });
    if (!isPlainObject(resp)) {
      throw unsupported("transport egress unavailable (script.transport.request)");
    }
    const status = typeof resp.status === "number" ? resp.status : 200;
    session = typeof resp.session === "string" ? resp.session : null;
    const respHeaders = isPlainObject(resp.headers) ? resp.headers : {};

    if (status < 200 || status >= 300) {
      // 非 2xx：head(状态码) → 错误体字节 → end（网关 take_head 的约定——
      // 错误体让网关按上游错误形状解析，客户端不拿到半截 SSE 当错误）
      const errBytes = await drainSession(wire, session);
      headSent = true;
      await wire.sendHead(rpcId, status, respHeaders, upstream);
      if (errBytes.length > 0) await wire.sendChunk(rpcId, errBytes);
      await wire.sendEnd(rpcId);
      return STREAMED;
    }

    // 4. head：状态 + 上游响应头 + 自报协议
    headSent = true;
    await wire.sendHead(rpcId, status, respHeaders, upstream);

    // 5. SSE 循环：上游原始字节 →（非 chat 经网关转换）→ chunk 帧
    const convert = upstream !== "chat";
    let done = false;
    while (!done) {
      const r = await wire.call(CALLBACK.transportRead, { session });
      if (!isPlainObject(r)) {
        throw unsupported("transport egress unavailable (script.transport.read)");
      }
      if (r.done === true) break;
      if (typeof r.chunk_b64 !== "string" || r.chunk_b64.length === 0) continue;
      const raw = unb64(r.chunk_b64);
      let out = raw;
      if (convert) {
        const c = await wire.call(CALLBACK.protocolChunk, {
          protocol: upstream,
          stream: rpcId,
          data: b64(raw),
        });
        out = isPlainObject(c) && typeof c.data === "string" ? unb64(c.data) : raw;
      }
      if (out.length > 0) await wire.sendChunk(rpcId, out);
    }

    // 6. finish：转换器尾部冲刷（上游协议的收尾事件不落在 chunk 边界上）
    if (convert) {
      const f = await wire.call(CALLBACK.protocolFinish, { stream: rpcId });
      if (isPlainObject(f) && typeof f.data === "string" && f.data.length > 0) {
        const tail = unb64(f.data);
        if (tail.length > 0) await wire.sendChunk(rpcId, tail);
      }
    }
    await wire.sendEnd(rpcId);
    return STREAMED;
  } catch (err) {
    if (headSent) {
      await wire.sendStreamError(rpcId, classifyError(err));
      return STREAMED;
    }
    throw err;
  } finally {
    if (session) {
      try {
        await wire.call(CALLBACK.transportClose, { session });
      } catch {
        // 会话清理失败不影响本次调用的终态（网关空闲回收兜底）
      }
    }
  }
}

/** 收齐一个流式会话的剩余字节（错误体用；上限 1 MiB，超限继续丢弃到 EOF）。 */
async function drainSession(wire, session) {
  const CAP = 1 << 20;
  const out = [];
  if (!session) return Buffer.concat(out);
  for (;;) {
    let r;
    try {
      r = await wire.call(CALLBACK.transportRead, { session });
    } catch {
      break;
    }
    if (!isPlainObject(r) || r.done === true) break;
    if (typeof r.chunk_b64 === "string" && r.chunk_b64.length > 0) {
      const chunk = unb64(r.chunk_b64);
      if (out.reduce((n, b) => n + b.length, 0) < CAP) out.push(chunk);
    }
  }
  return Buffer.concat(out);
}
