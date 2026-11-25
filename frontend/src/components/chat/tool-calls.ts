/**
 * 工具卡纯逻辑：参数解析 / 凭证遮蔽 / 长文本截断 / 单条调用状态推导。
 *
 * ── 为什么单独一个模块 ────────────────────────────────────────
 * 同一份 `tool_call` 有两个渲染入口：`/chat`（ChatDebugger → ChatBubble →
 * ToolCallCard）与 `/agent`（AI 运维工作台的工具轨迹）。若「参数算不算合法
 * JSON」「这条算成功还是失败」「截断到多少字」各写一份，分叉的后果很具体：
 * 同一次调用在聊天页显示「完成」、在运维页显示「失败」，用户不知道该信谁。
 * 判据只在这里，渲染层只消费结论。
 *
 * ── 参数一律不抛错 ────────────────────────────────────────────
 * 流式分片是中转态：`{"path":` 这种半截 JSON 是常态，不是异常。解析失败要
 * **保留原文**（用户要看模型到底发了什么）并标明「非合法 JSON」，而不是像
 * A2A 那样当成空对象隐藏起来 —— 空对象会让人以为「模型没给参数」。
 *
 * ── 凭证不进 DOM ──────────────────────────────────────────────
 * 工具参数里可能有 `api_key` / `token` 这类字面值（AIGX 的运维工具甚至会
 * **创建**密钥）。会话落库（`extra.tool_calls`）与会话回看都会把它们摊在
 * 屏幕上。这里按「键名判定 + 值替换」遮蔽，只遮蔽判定为凭证的键，`key_id`
 * 这种标识符保持可见（它不是秘密，遮了反而看不出改的是哪把钥匙）。
 */
import type { ChatStreamUsage, ToolCallCardState, ToolCallState } from '../../types';

/** 判定为凭证的键名 → 值替换成这个（不是 i18n 文案：出现在 JSON 片段里） */
export const REDACTED_VALUE = '***';

/**
 * 键名是不是「凭证类」。
 *
 * 先排除标识符后缀（`_id` / `_name` / `_count` 等）：`api_key_id` 是钥匙的编号，
 * `api_key` 才是钥匙本身。只排除后缀不做整词白名单，是因为各家工具的命名习惯
 * 不一致（`access_token` / `op_token` / `x-api-key`），前缀正则漏得比后缀多。
 */
export function isSensitiveKey(key: string): boolean {
  const normalized = key.toLowerCase().replace(/[^a-z0-9]/g, '');
  if (!normalized) return false;
  if (/(id|name|count|len|length|type|url|endpoint|path|mode|status)$/.test(normalized)) return false;
  return /(apikey|token|secret|password|passwd|authorization|cookie|credential|privatekey|sessionkey)/.test(normalized);
}

/** 递归遮蔽（深度上限 6：工具参数不会有更深的合法结构，超深只可能是环） */
export function redactArguments(
  value: unknown,
  depth = 0,
): { value: unknown; redacted: number } {
  if (depth > 6) return { value, redacted: 0 };
  if (Array.isArray(value)) {
    let redacted = 0;
    const next = value.map((item) => {
      const result = redactArguments(item, depth + 1);
      redacted += result.redacted;
      return result.value;
    });
    return { value: next, redacted };
  }
  if (value && typeof value === 'object') {
    let redacted = 0;
    const next: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
      if (isSensitiveKey(key) && item !== null && item !== undefined && item !== '') {
        if (typeof item === 'string' || typeof item === 'number') {
          next[key] = REDACTED_VALUE;
          redacted += 1;
          continue;
        }
      }
      const result = redactArguments(item, depth + 1);
      redacted += result.redacted;
      next[key] = result.value;
    }
    return { value: next, redacted };
  }
  return { value, redacted: 0 };
}

/** 宽松解析：键值都当缺（模型偶尔给出数组 / 非对象 / 半截 JSON） */
export function parseToolArguments(raw: string | undefined | null): {
  ok: boolean;
  value: Record<string, unknown>;
} {
  const text = String(raw ?? '').trim();
  if (!text) return { ok: false, value: {} };
  try {
    const parsed: unknown = JSON.parse(text);
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
      return { ok: true, value: parsed as Record<string, unknown> };
    }
    return { ok: false, value: {} };
  } catch {
    return { ok: false, value: {} };
  }
}

/** 参数块的渲染形态 */
export interface ToolArgumentsView {
  /** 展示文本：可解析且无凭证时**逐字保留原文**（前端既有断言按紧凑形态匹配） */
  text: string;
  /** 原文不是合法 JSON 对象（半截分片/数组/纯文本） */
  invalid: boolean;
  /** 被遮蔽的凭证键数量（> 0 时界面要给「已隐藏」提示） */
  redacted: number;
}

