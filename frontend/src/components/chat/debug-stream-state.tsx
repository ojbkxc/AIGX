/**
 * 对话调试器（ChatDebugger）流式状态机 —— 纯函数层（可单测，不测渲染）。
 *
 * 抽出的原因：`ChatDebugger.tsx` 的 `onDelta` / 停止 / 收尾三处各自手改
 * `messages` 数组，判据（占位「…」、isEnd 尾巴、工具分片聚合）散在 JSX 组件里，
 * 出错只能靠肉眼点界面发现。这里把「一帧增量 → 新消息数组」收敛成单点，
 * 组件只负责把结果塞进 setState。
 *
 * 三条修过的缺陷（均有 test 复现）：
 * 1. `isEnd` 帧的 `content` 被整帧丢弃（`patch.content = delta.isEnd ? base : base + delta.content`）
 *    —— 非流式降级/直连上游会把整段正文放在 isEnd 帧里（`readSseResponse` 的
 *    `onDelta({ content, isEnd: true })`），于是界面上是「上游未返回内容」。
 * 2. 停滞（stalled）无法退出：`onDelta` 闭包捕获的是首次渲染的 `streamPhase`，
 *    `streamPhase === 'stalled'` 分支永远为假，一旦标红就再不回「正在生成」。
 * 3. 时间戳/耗时按**数组下标**存取，删除消息或重新生成截断后整体错位
 *    （见 debug-message-meta.tsx）。
 */
import type { ChatStreamDelta } from '../../types';
import { mergeToolCallDelta } from '../../lib/toolCallAdapter';
import type { DebugMessage } from './types';

export type DebugStreamPhase = 'thinking' | 'generating' | 'stalled';

/** 流式占位文本（首帧前气泡里显示的内容） */
export const STREAM_PLACEHOLDER = '…';

/** 停滞判定的默认静默时长（与 ChatDebugger 原实现一致：3s） */
export const STALL_AFTER_MS = 3000;

/**
 * 把一帧增量并入最后一条 assistant 气泡，返回**新数组**（不修改入参）。
 *
 * 判据顺序（与后端帧形状一一对应，见 `api/stream.ts`）：
 * - `kind === 'reasoning'` → 拼进 `reasoning`（折叠面板），正文不动；
 * - `kind === 'tool_call'` → 按 index 聚合进 `toolCalls`（唯一合并点仍是
 *   `lib/toolCallAdapter.mergeToolCallDelta`，这里不复制第二套拼接逻辑）；
 * - `kind === 'usage'` → 覆盖式写入 `usage`；
 * - `kind === 'error'` → 「⚠️」尾巴（已有正文时追加，不吞已流出的内容）；
 * - 其余（含 `kind` 缺省的普通增量）→ 正文追加；**isEnd 也追加**。
 *
 * 末尾不是 assistant 气泡（占位还没建起来）时原样返回：调用方按需新起一条，
 * 本函数不替它决定 UI 形状。
 */
export function applyStreamDelta(
  messages: DebugMessage[],
  delta: ChatStreamDelta,
): DebugMessage[] {
  if (!messages.length) return messages;
  const last = messages[messages.length - 1];
  if (last.role !== 'assistant') return messages;

  const next = messages.slice();
  const patch: DebugMessage = { ...last };
  const base = last.content === STREAM_PLACEHOLDER ? '' : last.content;

  if (delta.kind === 'reasoning') {
    patch.reasoning = (last.reasoning ?? '') + delta.content;
  } else if (delta.kind === 'tool_call' && delta.toolCall) {
    patch.toolCalls = mergeToolCallDelta(last.toolCalls ?? [], delta.toolCall);
    patch.toolState = 'streaming';
    // 工具分片帧不带正文；占位保持原样等正文帧来清
  } else if (delta.kind === 'usage' && delta.usage) {
    patch.usage = delta.usage;
  } else if (delta.kind === 'error') {
    patch.content = base ? `${base}\n\n⚠️ ${delta.content}` : `⚠️ ${delta.content}`;
    if (patch.toolCalls?.length) patch.toolState = 'error';
  } else {
    // 正文增量（含 isEnd 帧）：一律追加，不再按 isEnd 丢弃内容
    patch.content = base + delta.content;
  }

  next[next.length - 1] = patch;
  return next;
}

/**
 * 流式正常收尾：工具卡片切「完成」；空流兜底给一条可见提示。
 *
 * `emptyHint` 由调用方传入已翻译文案（本模块不引入 i18n，便于纯函数测试）。
 */
