/**
 * SSE 分帧层（唯一实现）。
 *
 * ── 为什么单独成文件 ────────────────────────────────────────
 * 此前「怎么把 HTTP 字节流切成事件」在两处各写了一遍：
 *   · `api/stream.ts` 的 `readSseResponse`（对话调试 / 上游直连）
 *   · `api/endpoints/prompts_public.ts` 的 `agentChatStream`（AI 运维）
 * 两份实现都只做 `buffer.indexOf('\n\n')` + 逐行 `startsWith('data:')`，
 * 于是同一批协议形态在两处各自丢数据（见下）。分帧是纯逻辑，收敛到本文件后
 * 可以完全离线驱动测试（假的 ReadableStream），不连真网络。
 *
 * ── 与 A2A v3.5.1 的口径对齐 ─────────────────────────────────
 * A2A `ui-shared/assistant-stream.ts` 的做法：
 *   · 按 `\r?\n\r?\n` 切帧，`data:` 多行用 `\n` 拼接后再 `JSON.parse`；
 *   · abort 抛 AbortError，`finally` 里 `reader.cancel()` + `releaseLock()`；
 *   · 「缺结束标记」不算错误（AIGX 这边更明确：Trae 上游根本不发 `[DONE]`）。
 * 本文件照抄的是**语义**（拼接、真断、不误报），不是它的 React 结构。
 *
 * ── 这里刻意不做的事 ───────────────────────────────────────
 * 不解析 JSON 业务形状、不判断「这一轮结束了吗」。那是 `api/stream.ts`
 * （OpenAI/Anthropic 增量）与 `agentChatStream`（AgentEvent）各自的职责，
 * 它们拿到的就是「一个事件 = event 名 + data 文本」。
 */

/** 单帧上限（字符）。超限的帧必须被丢弃并显式报错，不能让 buffer 无限增长。 */
export const SSE_MAX_FRAME_CHARS = 4 * 1024 * 1024;

/** 一个 SSE 事件（已按规范合并多行 data）。 */
export interface SseFrame {
  /** `event:` 字段；缺省为空串（= 默认事件类型 `message`） */
  event: string;
  /** `data:` 字段（多行按 `\n` 拼接）；缺省为空串 */
  data: string;
  /** `id:` 字段（供将来的 Last-Event-ID 重连续传使用，当前只透传） */
  id?: string;
  /** `retry:` 字段（毫秒） */
  retry?: number;
  /** true = 该帧因超过 `maxFrameChars` 被丢弃，data 里是给调用方看的说明 JSON */
  oversize?: boolean;
}

/**
 * SSE 分帧器：`feed(chunk)` 进、完整帧出，跨块状态保存在实例里。
 *
 * 处理的关键形态（每条都有 `stream.test.ts` 的用例）：
 *   · 半行 / 半帧（`data: {...` 被 TCP 切开）—— 缓冲到下一块；
 *   · 多行 `data:` —— 按规范用 `\n` 拼接成一个事件；
 *   · `event:` / `id:` / `retry:` 与 `data:` 交错 —— 字段各自归位；
 *   · 注释行 `: ping` / `: keepalive` —— 丢弃（AIGX 后端就用它保活）；
 *   · CRLF 与裸 CR 行尾，且 `\r` 恰好落在块边界 —— 用 pendingCR 暂存；
 *   · UTF-8 BOM —— 首块剥离；
 *   · 超长单帧 —— 丢弃并吐一个 error 帧（不让内存涨到 OOM）。
 */
export class SseFramer {
  private buffer = '';
  private pendingCR = false;
  private started = false;
  private readonly maxFrameChars: number;

  constructor(opts: { maxFrameChars?: number } = {}) {
    this.maxFrameChars = opts.maxFrameChars ?? SSE_MAX_FRAME_CHARS;
  }

  /** 推入一块文本，返回由此解锁的全部完整帧。 */
  feed(chunk: string): SseFrame[] {
    if (!chunk) return [];
    let text = chunk;
    if (!this.started) {
      this.started = true;
      if (text.charCodeAt(0) === 0xfeff) text = text.slice(1);
    }
    if (this.pendingCR) {
      text = `\r${text}`;
      this.pendingCR = false;
    }
    // `\r` 落在块尾时无法判断下一块是否以 `\n` 开头（CRLF 被切开），
    // 先扣住这个字符，下一块再合并 —— 否则会把一个 CRLF 当成两个换行。
    if (text.endsWith('\r')) {
      this.pendingCR = true;
      text = text.slice(0, -1);
    }
    // SSE 规范允许 \n、\r\n、\r 三种行尾，统一成 \n 之后只需找 \n\n
    this.buffer += text.replace(/\r\n/g, '\n').replace(/\r/g, '\n');
    return this.drain();
  }

  /**
   * 流结束时收尾：把缓冲里没有 `\n\n` 结尾的最后一帧也吐出来。
   * 上游正常都会以空行收尾，但连接被中途关闭时最后一帧常常缺终结符 ——
   * 丢掉它等于丢掉最后一段正文。
   */
  flush(): SseFrame[] {
    const out: SseFrame[] = [];
    if (this.pendingCR) {
      this.pendingCR = false;
      this.buffer += '\n';
    }
    const rest = this.buffer;
    this.buffer = '';
    if (rest.trim()) {
      const frame = parseBlock(rest);
      if (frame) out.push(frame);
    }
    return out;
  }

