/**
 * tool_call / usage 帧适配层（#63）。
 *
 * 背景：#58 交付会话持久化 UI 前置件时，后端计量层已透传 `delta.tool_calls`
 * （chat.rs:578-610 按 index 分片聚合 + 三层参数自修复），但 role:tool 回灌层
 * 仍在制品中——**事件形状未定**。为了不让 UI 被未来的形状变更污染，所有
 * 「线上帧 → UI 结构」的转换都收敛到本文件：回灌层落地后只改 adapter。
 *
 * 已支持的线上形状（按优先级识别，`mode: 'auto'` 全识别）：
 * 1. OpenAI 原生流式分片 `choices[0].delta.tool_calls[]`（当前稳定契约）：
 *    首帧 `{index, id, type:'function', function:{name, arguments:''}}`，
 *    后续帧只剩 `{index, function:{arguments:'<分片>'}}`。
 * 2. OpenAI 非流式整帧 `choices[0].message.tool_calls[]`（arguments 是完整字符串）。
 * 3. 回灌层预留形状（role:tool）：顶层 `tool_calls[]` / `tool_call` 单条 /
 *    `{role:'tool', tool_call_id, name, content}` 结果帧。结果字段名同时接受
 *    `result` / `output` / `content`（三者语义一致，取第一个存在的字符串）。
 *
 * 这里只做**归一化**，不做聚合：分片按 index 原样交给 UI，聚合逻辑在
 * ToolCallCard 侧的状态机里（便于流式渐进渲染与中断态处理）。
 */
import type { ChatStreamUsage, ToolCallDelta, ToolCallState } from '../types';

export type ToolCallWireMode = 'auto' | 'openai_delta' | 'role_tool';

const asString = (v: unknown): string | undefined =>
  typeof v === 'string' ? v : typeof v === 'number' ? String(v) : undefined;

const asRecord = (v: unknown): Record<string, unknown> | null =>
  v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : null;

/** 从一条 tool_call 对象归一化（兼容 function 嵌套与扁平两种写法） */
export function normalizeToolCall(raw: unknown, fallbackIndex: number): ToolCallDelta | null {
  const obj = asRecord(raw);
  if (!obj) return null;
  const fn = asRecord(obj.function) ?? obj;
  const name = asString(fn.name);
  const args = asString(fn.arguments);
  const id = asString(obj.id) ?? asString(obj.tool_call_id);
  const result = asString(obj.result) ?? asString(obj.output) ?? asString(obj.content);
  // 三者全无 → 不是工具帧（例如 heartbeat）
  if (!name && args === undefined && id === undefined && result === undefined) return null;
  const index = typeof obj.index === 'number' ? obj.index : fallbackIndex;
  return {
    index,
    ...(id !== undefined ? { id } : {}),
    ...(name !== undefined ? { name } : {}),
    ...(args !== undefined ? { arguments: args } : {}),
    ...(result !== undefined ? { result } : {}),
  };
}

/** 从数组归一化（丢弃无法识别的条目，保持 index 顺序） */
export function normalizeToolCallList(raw: unknown): ToolCallDelta[] {
  if (!Array.isArray(raw)) return [];
  const out: ToolCallDelta[] = [];
  raw.forEach((item, i) => {
    const tc = normalizeToolCall(item, i);
    if (tc) out.push(tc);
  });
  return out;
}

/**
 * 从单个 SSE 帧（已 JSON.parse）提取 tool_call 增量。
 *
 * 注意：明确忽略 Anthropic 原生 `content_block_start/delta` 里的
 * `tool_use`——当前 chat_test 的 Anthropic 路径由后端归一化为 `delta.text`，
 * 前端只消费归一化后的事件（与 #47 的性能口径一致，零额外解析开销）。
 */
