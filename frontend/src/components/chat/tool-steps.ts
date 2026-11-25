/**
 * AI 运维（`/agent`）的工具轨迹 → 与 A2A 一致的工具卡数据。
 *
 * ── 现状的问题 ───────────────────────────────────────────────
 * `pages/Agent.tsx` 把 SSE 的 `tool_call` / `approval_resolved` /
 * `tool_result` 各自 push 成一「行」，三行分别是「调用」「人工审批」「结果」，
 * **没有配对**。后果：一串多次调用的轨迹读起来像流水账，且「这次调用是成功
 * 还是失败」要人眼在相邻两行之间拼。A2A 的口径是**一次调用一张卡**：卡头是
 * 工具名 + 状态（待确认 / 执行中 / 完成 / 失败 / 已取消），点开才看参数与结果。
 *
 * 本模块把轨迹（含历史会话里落库的 `tool_calls` JSON）重建成那个形状，并给出
 * 回灌到 `ToolCallCard` 的适配（`toToolCallStates` + `phaseOverrides`），
 * 让两个页面共用同一张卡，而不是各写一套三态。
 *
 * ── 配对判据 ─────────────────────────────────────────────────
 * 后端事件只带 `name`，**没有** call id（见 `agent/runner.rs` 的 `AgentEvent`）。
 * 因此按「同名先进先出」配对：第 N 条 `tool_call` 与第 N 条同名 `tool_result`
 * 是一对。这在串行工具调用下等价于按 id 配对（后端就是串行执行的）；同一轮
 * 并行同名的情况后端不存在，若将来出现，会体现为「后到的结果覆盖前一条」，
 * 届时必须由后端补 id —— 不要在名字上加序号猜。
 */

/** 轨迹里的一条（历史会话落库形状 = 流式事件形状，见 `agent/api.rs`） */
export type AgentTraceStep =
  | { kind: 'call'; name: string; args: string }
  | { kind: 'result'; name: string; ok: boolean; text?: string }
  | { kind: 'approval'; name: string; approved?: boolean; requestId?: string };

/** 流式事件（后端 `AgentEvent` 的 snake_case 序列化） */
export interface AgentStreamEvent {
  type: string;
  name?: string;
  arguments?: string;
  ok?: boolean;
  text?: string;
  request_id?: string;
  approved?: boolean;
  message?: string;
  content?: string;
  turn?: number;
}

/** 一次调用在卡片里的形态 */
export interface ToolAgentItem {
  /** 与 `ToolCallState.index` 同义：卡片内顺序 + 阶段覆盖的键 */
  index: number;
  name: string;
  args: string;
  result?: string;
  /** 后端 `tool_result.ok`；缺省 = 没拿到结果 */
  ok?: boolean;
  /** 这次调用上挂过的审批（AIGX 独有：人工审批是 Agent 的一等公民） */
  approvals: Array<{ approved?: boolean; requestId?: string }>;
  phase: 'pending' | 'running' | 'done' | 'failed' | 'cancelled';
}

const asText = (value: unknown): string => (typeof value === 'string' ? value : '');

/**
 * 历史会话里落库的 `tool_calls`（`Array<Record<string, unknown>>`）→ 轨迹。
 *
 * 落库形状是后端 `json!({...})` 直接序列化的结果，**不是**类型化的：字段可能
 * 因版本差异缺失（老会话没有 `request_id`）。认不出的条目跳过，不让一条坏
 * 数据把整段轨迹吞掉。
 */
export function stepsFromTrace(raw: unknown): AgentTraceStep[] {
  if (!Array.isArray(raw)) return [];
  const steps: AgentTraceStep[] = [];
  for (const entry of raw) {
    if (!entry || typeof entry !== 'object') continue;
    const row = entry as Record<string, unknown>;
    const name = asText(row.name);
    const type = asText(row.type);
    if (!name && type !== 'tool_result') continue;
    if (type === 'tool_call') {
      steps.push({ kind: 'call', name, args: asText(row.arguments) });
      continue;
    }
    if (type === 'tool_result') {
      steps.push({ kind: 'result', name, ok: row.ok === true, text: asText(row.text) || undefined });
      continue;
    }
    if (type === 'approval_request') {
      steps.push({
        kind: 'approval',
        name,
        approved: undefined,
        ...(asText(row.request_id) ? { requestId: asText(row.request_id) } : {}),
      });
      continue;
    }
    if (type === 'approval_resolved') {
      steps.push({ kind: 'approval', name, approved: row.approved !== false });
    }
  }
  return steps;
}

/** 流式事件 → 轨迹条目（与 `stepsFromTrace` 同一套语义，只是来源是 SSE） */
export function stepFromEvent(event: AgentStreamEvent): AgentTraceStep | null {
  const name = asText(event.name);
  if (event.type === 'tool_call') {
    return { kind: 'call', name, args: asText(event.arguments) };
  }
  if (event.type === 'tool_result') {
    return { kind: 'result', name, ok: event.ok === true, text: asText(event.text) || undefined };
  }
  if (event.type === 'approval_request') {
    return {
      kind: 'approval',
      name,
      approved: undefined,
      ...(asText(event.request_id) ? { requestId: asText(event.request_id) } : {}),
    };
  }
  if (event.type === 'approval_resolved') {
    return { kind: 'approval', name, approved: event.approved !== false };
  }
  return null;
}

