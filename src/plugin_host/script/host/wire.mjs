// NDJSON wire 层 —— AIGX 自研实现（上游 AGPL，仅按协议契约独立书写）。
//
// 与 src/plugin_host/rpc.rs 是同一套线契约的两侧实现（常量逐字一致，
// 改一边必须同步另一边）：
//
// - 一行一帧 JSON：
//     {"id":7,"method":"conversation","params":{…}}   网关 → 宿主 请求（奇数 id）
//     {"id":7,"result":{…}} / {"id":7,"error":{…}}    应答
//     {"id":7,"event":"head","status":200,…}          流式事件帧
//     {"id":0,"method":"log","params":{…}}            通知（无应答）
// - id 奇偶：网关发请求用奇数；本宿主回调网关用偶数（从 2 起步）；0 通知。
//   回调必须用偶数 id——网关读循环按「应答 id 在自己的 pending 表」路由，
//   奇数会撞上网关自己的请求 id 空间，应答会被错配。
// - credit 背压：每条流初始窗口 DEFAULT_WINDOW_BYTES；宿主每发一个 chunk
//   按「载荷 + FRAME_OVERHEAD_BYTES」扣减；窗口不够就停发等 credit。
//   网关消费后发 {"id":<新分配奇数>,"method":"credit","params":{"id":<流id>,"n":<字节>}}
//   归还额度（归还量是「实际未归还的消费量」不是固定半窗，见 rpc.rs）。
//   本宿主收到 credit：把 n 加回流窗口 + 回 {"id":<credit 请求 id>,"result":null}
//   （上游 SDK 同形态；网关其实不等这份应答，丢了不影响）。
// - 入站行超 MAX_LINE_BYTES = 致命：直接退出进程（与网关读循环同处置——
//   「这一帧不合法，两边继续没有意义」）。
//
// 出站回调并发上限（防回调风暴拖垮网关，任务契约的「in-flight 上限」）：
// - normal（script.transport.* / script.protocol.* 等网络面）128
// - control（store.* / settings.* / log / notify）160
//   达到上限的调用排队等槽位（30s 等不到按失败处理，不无限挂起）。

export const MAX_LINE_BYTES = 128 * 1024 * 1024;
export const MAX_CHUNK_BYTES = 64 * 1024;
export const DEFAULT_WINDOW_BYTES = 512 * 1024;
export const FRAME_OVERHEAD_BYTES = 2048;
export const MAX_INFLIGHT_NORMAL = 128;
export const MAX_INFLIGHT_CONTROL = 160;

/** 等回调槽位的超时（上限是防御，正常永远等得到）。 */
const SLOT_WAIT_TIMEOUT_MS = 30_000;

/** 控制类回调方法（其余一律按网络面 normal 计）。 */
const CONTROL_METHODS = new Set(["log", "notify"]);
const CONTROL_PREFIXES = ["store.", "settings."];

/** 回调方法 → 槽位类别。 */
export function isControlMethod(method) {
  if (CONTROL_METHODS.has(method)) return true;
  return CONTROL_PREFIXES.some((p) => method.startsWith(p));
}

/** 把二进制切成 ≤MAX_CHUNK_BYTES 的块（发送方先切分的约定）。 */
function splitChunks(bytes) {
  const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
  if (buf.length <= MAX_CHUNK_BYTES) return [buf];
  const out = [];
  for (let off = 0; off < buf.length; off += MAX_CHUNK_BYTES) {
    out.push(buf.subarray(off, Math.min(off + MAX_CHUNK_BYTES, buf.length)));
  }
  return out;
}

/**
 * wire 通道：stdin 读循环 + 帧路由 + 回调表 + 流窗口。
 *
 * 生命周期：构造 → 挂 onRequest/onNotify → start() →（网关发 init）。
 * EOF（网关关 stdin）= 关机信号：整个进程直接退出。
 */
export class Wire {
  // 私有状态（类体声明：私有字段必须先声明后赋值）
  #nextCallbackId;
  #pending;
  #inflight;
  #slotWaiters;
  #windows;
  #creditWaiters;
  #writeChain;
  #started;