  private drain(): SseFrame[] {
    const out: SseFrame[] = [];
    for (;;) {
      const idx = this.buffer.indexOf('\n\n');
      if (idx === -1) break;
      const block = this.buffer.slice(0, idx);
      this.buffer = this.buffer.slice(idx + 2);
      const frame = parseBlock(block);
      if (frame) out.push(frame);
    }
    if (this.buffer.length > this.maxFrameChars) {
      const dropped = this.buffer.length;
      this.buffer = '';
      out.push({
        event: 'error',
        oversize: true,
        data: JSON.stringify({
          error: {
            type: 'sse_frame_too_large',
            message: `SSE 单帧超过 ${this.maxFrameChars} 字符上限，已丢弃该帧（${dropped} 字符）`,
          },
        }),
      });
    }
    return out;
  }
}

/** 把一个块（一个或多个完整帧）解析成帧数组；不完整的尾部按 `flush` 语义收尾。 */
export function splitSseFrames(text: string): SseFrame[] {
  const framer = new SseFramer();
  return [...framer.feed(text), ...framer.flush()];
}

/** 解析单个事件块（不含终结符空行）。 */
function parseBlock(block: string): SseFrame | null {
  let event = '';
  let id: string | undefined;
  let retry: number | undefined;
  const dataLines: string[] = [];
  let sawField = false;
  for (const raw of block.split('\n')) {
    if (!raw) continue;
    // 注释行（心跳 / keepalive）：规范要求解析器忽略
    if (raw.startsWith(':')) continue;
    const colon = raw.indexOf(':');
    const field = colon === -1 ? raw : raw.slice(0, colon);
    let value = colon === -1 ? '' : raw.slice(colon + 1);
    // 规范：字段值前的一个空格不属于值
    if (value.startsWith(' ')) value = value.slice(1);
    switch (field) {
      case 'data':
        dataLines.push(value);
        sawField = true;
        break;
      case 'event':
        event = value;
        sawField = true;
        break;
      case 'id':
        id = value;
        sawField = true;
        break;
      case 'retry': {
        const ms = Number(value);
        if (Number.isFinite(ms) && ms >= 0) retry = ms;
        sawField = true;
        break;
      }
      default:
        // 未知字段按规范忽略
        break;
    }
  }
  // 纯注释块（`: keepalive`）不构成事件
  if (!sawField && !dataLines.length) return null;
  return {
    event,
    data: dataLines.join('\n'),
    ...(id !== undefined ? { id } : {}),
    ...(retry !== undefined ? { retry } : {}),
  };
}

/** 统一的 abort 判定：调用方据此区分「用户停止」与真故障。 */
export function isAbortError(err: unknown): boolean {
  if (!err || typeof err !== 'object') return false;
  const name = (err as { name?: unknown }).name;
  return name === 'AbortError' || name === 'Aborted';
}

function abortError(): Error {
  return new DOMException('Aborted', 'AbortError');
}

export interface ReadSseBodyOptions {
  /** 每个完整帧回调一次（abort 之后不再回调） */
  onFrame: (frame: SseFrame) => void;
  signal?: AbortSignal;
  /** 返回 true 时立即停止读取并断开底层流（例如已收到 `[DONE]`） */
  stopWhen?: () => boolean;
  /** 覆盖单帧上限（测试用） */
  maxFrameChars?: number;
}

/**
 * 驱动一个 SSE 响应体，逐帧回调。
 *
 * 与旧实现（`reader.read()` 循环 + 就地切帧）的差别，都是能复现的缺陷：
 *   1. **真断**：abort / 异常 / 正常结束都会 `reader.cancel()` + `releaseLock()`。
 *      旧实现从不 cancel —— abort 之后浏览器里那条连接要等到 GC 才关，
 *      上游继续生成、继续计费。
 *   2. **abort 之后零回调**：每次 `read()` 落地后都再查一次 signal，
 *      避免「卸载前最后一读」把增量写进已卸载组件（React 里就是白跑一次 setState）。
 *   3. **EOF 收尾**：`decoder.decode()` 冲刷未成形的多字节，`framer.flush()`
 *      吐出缺终结符的最后一帧。
 *   4. **提前停**：`stopWhen` 为真时立刻停读并 cancel，不再空转到上游关闭。
 */
export async function readSseBody(
  body: ReadableStream<Uint8Array>,
  options: ReadSseBodyOptions,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder('utf-8');
  const framer = new SseFramer(
    options.maxFrameChars === undefined ? {} : { maxFrameChars: options.maxFrameChars },
  );
  try {
    for (;;) {
      if (options.signal?.aborted) throw abortError();
      if (options.stopWhen?.()) break;
      const { done, value } = await reader.read();
      // read 期间被 abort：这一块不算数（否则就是「卸载后 setState」）
      if (options.signal?.aborted) throw abortError();
      if (done) {
        const tail = decoder.decode();
        if (tail) {
          for (const frame of framer.feed(tail)) options.onFrame(frame);
        }
        for (const frame of framer.flush()) options.onFrame(frame);
        break;
      }
      for (const frame of framer.feed(decoder.decode(value, { stream: true }))) {
        options.onFrame(frame);
      }
    }
  } finally {
    // 无论怎么离开（正常结束 / abort / 解析抛错）都要把底层流关掉。
    // 已关闭或已取消时 cancel 会 reject，忽略即可 —— 这里的目的是释放连接，
    // 不是报告错误。
    try {
      await reader.cancel();
    } catch {
      /* 已关闭/已取消：无需处理 */
    }
    try {
      reader.releaseLock();
    } catch {
      /* 同上 */
    }
  }
}