export function finishStream(
  messages: DebugMessage[],
  emptyHint: string,
): DebugMessage[] {
  if (!messages.length) return messages;
  const last = messages[messages.length - 1];
  if (last.role !== 'assistant') return messages;
  const next = messages.slice();
  if (last.toolCalls?.length) {
    next[next.length - 1] = { ...last, toolState: 'done' };
  } else if (!last.reasoning && (last.content === STREAM_PLACEHOLDER || !last.content.trim())) {
    next[next.length - 1] = { ...last, content: `⚠️ ${emptyHint}` };
  }
  return next;
}

/**
 * 用户主动停止：在**同一只气泡**里标中止（#37），不新起「⏹」气泡。
 *
 * 停在占位阶段时把「…」清成空串（占位没有信息量，气泡只剩中止标记）。
 * 末尾不是 assistant（占位还没建起来就被停掉）时新起一条 `stoppedText`。
 */
export function markInterrupted(
  messages: DebugMessage[],
  stoppedText: string,
): DebugMessage[] {
  const next = messages.slice();
  const last = next[next.length - 1];
  if (last && last.role === 'assistant') {
    const blank = last.content === STREAM_PLACEHOLDER || !last.content.trim();
    next[next.length - 1] = {
      ...last,
      ...(blank ? { content: '' } : {}),
      toolState: 'interrupted',
    };
    return next;
  }
  next.push({ role: 'assistant', content: `⏹ ${stoppedText}` });
  return next;
}

/**
 * 一次失败只留一条可见错误：末尾是占位/错误/停止气泡时就地替换，
 * 已收到工具分片时标错误态（保留部分参数供排查）。
 */
export function applyStreamError(messages: DebugMessage[], msg: string): DebugMessage[] {
  const next = messages.slice();
  const last = next[next.length - 1];
  const transient = (s: string | undefined): boolean => {
    const v = (s ?? '').trim();
    return v === '' || v === STREAM_PLACEHOLDER || v.startsWith('⚠️') || v.startsWith('⏹');
  };
  if (last && last.role === 'assistant' && transient(last.content)) {
    next[next.length - 1] = {
      ...last,
      content: `⚠️ ${msg}`,
      ...(last.toolCalls?.length ? { toolState: 'error' as const } : {}),
    };
  } else if (last && last.role === 'assistant' && last.toolCalls?.length) {
    next[next.length - 1] = { ...last, toolState: 'error' };
  } else {
    next.push({ role: 'assistant', content: `⚠️ ${msg}` });
  }
  return next;
}

/**
 * 下一个阶段值。
 *
 * 修的是「停滞后再也不回生成态」：原实现在闭包里读 `streamPhase === 'stalled'`
 * 做恢复判断，而闭包捕获的是挂载时的值，恒为 'thinking'。这里改成**任何一帧
 * 增量都直接回 generating**（帧到了就是没停滞），阶段推进只依赖「收到帧」本身。
 */
export function nextStreamPhaseOnDelta(): DebugStreamPhase {
  return 'generating';
}

/**
 * 停滞判定：距最后一次增量超过阈值即为 stalled，否则保持当前阶段。
 *
 * 与 `StreamPhase`（chat 组件，属 shell 成员）的语义保持一致：只在已经进入
 * 生成态后才可能停判断；「思考中」阶段不计停滞（首帧本来就慢）。
 */
export function tickStreamPhase(
  current: DebugStreamPhase,
  lastDeltaAt: number,
  now: number,
  stallAfterMs: number = STALL_AFTER_MS,
): DebugStreamPhase {
  if (current === 'thinking') return current;
  if (lastDeltaAt <= 0) return current;
  return now - lastDeltaAt > stallAfterMs ? 'stalled' : 'generating';
}

/**
 * 停止/失败后是否应该把阶段复位（避免下次发送沿用上一轮的 stalled）。
 */
export function resetStreamPhase(): DebugStreamPhase {
  return 'thinking';
}

/**
 * 一次新请求开始时的阶段初值（阶段 + 停滞计时起点）。
 *
 * 这是给**宿主页**（`pages/Agent.tsx` 用共享 `components/chat/StreamPhase`）的判据：
 * 那里 `lastEventAt` 只在收到 SSE 事件时更新，发送时不重置。上一轮结束 5 分钟后
 * 再发新请求，`StreamPhaseIndicator` 的停滞判定会拿「5 分钟前的旧时间戳」算，
 * 于是新请求一开始就显示「响应似乎停滞了（仍在等待上游）…」，直到首个事件到达
 * 才恢复——用户以为上游卡了，其实请求刚发出。
 *
 * 用法（Agent.tsx 的 send() 里，两行）：
 * ```ts
 * const seed = seedStreamPhase();
 * setStreamPhase(seed.phase);
 * setLastEventAt(seed.lastEventAt);
 * ```
 */
export function seedStreamPhase(now: number = Date.now()): {
  phase: DebugStreamPhase;
  lastEventAt: number;
} {
  return { phase: 'thinking', lastEventAt: now };
}