/** 同名先进先出：找到「同名、还没有结果」的最早一条 */
function pendingIndex(items: readonly ToolAgentItem[], name: string): number {
  return items.findIndex((item) => item.name === name && item.result === undefined);
}

/**
 * 轨迹 → 卡片条目。
 *
 * 批处理形态（历史恢复 / 一次拿到全量轨迹）与增量形态（`pushStep`）共用这一
 * 段配对逻辑，避免两条路径判出不同的成功/失败。
 */
export function itemsFromSteps(steps: readonly AgentTraceStep[]): ToolAgentItem[] {
  const items: ToolAgentItem[] = [];
  for (const step of steps) {
    if (step.kind === 'call') {
      items.push({
        index: items.length,
        name: step.name,
        args: step.args,
        approvals: [],
        phase: 'running',
      });
      continue;
    }
    if (step.kind === 'approval') {
      const at = pendingIndex(items, step.name);
      const target = at >= 0 ? items[at] : undefined;
      if (!target) {
        // 只有审批、没有对应调用（老会话被截断过）：单独记一条，不并入别的调用
        items.push({
          index: items.length,
          name: step.name,
          args: '',
          approvals: [{ approved: step.approved, requestId: step.requestId }],
          phase: step.approved === false ? 'cancelled' : 'pending',
        });
        continue;
      }
      target.approvals.push({ approved: step.approved, requestId: step.requestId });
      if (step.approved === false) target.phase = 'cancelled';
      else if (step.approved === true) target.phase = 'running';
      else target.phase = 'pending';
      continue;
    }
    const at = pendingIndex(items, step.name);
    if (at < 0) {
      // 结果先到（轨迹从中间截断）：如实记一条「有结果、无调用」
      items.push({
        index: items.length,
        name: step.name,
        args: '',
        result: step.text ?? '',
        ok: step.ok,
        approvals: [],
        phase: step.ok ? 'done' : 'failed',
      });
      continue;
    }
    const target = items[at];
    target.result = step.text ?? '';
    target.ok = step.ok;
    target.phase = step.ok ? 'done' : 'failed';
  }
  return items;
}

/**
 * 流结束时的收尾：还在 `running` / `pending` 的条目按「是否被中止」定性。
 *
 * 不做「看起来成功就算成功」的猜测：没有结果就是没有结果，`done` 只表示
 * 「这一轮结束了」，卡片会额外标「结果未回灌」。
 */
export function finalizeItems(
  items: readonly ToolAgentItem[],
  options: { cancelled?: boolean } = {},
): ToolAgentItem[] {
  return items.map((item) => {
    if (item.phase !== 'running' && item.phase !== 'pending') return item;
    return { ...item, phase: options.cancelled ? 'cancelled' : 'done' };
  });
}

/** 增量：把一条新轨迹并入已有条目（流式期间调用，返回新数组，不改入参） */
export function appendStep(
  items: readonly ToolAgentItem[],
  step: AgentTraceStep,
): ToolAgentItem[] {
  return itemsFromSteps([
    ...itemsToSteps(items),
    step,
  ]);
}

/**
 * 条目 → 轨迹（`appendStep` 的逆运算）。
 *
 * 增量路径刻意**重跑一遍配对**而不是就地改：就地改要维护队列状态（哪条已经
 * 配过），而重跑是纯函数、与历史恢复走同一条判据。轨迹条数是「工具调用次数」，
 * 量级在十几条，重跑没有性能问题。
 */
export function itemsToSteps(items: readonly ToolAgentItem[]): AgentTraceStep[] {
  const steps: AgentTraceStep[] = [];
  for (const item of items) {
    steps.push({ kind: 'call', name: item.name, args: item.args });
    for (const approval of item.approvals) {
      steps.push({ kind: 'approval', name: item.name, approved: approval.approved, requestId: approval.requestId });
    }
    if (item.result !== undefined) {
      steps.push({ kind: 'result', name: item.name, ok: item.ok === true, text: item.result });
    }
  }
  return steps;
}

/** 条目 → `ToolCallCard` 的 `calls`（让运维页与聊天页共用同一张卡） */
export function toToolCallStates(items: readonly ToolAgentItem[]): Array<{
  index: number;
  name: string;
  arguments: string;
  result?: string;
}> {
  return items.map((item) => ({
    index: item.index,
    name: item.name,
    arguments: item.args,
    ...(item.result !== undefined ? { result: item.result } : {}),
  }));
}

/** 条目 → `ToolCallCard` 的阶段覆盖（`ok:false` 的文本不一定长得像错误） */
export function phaseOverrides(
  items: readonly ToolAgentItem[],
): Record<number, 'pending' | 'running' | 'done' | 'failed' | 'cancelled'> {
  const out: Record<number, 'pending' | 'running' | 'done' | 'failed' | 'cancelled'> = {};
  for (const item of items) out[item.index] = item.phase;
  return out;
}

/** 会话整体的卡片状态（`ToolCallCard` 的 `state` 入参） */
export function messageStateFor(options: { busy?: boolean; cancelled?: boolean; failed?: boolean } = {}): 'streaming' | 'done' | 'interrupted' | 'error' {
  if (options.failed) return 'error';
  if (options.busy) return 'streaming';
  if (options.cancelled) return 'interrupted';
  return 'done';
}
