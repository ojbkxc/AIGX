/**
 * 消息操作的可用判据（对齐 A2A v3.5.1 `assistant-message-operations.ts` 与
 * `ui-shared/assistant-message-actions.tsx`，补齐 AIGX 的复制/重生成/朗读）。
 *
 * ── A2A 的判据（照抄的部分）────────────────────────────────
 * ① 任何操作都先看 `busy()`：编辑 / 删除 / 分叉在生成中一律不执行（不是
 *    「灰着但还能点」，而是直接 return —— 生成中改历史会让流式回灌写回错位置）；
 * ② 用户消息只给「编辑 + 删除」，助手消息才给「分叉」（分叉语义是「从这条
 *    起复制」，只有助手消息是完整一轮的边界）；
 * ③ 流式中的助手消息不给任何操作（`message.streaming`）；
 * ④ 删除要二次确认，且确认文案必须写明「关联的工具结果会一并删除」。
 *
 * ── AIGX 独有的部分（单列，见交付清单）──────────────────────
 * 复制、重新生成、朗读（TTS）、源码视图切换。判据在这里统一，避免 ChatBubble
 * 里 `isLast &&` / `!busy &&` 散落成四五个条件。
 */

export type MessageRole = 'user' | 'assistant';

/** 一条消息的最小特征（渲染层手里有更多字段，这里只取判据用得到的） */
export interface MessageActionInput {
  role: MessageRole;
  /** 是否流式中（占位气泡 / 正在追加） */
  streaming?: boolean;
  /** 正文（去空白后判空：空消息不给复制与重生成） */
  content?: string;
  /** 是否是当前会话的最后一条 */
  isLast?: boolean;
  /** 是否已经拉取到工具轨迹 / 工具调用（删除时确认文案要提它） */
  hasToolCalls?: boolean;
  /** 会话级忙碌（生成中 / 审批等待中 / 历史操作中） */
  busy?: boolean;
  /** 已有朗读在播（AIGX 独有：同一时刻只播一条） */
  speaking?: boolean;
}

export interface MessageActions {
  copy: boolean;
  edit: boolean;
  regenerate: boolean;
  remove: boolean;
  fork: boolean;
  speak: boolean;
  source: boolean;
  /** 不可用的原因（渲染层可用于 title/aria；结构化码，非文案） */
  blocked: {
    copy?: 'streaming' | 'empty';
    edit?: 'streaming' | 'busy' | 'role';
    regenerate?: 'streaming' | 'busy' | 'not-last' | 'empty' | 'role';
    remove?: 'streaming' | 'busy';
    fork?: 'streaming' | 'busy' | 'role' | 'empty';
    speak?: 'streaming' | 'empty' | 'role';
  };
}

/**
 * 一条消息上每个操作能不能点。
 *
 * 「空消息」不只是体验问题：占位气泡（`content === ''`）在流式期间一直存在，
 * 给它挂「复制」「重生成」会在复制出空串、或对空回复发起一次新请求（白花钱）。
 */
export function messageActions(input: MessageActionInput): MessageActions {
  const text = String(input.content ?? '').trim();
  const empty = text.length === 0;
  const streaming = input.streaming === true;
  const busy = input.busy === true;
  const isAssistant = input.role === 'assistant';
  const blocked: MessageActions['blocked'] = {};

  const copy = !streaming && !empty;
  if (!copy) blocked.copy = streaming ? 'streaming' : 'empty';

  const edit = input.role === 'user' && !streaming && !busy;
  if (!edit) {
    blocked.edit = input.role === 'user' ? (streaming ? 'streaming' : 'busy') : 'role';
  }

  // 重生成只对「最后一条助手消息且有正文」成立：中间那条重生成会与后面的
  // 历史冲突（AIGX 的现有实现是 `isLast && !busy`，这里补「非空」与角色判定）
  const regenerate = isAssistant && !streaming && !busy && input.isLast === true && !empty;
  if (!regenerate) {
    blocked.regenerate = !isAssistant
      ? 'role'
      : streaming
        ? 'streaming'
        : busy
          ? 'busy'
          : input.isLast !== true
            ? 'not-last'
            : 'empty';
  }

  // 删除：生成中不给（会把流式回灌写回已删的下标）；其余情况允许
  const remove = !streaming && !busy;
  if (!remove) blocked.remove = streaming ? 'streaming' : 'busy';

  // 分叉：只对助手消息，且必须是完整一轮（空回复分叉出来没有意义）
  const fork = isAssistant && !streaming && !busy && !empty;
  if (!fork) {
    blocked.fork = !isAssistant ? 'role' : streaming ? 'streaming' : busy ? 'busy' : 'empty';
  }

  const speak = isAssistant && !streaming && !empty && input.speaking !== true;
  if (!speak) blocked.speak = !isAssistant ? 'role' : streaming ? 'streaming' : 'empty';

  // 源码视图：有正文才值得切（对应 AIGX ChatBubble 的 `showSource`）
  const source = !streaming && !empty;

  return { copy, edit, regenerate, remove, fork, speak, source, blocked };
}

/**
 * 删除确认要提示「关联工具结果一并删除」吗。
 *
 * A2A 的文案是固定一句（`删除这条消息？模型消息关联的工具结果也会一并删除。`），
 * 但只在带工具调用时提示才准确 —— 一条纯文本消息说「工具结果会删」会让人以为
 * 删错了东西。返回值决定渲染哪一种文案。
 */
export function deleteMentionsTools(input: { hasToolCalls?: boolean }): boolean {
  return input.hasToolCalls === true;
}

/**
 * 「重新生成」应该从哪条开始重来（下标）。
 *
 * 只在**最后一条是助手消息**时有效；最后一条是用户消息时该走「发送」，不是
 * 「重生成」（重生成会把用户刚输入的内容丢掉）。返回 -1 表示当前不可重生成。
 */
export function regenerateAnchor(
  messages: ReadonlyArray<{ role: string; content?: string }>,
  options: { busy?: boolean } = {},
): number {
  if (options.busy) return -1;
  const last = messages.length - 1;
  if (last < 0) return -1;
  const message = messages[last];
  if (!message || message.role !== 'assistant') return -1;
  if (!String(message.content ?? '').trim()) return -1;
  return last;
}

/**
 * 编辑重发的边界：被编辑的那条必须是用户消息，且**其后的助手回复要一并丢弃**。
 *
 * 返回 `keep`（保留到该条之前）+ `drop`（该条及其后要丢弃的条数）。A2A 的
 * `edit` 只替换正文然后 `replayMessages` 重建 UI（它把工具结果也一并重放），
 * AIGX 的消息是扁平数组，必须显式告诉调用方截到哪。
 */
export function editAnchor(
  messages: ReadonlyArray<{ role: string }>,
  index: number,
): { ok: boolean; keep: number; drop: number } {
  const message = messages[index];
  if (!message || message.role !== 'user') return { ok: false, keep: 0, drop: 0 };
  return { ok: true, keep: index, drop: messages.length - index };
}