/**
 * 参数 → 展示文本。
 *
 * 可解析且不含凭证时返回**原文**而不是 `JSON.stringify(value, null, 2)`：
 * 一是流式期间重新格式化会让文本跳动，二是既有 e2e 断言
 * （`e2e/chat-sse-tool-calls.spec.ts`）按 `{"path":"README.md"}` 这种紧凑
 * 形态匹配，重排会把它拆散成跨行 token。
 */
export function argumentsView(raw: string | undefined | null): ToolArgumentsView {
  const text = String(raw ?? '');
  const parsed = parseToolArguments(text);
  if (!parsed.ok) return { text, invalid: text.trim().length > 0, redacted: 0 };
  const masked = redactArguments(parsed.value);
  if (!masked.redacted) return { text, invalid: false, redacted: 0 };
  return {
    text: JSON.stringify(masked.value, null, 2),
    invalid: false,
    redacted: masked.redacted,
  };
}

/* ─── 单条调用状态 ─────────────────────────────────────────── */

/**
 * 单条调用的阶段。
 *
 * `done`：结果已回灌且不像失败；`failed`：结果像失败 / 整条消息出错且无结果；
 * `cancelled`：用户中止且这条没拿到结果；`running`：还在流式里等结果。
 */
export type ToolCallPhase = 'running' | 'done' | 'failed' | 'cancelled' | 'pending';

/** 结果文本像不像失败（后端 `tool_result.ok` 在回灌层不一定带下来，只能从文本判） */
export function resultLooksFailed(text: string | undefined | null): boolean {
  const raw = String(text ?? '').trim();
  if (!raw) return false;
  if (/^(⚠️|error\b|failed\b|exception\b|panic\b)/i.test(raw)) return true;
  // JSON 结果里的显式失败位（`{"ok":false}` / `{"success":false}` / 非空 `error`）
  if (raw.startsWith('{')) {
    const parsed = parseToolArguments(raw);
    if (parsed.ok) {
      if (parsed.value.ok === false) return true;
      if (parsed.value.success === false) return true;
      const err = parsed.value.error;
      if (typeof err === 'string' && err.trim()) return true;
      if (err && typeof err === 'object') return true;
    }
  }
  return false;
}

/** 单条调用的阶段判定（消息级状态是兜底，不是覆盖） */
export function callPhase(
  call: Pick<ToolCallState, 'result'>,
  state: ToolCallCardState,
): ToolCallPhase {
  const result = typeof call.result === 'string' ? call.result : '';
  if (result.trim()) return resultLooksFailed(result) ? 'failed' : 'done';
  if (state === 'interrupted') return 'cancelled';
  if (state === 'error') return 'failed';
  if (state === 'streaming') return 'running';
  return 'done';
}

/**
 * 阶段 → 既有 i18n key。
 *
 * `pending` 只用「人工审批」这个既有键：AIGX 的「等确认」场景**只有**审批
 * （`/api/agent/approvals`），没有 A2A 那种「改动类工具先渲染确认卡」的通用
 * 确认流程。另造一个「待确认」键会让英文界面回落中文 —— `zh.json`/`en.json`
 * 不在本次改动的可写范围，键必须落在既有集合里。
 */
export function phaseLabelKey(phase: ToolCallPhase): string {
  switch (phase) {
    case 'running':
      return '进行中';
    case 'failed':
      return '失败';
    case 'cancelled':
      return '停止';
    case 'pending':
      return '人工审批';
    default:
      return '完成';
  }
}

/** 结果未回灌（显示「完成」但没有内容可看） */
export function missingResult(phase: ToolCallPhase): boolean {
  return phase === 'done' || phase === 'cancelled';
}

/* ─── 长文本 ───────────────────────────────────────────────── */

/** 结果块默认展示多少字符（超出才折叠，避免一屏 DOM 塞进几十 KB） */
export const RESULT_PREVIEW_CHARS = 4000;

export interface PreviewText {
  shown: string;
  truncated: boolean;
  /** 被折叠掉的字符数 */
  hidden: number;
}

/** 超长文本截断（纯函数，便于单测；渲染层负责「展开全部」按钮） */
export function previewText(text: string, limit = RESULT_PREVIEW_CHARS): PreviewText {
  const raw = String(text ?? '');
  const size = Math.max(0, Math.floor(limit));
  if (raw.length <= size) return { shown: raw, truncated: false, hidden: 0 };
  return { shown: raw.slice(0, size), truncated: true, hidden: raw.length - size };
}

/** 用量总 token（缺 total 时用 prompt + completion 合成） */
export function usageTotal(usage?: ChatStreamUsage): number | undefined {
  if (!usage) return undefined;
  if (typeof usage.total_tokens === 'number') return usage.total_tokens;
  if (typeof usage.prompt_tokens === 'number' || typeof usage.completion_tokens === 'number') {
    return (usage.prompt_tokens ?? 0) + (usage.completion_tokens ?? 0);
  }
  return undefined;
}
