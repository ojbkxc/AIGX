/**
 * /chat 的会话持久化与标题总结（从旧 Chat.tsx 抽出的纯逻辑层）。
 *
 * 为什么单独成文件：/chat 改成 A2A 对话形态后，页面组件只负责装配
 * （左栏 / 顶栏 / 消息流 / 输入区 / 抽屉），而「会话存哪、什么时候落盘、
 * 标题怎么总结」这些与渲染无关的规则都在这儿 —— 与 A2A 把
 * `assistant-session-state.ts` 独立出来的理由相同。
 *
 * ── 数据面 ─────────────────────────────────────────────────
 * 会话列表与消息都走后端 `/api/chat/sessions`（按用户隔离）；
 * 旧版把消息整表存 localStorage，首启时一次性迁移进后端（迁移后清旧键）。
 */
import { api } from '../api';
import { extractAssistantConfig } from '../api/endpoints/assistant';
import type { AssistantConfigWire } from '../api/endpoints/assistant';
import type { DebugMessage } from '../components/chat/types';
import type { ChatStreamUsage, ToolCallState } from '../types';
import type { AssistantConfig } from './chat-state';

/** 本机会话存储（仅迁移期读取旧键，写路径已全在后端） */
export const SESSIONS_KEY = 'aigx_playground_sessions';
export const ACTIVE_KEY = 'aigx_playground_active';
export const LEGACY_KEY = 'aigx_playground_messages';

/** 旧本机会话（仅迁移期读取；含消息体） */
export interface LegacySession {
  id: string;
  title: string;
  model: string;
  messages: DebugMessage[];
  updated_at: number;
}

/** 会话摘要（左栏用；消息按需从后端拉）。
 *  parent/title_source 供父子追溯与命名来源徽标（#58）。 */
export interface PlaygroundSession {
  id: string;
  title: string;
  model: string;
  updated_at: number;
  parent_session_id?: string | null;
  title_source?: string | null;
}