  constructor() {
    /** 回调偶数 id 计数器（网关奇数、宿主偶数、0 通知——硬契约）。 */
    this.#nextCallbackId = 2;
    /** 出站回调在途表：id → {resolve, reject}。 */
    this.#pending = new Map();
    /** 槽位占用与等待队列（normal / control 两类分开计）。 */
    this.#inflight = { normal: 0, control: 0 };
    this.#slotWaiters = { normal: [], control: [] };
    /** 流发送窗口：streamId → 剩余字节（初始 DEFAULT_WINDOW_BYTES）。 */
    this.#windows = new Map();
    /** 等窗口的续发回调：streamId → [fn]。 */
    this.#creditWaiters = new Map();
    /** 出站写串行链：帧必须整行原子写出，不能交错。 */
    this.#writeChain = Promise.resolve();
    this.onRequest = null;
    this.onNotify = null;
    this.onEvent = null;
    this.#started = false;
  }

  // ── 出站：请求 / 应答 / 通知 ─────────────────────────────────────

  /**
   * 发一个回调请求给网关（偶数 id）并等应答。
   *
   * opts.timeoutMs 可选；不传则不设超时（transport.read 这类长挂调用
   * 依赖网关自身的空闲回收，宿主再设超时只会误杀）。
   */
  async call(method, params, opts = undefined) {
    const kind = isControlMethod(method) ? "control" : "normal";
    await this.#acquireSlot(kind);
    const id = this.#nextCallbackId;
    this.#nextCallbackId += 2;
    let entry;
    const promise = new Promise((resolve, reject) => {
      entry = { resolve, reject };
      this.#pending.set(id, entry);
    });
    try {
      await this.writeLine({ id, method, params: params === undefined ? null : params });
      if (opts && opts.timeoutMs > 0) {
        return await this.#withTimeout(promise, opts.timeoutMs, `callback ${method} timeout`);
      }
      return await promise;
    } finally {
      if (this.#pending.get(id) === entry) this.#pending.delete(id);
      this.#releaseSlot(kind);
    }
  }

  /** 成功应答网关的请求（result 可以是 null）。写失败不炸：网关死了说什么都没用。 */
  async reply(id, result) {
    try {
      await this.writeLine({ id, result: result === undefined ? null : result });
    } catch (err) {
      this.#logStderr(`reply(${id}) 写失败：${describe(err)}`);
    }
  }

  /** 失败应答网关的请求（err 是 classifyError 的产出 {message,code,status?}）。 */
  async replyError(id, err) {
    const error = { message: err?.message ?? "internal error", code: err?.code ?? "internal" };
    if (typeof err?.status === "number") error.status = err.status;
    try {
      await this.writeLine({ id, error });
    } catch (e) {
      this.#logStderr(`replyError(${id}) 写失败：${describe(e)}`);
    }
  }

  /** 流中段错误帧：head 已发后的失败通道（网关把 error 应答投进流通道作终结）。 */
  async sendStreamError(id, err) {
    const error = { message: err?.message ?? "internal error", code: err?.code ?? "internal" };
    if (typeof err?.status === "number") error.status = err.status;
    try {
      await this.writeLine({ id, error });
    } catch (e) {
      this.#logStderr(`sendStreamError(${id}) 写失败：${describe(e)}`);
    }
  }

  /** 日志通知（id=0，无应答；网关 tracing 直收）。 */
  notifyLog(level, message) {
    this.writeLine({ id: 0, method: "log", params: { level, message } }).catch(() => {});
  }

  /** 启动事件（加载阶段的装载摘要，网关侧按事件帧观察）。 */
  sendStartupEvent(message) {
    this.writeLine({ id: 0, event: "startup", message }).catch(() => {});
  }

  // ── 出站：流式（head → chunk×N → end）──────────────────────────

  /** 流的第一帧：上游状态码 + 响应头（小写键）+ 自报协议。 */
  async sendHead(id, status, headers, upstream) {
    const frame = { id, event: "head", status: status ?? 200, headers: headers ?? {} };
    if (upstream) frame.upstream = upstream;
    await this.writeLine(frame);
  }

  /**
   * 发一段流字节（自动按 MAX_CHUNK_BYTES 切分 + 信用窗口背压）。
   *
   * 每块计费 = 载荷 + FRAME_OVERHEAD_BYTES，与网关 StreamCall 的消费计费
   * 同口径——差一个常量两边窗口就会缓慢泄漏，背压形同虚设。窗口不够时
   * 挂起等 credit（背压是常态不是错误，不设超时：网关死了 stdin EOF
   * 会杀掉整个宿主）。
   */
  async sendChunk(id, bytes) {
    if (!this.#windows.has(id)) this.#windows.set(id, DEFAULT_WINDOW_BYTES);
    for (const piece of splitChunks(bytes)) {
      await this.#waitForWindow(id, piece.length);
      const rest = this.#windows.get(id) - piece.length - FRAME_OVERHEAD_BYTES;
      this.#windows.set(id, rest);
      await this.writeLine({
        id,
        event: "chunk",
        data: Buffer.from(piece).toString("base64"),
      });
    }
  }

  /** 流正常收尾。摘除窗口并唤醒所有等 credit 的挂起者（不会有下一块了）。 */
  async sendEnd(id) {
    this.#windows.delete(id);
    this.#wakeCreditWaiters(id);
    try {
      await this.writeLine({ id, event: "end" });
    } catch (err) {
      this.#logStderr(`sendEnd(${id}) 写失败：${describe(err)}`);
    }
  }

  // ── 入站读循环 ─────────────────────────────────────────────────

  /** 启动 stdin 读循环（不 await：主模块继续做 init 前的准备）。 */
  start() {
    if (this.#started) return;
    this.#started = true;
    void this.#readLoop();
  }

  async #readLoop() {
    // 按字节攒行（不整块转字符串：128MiB 的参数帧不能无谓翻倍内存）。
    const parts = [];
    let pendingBytes = 0;
    try {
      for await (const piece of process.stdin) {
        const buf = typeof piece === "string" ? Buffer.from(piece, "utf8") : piece;
        let start = 0;
        for (;;) {
          const idx = buf.indexOf(0x0a, start);
          if (idx < 0) break;
          parts.push(buf.subarray(start, idx));
          const line = parts.length === 1 ? parts[0] : Buffer.concat(parts);
          parts.length = 0;
          pendingBytes = 0;
          start = idx + 1;
          this.#handleLine(line);
        }
        const rest = buf.subarray(start);
        if (rest.length > 0) {
          parts.push(rest);
          pendingBytes += rest.length;
          if (pendingBytes > MAX_LINE_BYTES) {
            this.#fatal(`入站行超过 MAX_LINE_BYTES(${MAX_LINE_BYTES})`);
            return;
          }
        }
      }
    } catch (err) {
      this.#logStderr(`stdin 读循环异常：${describe(err)}`);
      process.exit(1);
    }
    // EOF：网关关掉了 stdin = 关机信号。
    process.exit(0);
  }

  #handleLine(line) {
    const text = line.toString("utf8").trim();
    // 空行跳过（网关同款）
    if (text.length === 0) return;
    let frame;
    try {
      frame = JSON.parse(text);
    } catch (err) {
      // 坏帧不致命：报日志继续（与网关读循环同处置）
      this.notifyLog("warn", `invalid frame: ${describe(err)}`);
      return;
    }
    if (!frame || typeof frame !== "object") return;

    if (frame.method !== undefined && frame.method !== null) {
      if (frame.id === 0) {
        // 通知（无应答）
        if (this.onNotify) this.onNotify(frame);
        return;
      }
      if (frame.method === "credit") {
        // credit 是传输层职责，不经 dispatch
        void this.#handleCredit(frame);
        return;
      }
      // 网关请求：异步分发，不让一个 handler 的崩溃带走读循环
      Promise.resolve()
        .then(() => this.onRequest?.(frame))
        .catch((err) => this.#logStderr(`request dispatch crash: ${describe(err)}`));
      return;
    }
    if (frame.result !== undefined || frame.error !== undefined) {
      const entry = this.#pending.get(frame.id);
      if (!entry) return; // 无人认领的应答（credit 回执等）：忽略
      this.#pending.delete(frame.id);
      if (frame.error !== undefined && frame.error !== null) {
        const e = new Error(frame.error.message || "callback failed");
        if (typeof frame.error.code === "string") e.code = frame.error.code;
        entry.reject(e);
      } else {
        entry.resolve(frame.result === undefined ? null : frame.result);
      }
      return;
    }
    if (frame.event !== undefined) {
      if (this.onEvent) this.onEvent(frame);
      return;
    }
    // Unknown 帧：忽略（向前兼容）
  }

  /** credit 请求：窗口归还 + 应答（网关不等这份应答，写失败无所谓）。 */
  async #handleCredit(frame) {
    const params = frame.params ?? {};
    const streamId = Number(params.id);
    const n = Number(params.n);
    if (Number.isFinite(streamId) && Number.isFinite(n) && n > 0) {
      const cur = this.#windows.get(streamId) ?? 0;
      this.#windows.set(streamId, cur + n);
      this.#wakeCreditWaiters(streamId);
    }
    try {
      await this.writeLine({ id: frame.id, result: null });
    } catch {
      // 网关不等待 credit 应答；写失败让它去
    }
  }

  #waitForWindow(streamId, need) {
    const cur = this.#windows.get(streamId) ?? 0;
    if (cur >= need) return Promise.resolve();
    return new Promise((resolve) => {
      let waiters = this.#creditWaiters.get(streamId);
      if (!waiters) {
        waiters = [];
        this.#creditWaiters.set(streamId, waiters);
      }
      waiters.push(resolve);
    });
  }