export function extractToolCalls(
  frame: Record<string, unknown>,
  mode: ToolCallWireMode = 'auto',
): ToolCallDelta[] {
  if (mode === 'openai_delta' || mode === 'auto') {
    const choices = Array.isArray(frame.choices)
      ? (frame.choices as Array<Record<string, unknown>>)
      : [];
    for (const choice of choices) {
      // 流式：delta.tool_calls
      const delta = asRecord(choice.delta);
      if (delta) {
        const list = normalizeToolCallList(delta.tool_calls);
        if (list.length) return list;
      }
      // 非流式降级（上游一次性返回）：message.tool_calls
      const message = asRecord(choice.message);
      if (message) {
        const list = normalizeToolCallList(message.tool_calls);
        if (list.length) return list;
      }
    }
  }
  if (mode === 'role_tool' || mode === 'auto') {
    // 回灌层预留形状：顶层 tool_calls 数组 / 单条 tool_call / role:tool 结果帧
    const list = normalizeToolCallList(frame.tool_calls);
    if (list.length) return list;
    const single = normalizeToolCall(frame.tool_call, 0);
    if (single) return [single];
    if (frame.role === 'tool') {
      const t = normalizeToolCall(frame, 0);
      if (t) return [t];
    }
  }
  return [];
}

/**
 * 从单个 SSE 帧提取用量。
 *
 * OpenAI 契约：`stream_options.include_usage=true` 时终帧为
 * `{choices:[], usage:{prompt_tokens,...}}`（choices 空数组是官方信号）。
 * 部分兼容上游把 usage 挂在最后一个带 choices 的帧上，这里两种都接。
 */
export function extractUsage(frame: Record<string, unknown>): ChatStreamUsage | null {
  const usage = asRecord(frame.usage) ?? asRecord(asRecord(frame.data)?.usage);
  if (!usage) return null;
  const num = (v: unknown): number | undefined =>
    typeof v === 'number' && Number.isFinite(v) ? v : undefined;
  const prompt = num(usage.prompt_tokens);
  const completion = num(usage.completion_tokens);
  const total = num(usage.total_tokens);
  if (prompt === undefined && completion === undefined && total === undefined) return null;
  return {
    ...(prompt !== undefined ? { prompt_tokens: prompt } : {}),
    ...(completion !== undefined ? { completion_tokens: completion } : {}),
    ...(total !== undefined ? { total_tokens: total } : {}),
  };
}

/** 供设置面板/调试使用：可切换的线上形状（默认 auto） */
export function isToolCallWireMode(v: unknown): v is ToolCallWireMode {
  return v === 'auto' || v === 'openai_delta' || v === 'role_tool';
}

/**
 * 把一个分片并入已聚合列表（纯函数，便于单测与流式状态机复用）。
 *
 * 合并规则：
 * - 同 index 视为同一次调用；`id`/`name` 只取首次出现的非空值（分片帧不带）；
 * - `arguments` 逐片拼接（首帧常为空串）；`result` 覆盖式取最新非空值
 *   （回灌层可能多帧更新结果，取最后一份）；
 * - 新 index 追加到末尾，保证渲染顺序与模型下发顺序一致。
 */
export function mergeToolCallDelta(prev: ToolCallState[], delta: ToolCallDelta): ToolCallState[] {
  const next = prev.map((t) => ({ ...t }));
  const at = next.findIndex((t) => t.index === delta.index);
  if (at < 0) {
    next.push({
      index: delta.index,
      ...(delta.id ? { id: delta.id } : {}),
      ...(delta.name ? { name: delta.name } : {}),
      arguments: delta.arguments ?? '',
      ...(delta.result !== undefined ? { result: delta.result } : {}),
    });
    return next;
  }
  const cur = next[at];
  next[at] = {
    ...cur,
    ...(delta.id ? { id: delta.id } : {}),
    ...(delta.name ? { name: delta.name } : {}),
    arguments: cur.arguments + (delta.arguments ?? ''),
    ...(delta.result !== undefined && delta.result !== '' ? { result: delta.result } : {}),
  };
  return next;
}