export const uid = (): string => `pg-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

/* ── LLM 标题总结（对照上游 assistant_history.rs title / assistant-message-operations）──
 * 上游由后端总结（config.title_model/title_prompt + titleGenerated 幂等位）；
 * AIGX 无该后端，用同一条 chat_test 链在前端完成（用户/管理员共用）。
 * 可配 titleModel / titlePrompt（localStorage），首轮对话后自动一次，失败静默。 */
export const TITLE_CFG_KEY = 'aigx_chat_title_cfg';
export const TITLED_KEY = 'aigx_chat_titled';

/** 默认提示词（上游 DEFAULT_TITLE_PROMPT 同语义：短标题、无引号无解释） */
export const DEFAULT_TITLE_PROMPT =
  '你是会话标题生成器。根据对话摘录生成一个不超过 12 个字的简洁标题，直接输出标题本身，不要引号、标点或任何解释。';

/** 标题总结配置（本机偏好；titleModel 留空 = 渠道默认首选模型） */
export interface ChatTitleCfg {
  titleModel?: string;
  titlePrompt?: string;
}

export function loadTitleCfg(): ChatTitleCfg {
  try {
    const raw = localStorage.getItem(TITLE_CFG_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    return {
      titleModel: typeof parsed.titleModel === 'string' ? parsed.titleModel : '',
      titlePrompt: typeof parsed.titlePrompt === 'string' ? parsed.titlePrompt : '',
    };
  } catch {
    return {};
  }
}

export function saveTitleCfg(cfg: ChatTitleCfg): void {
  try {
    localStorage.setItem(TITLE_CFG_KEY, JSON.stringify(cfg));
  } catch {
    /* 隐私模式等：静默，本次会话内仍用草稿值 */
  }
}

/** 已自动总结过的会话 id（成败都记一次，防止每条消息都打一次 LLM）；本地封顶 200 条 */
export function loadTitledIds(): string[] {
  try {
    const raw = localStorage.getItem(TITLED_KEY);
    const arr = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === 'string') : [];
  } catch {
    return [];
  }
}

export function saveTitledIds(ids: string[]): void {
  try {
    localStorage.setItem(TITLED_KEY, JSON.stringify(ids.slice(-200)));
  } catch {
    /* 隐私模式等：静默（下次会重复尝试，可接受） */
  }
}

/** 从模型回复里清洗出标题：去首尾引号/空白、截断（模型偶尔带引号或解释） */
export function cleanTitle(raw: string): string {
  return raw
    .trim()
    .replace(/^["'「『『]+/, '')
    .replace(/["'」』』]+$/, '')
    .split('\n')[0]
    .trim()
    .slice(0, 40);
}

/* ── 消息形状归一 ───────────────────────────────────────── */

export const isDebugMessage = (m: unknown): m is DebugMessage => Boolean(
  m && typeof m === 'object' && ((m as DebugMessage).role === 'user' || (m as DebugMessage).role === 'assistant'),
);

/** 消息 → 后端落库形状（#63）：工具调用/用量等非正文数据放 extra，
 *  正文仍走 content，保证旧后端（忽略 extra）与旧数据（无 extra）双向兼容。 */
export function toPersistedMessage(m: DebugMessage): { role: string; content: string; extra?: unknown } {
  const extra: Record<string, unknown> = {};
  if (m.reasoning) extra.reasoning = m.reasoning;
  if (m.toolCalls?.length) extra.tool_calls = m.toolCalls;
  if (m.toolState) extra.tool_state = m.toolState;
  if (m.usage) extra.usage = m.usage;
  if (m.attachments?.length) extra.attachments = m.attachments;
  return {
    role: m.role,
    content: m.content,
    ...(Object.keys(extra).length ? { extra } : {}),
  };
}

function isToolCallState(v: unknown): v is ToolCallState {
  return Boolean(v && typeof v === 'object'
    && typeof (v as ToolCallState).index === 'number'
    && typeof (v as ToolCallState).arguments === 'string');
}

/** 后端消息 → 本地 DebugMessage（extra 是自由形状：逐字段防御性读取） */
export function fromPersistedMessage(m: { role: string; content: string; extra?: unknown }): DebugMessage {
  const msg: DebugMessage = { role: m.role === 'user' ? 'user' : 'assistant', content: m.content ?? '' };
  const extra = m.extra && typeof m.extra === 'object' ? (m.extra as Record<string, unknown>) : null;
  if (!extra) return msg;
  if (typeof extra.reasoning === 'string' && extra.reasoning) msg.reasoning = extra.reasoning;
  if (Array.isArray(extra.tool_calls)) msg.toolCalls = extra.tool_calls.filter(isToolCallState);
  if (extra.tool_state === 'done' || extra.tool_state === 'interrupted' || extra.tool_state === 'error') {
    msg.toolState = extra.tool_state;
  }
  if (extra.usage && typeof extra.usage === 'object') msg.usage = extra.usage as ChatStreamUsage;
  if (Array.isArray(extra.attachments)) {
    msg.attachments = extra.attachments.filter(
      (a): a is { kind: 'image' | 'video' | 'audio'; url: string } => Boolean(
        a && typeof a === 'object' && typeof (a as { url?: unknown }).url === 'string',
      ),
    );
  }
  return msg;
}

/** 旧 localStorage 会话（迁移读取；非法项逐条丢弃，不整批失败） */
export function readLegacySessions(): LegacySession[] {
  try {
    const raw = localStorage.getItem(SESSIONS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((s): s is LegacySession => Boolean(s && typeof s === 'object' && typeof (s as LegacySession).id === 'string'))
      .map((s) => ({
        id: s.id,
        title: typeof s.title === 'string' ? s.title : '',
        model: typeof s.model === 'string' ? s.model : '',
        messages: Array.isArray(s.messages) ? s.messages.filter(isDebugMessage) : [],
        updated_at: typeof s.updated_at === 'number' ? s.updated_at : Date.now(),
      }));
  } catch {
    return [];
  }
}

/** 一次性迁移旧 localStorage 会话到后端（成功后清旧键）。
 *  单条失败跳过，不阻塞其余 —— 迁移是尽力而为的兼容动作。 */
export async function importLegacySessions(): Promise<PlaygroundSession[]> {
  const legacyList = readLegacySessions();
  if (!legacyList.length) return [];
  const imported: PlaygroundSession[] = [];
  for (const s of legacyList.slice(0, 20)) {
    try {
      // 保留原时间（seed 是 ms，后端存秒）→ 排序与旧版一致
      const updatedSec = Math.floor(s.updated_at / 1000);
      const created = await api.createChatSession({
        title: s.title || '新会话',
        model: s.model,
        updated_at: updatedSec > 0 ? updatedSec : undefined,
      });
      const id = created.data?.id;
      if (!id) continue;
      if (s.messages.length) {
        await api.replaceChatMessages(id, s.messages.map(toPersistedMessage), false);
      }
      imported.push({
        id,
        title: s.title || '新会话',
        model: s.model,
        updated_at: updatedSec > 0 ? updatedSec : Math.floor(Date.now() / 1000),
      });
    } catch {
      /* 单条失败跳过，不阻塞其余 */
    }
  }
  try {
    localStorage.removeItem(SESSIONS_KEY);
    localStorage.removeItem(ACTIVE_KEY);
    localStorage.removeItem(LEGACY_KEY);
  } catch {
    /* noop */
  }
  return imported;
}

/** 后端会话摘要 → 左栏项（列表接口字段可能缺省，逐项兜底） */
export function toSessionItem(summary: {
  id: string;
  title?: string;
  model?: string;
  updated_at?: number;
  parent_session_id?: string | null;
  title_source?: string | null;
}): PlaygroundSession {
  return {
    id: summary.id,
    title: summary.title ?? '',
    model: summary.model ?? '',
    updated_at: summary.updated_at ?? 0,
    parent_session_id: summary.parent_session_id ?? null,
    title_source: summary.title_source ?? null,
  };
}

/* ── 助手配置：服务端权威 + localStorage 首帧缓存 ─────────────
 *
 * 与侧栏布局同一套取向：库里那份是权威副本（`GET/PUT /api/assistant/config`），
 * localStorage 只当首帧缓存 —— 这样桌面壳/网页端/多设备读到同一份。端点未就绪
 * （404 / 测试替身缺方法）时静默降级为纯本机，行为与改造前一致。
 *
 * AIGX 的 `AssistantConfig` 与上游 `AssistantConfigWire` 字段一一对应，只做
 * 空串↔null 的归一（上游用 null 表达「未设置」，AIGX 界面用空串）。
 */
export function assistantConfigToWire(config: AssistantConfig): AssistantConfigWire {
  return {
    model: config.model && config.model.trim() ? config.model : null,
    apiKeyId: config.apiKeyId,
    reasoning: config.reasoning,
    titleModel: config.titleModel && config.titleModel.trim() ? config.titleModel : null,
    titlePrompt: config.titlePrompt,
    mode: config.mode,
    systemPromptEnabled: config.systemPromptEnabled,
    systemPrompt: config.systemPrompt,
    tools: config.tools,
  };
}

export function assistantConfigFromWire(wire: AssistantConfigWire, base: AssistantConfig): AssistantConfig {
  return {
    ...base,
    model: typeof wire.model === 'string' ? wire.model : '',
    reasoning: typeof wire.reasoning === 'string' ? wire.reasoning : null,
    titleModel: typeof wire.titleModel === 'string' ? wire.titleModel : '',
    titlePrompt: typeof wire.titlePrompt === 'string' ? wire.titlePrompt : base.titlePrompt,
    mode: wire.mode === 'full' ? 'full' : 'confirm',
    systemPromptEnabled: wire.systemPromptEnabled !== false,
    systemPrompt: typeof wire.systemPrompt === 'string' ? wire.systemPrompt : base.systemPrompt,
    tools: wire.tools && typeof wire.tools === 'object' ? wire.tools : {},
    apiKeyId: typeof wire.apiKeyId === 'string' ? wire.apiKeyId : null,
  };
}

/**
 * 读服务端助手配置。
 * `available=false` = 端点不存在/请求失败（调用方继续用 localStorage）；
 * `available=true, config=null` = 端点可用但库里没存过（调用方上行迁移一次）。
 */
export async function loadAssistantConfigFromServer(): Promise<{
  available: boolean;
  config: AssistantConfigWire | null;
}> {
  const fn = (api as { getAssistantConfig?: () => Promise<unknown> }).getAssistantConfig;
  if (typeof fn !== 'function') return { available: false, config: null };
  try {
    return { available: true, config: extractAssistantConfig(await fn()) };
  } catch {
    return { available: false, config: null };
  }
}

/** 把助手配置推给服务端（fire-and-forget；端点未就绪时静默）。 */
export async function pushAssistantConfigToServer(config: AssistantConfig): Promise<void> {
  const fn = (api as { saveAssistantConfig?: (wire: AssistantConfigWire) => Promise<unknown> }).saveAssistantConfig;
  if (typeof fn !== 'function') return;
  try {
    await fn(assistantConfigToWire(config));
  } catch {
    /* 端点未就绪 / 离线：本机缓存仍生效，界面不回滚（与侧栏布局同一取向） */
  }
}