  #wakeCreditWaiters(streamId) {
    const waiters = this.#creditWaiters.get(streamId);
    if (!waiters) return;
    this.#creditWaiters.delete(streamId);
    for (const w of waiters) {
      try {
        w();
      } catch {
        // 唤醒回调不应抛
      }
    }
  }

  // ── 出站写（串行 + drain 背压）────────────────────────────────

  /**
   * 写一帧（整行原子出）。串行链保证多协程并发写不交错；
   * write() 返回 false 时等 drain 再继续（内核缓冲满的背压）。
   */
  async writeLine(obj) {
    const line = JSON.stringify(obj);
    // 出站大小防线：超限帧网关必判致命杀进程，宁可当场失败
    if (Buffer.byteLength(line, "utf8") + 1 > MAX_LINE_BYTES) {
      throw new Error(`outbound frame exceeds MAX_LINE_BYTES (${line.length} chars)`);
    }
    const prev = this.#writeChain;
    let release;
    this.#writeChain = new Promise((r) => {
      release = r;
    });
    await prev.catch(() => {});
    try {
      await new Promise((resolve, reject) => {
        let settled = false;
        const finish = (err) => {
          if (settled) return;
          settled = true;
          process.stdout.removeListener("drain", onDrain);
          if (err) reject(err);
          else resolve();
        };
        const onDrain = () => finish(undefined);
        const ok = process.stdout.write(line + "\n", "utf8", (err) => finish(err || undefined));
        if (ok) finish(undefined);
        else process.stdout.once("drain", onDrain);
      });
    } finally {
      release();
    }
  }

  // ── 槽位（出站回调并发上限）────────────────────────────────────

  async #acquireSlot(kind) {
    const cap = kind === "control" ? MAX_INFLIGHT_CONTROL : MAX_INFLIGHT_NORMAL;
    if (this.#inflight[kind] < cap) {
      this.#inflight[kind] += 1;
      return;
    }
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        const i = this.#slotWaiters[kind].indexOf(entry);
        if (i >= 0) this.#slotWaiters[kind].splice(i, 1);
        reject(new Error(`callback slot saturated (${kind}) wait timeout`));
      }, SLOT_WAIT_TIMEOUT_MS);
      const entry = () => {
        clearTimeout(timer);
        this.#inflight[kind] += 1;
        resolve();
      };
      this.#slotWaiters[kind].push(entry);
    });
  }

  #releaseSlot(kind) {
    this.#inflight[kind] -= 1;
    const next = this.#slotWaiters[kind].shift();
    if (next) next();
  }

  #withTimeout(promise, ms, message) {
    let timer;
    const timeout = new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(message)), ms);
    });
    return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
  }

  #logStderr(message) {
    console.error(`[script-host] ${message}`);
  }

  #fatal(message) {
    console.error(`[script-host] fatal: ${message}`);
    process.exit(1);
  }
}

/** 任何抛出物的一句话描述（本模块内部用；错误细节归 errors.mjs）。 */
function describe(err) {
  if (err instanceof Error) return err.message;
  try {
    return JSON.stringify(err) ?? String(err);
  } catch {
    return String(err);
  }
}
