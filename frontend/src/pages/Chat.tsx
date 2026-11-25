/**
 * /chat —— AIGX 的「AI 运维」对话页。
 *
 * ── 页面结构（1:1 对齐 A2A v3.5.1 的 assistant 页）──────────────
 *   .asst-layout
 *     AsstSidebar          左栏：新建对话 / 会话列表（标题+时间）/ 底部动作
 *     .asst-main
 *       AsstTopbar         顶栏：会话标题 / 模型 / 设置
 *       .asst-scroll > .asst-flow(840px)
 *                          消息流：用户右气泡、助手左对齐无气泡、空态
 *       AsstComposer       输入区：附件行 / 文本域 / 底部工具栏胶囊
 *   AsstSettingsDrawer     右上设置抽屉：默认行为 / 系统提示词 / 工具
 *
 * ── 相对改造前的根因 ────────────────────────────────────────
 * 改造前本页把整个对话界面**外包**给 ChatDebugger 的 playground 形态：调试器
 * 既是消息流又是输入区，于是「对话形态」与「调试形态」耦合在一起 —— 调试器
 * 一改就动界面，界面想要的 A2A 结构（左栏历史、840px 消息列、胶囊工具栏、
 * 设置抽屉）一个都落不下来。现在页面自己持有布局与状态，ChatDebugger 退回
 * 它真正的职责：**调试视图**（顶栏「调试」入口，独立视图，SSE 报文与
 * tool_calls 全量可见），作为 AIGX 独有能力保留，不是被删掉。
 *
 * ── 数据面 ─────────────────────────────────────────────────
 *   · 会话列表/消息：`/api/chat/sessions`（按用户隔离，fork/rename 后端有界）；
 *   · 本轮回复：`/api/channels/chat_test`（SSE，与调试器同一条链路）；
 *   · 助手配置：本机 localStorage（AIGX 无 `/api/assistant/config`）。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import ChatDebugger, { type DebugMessage } from '../components/ChatDebugger';
import {
  AsstSidebar,
  AsstTopbar,
  AsstEmpty,
  type AsstSessionItem,
} from '../components/chat/AsstShell';
import AsstMessages from '../components/chat/AsstMessages';
import AsstComposer, { type AsstTextFile } from '../components/chat/AsstComposer';
import type { Effort } from '../components/chat/EffortPicker';
import AsstSettingsDrawer from '../components/chat/AsstSettingsDrawer';
import StreamPhaseIndicator from '../components/chat/StreamPhase';
import { MobileDrawer } from '../components/ui';
import { useToast } from '../components/Toast';
import ChatSettingsPanel from '../components/ChatSettingsPanel';
import { isAdmin } from '../lib/utils';
import { mergeToolCallDelta } from '../lib/toolCallAdapter';
import type { ChatStreamUsage, ToolCallDelta } from '../types';
import { streamChat, type AssistantConfig } from './chat-state';
import {
  DEFAULT_TITLE_PROMPT,
  TITLE_CFG_KEY,
  cleanTitle,
  importLegacySessions,
  fromPersistedMessage,
  isDebugMessage,
  loadTitledIds,
  readLegacySessions,
  saveTitledIds,
  toPersistedMessage,
  toSessionItem,
  uid,
  type PlaygroundSession,
} from './chat-session';
import './Chat.css';

/** 空态建议卡片：优先用「提示词管理」里启用的条目（A2A 没有这条链路） */
function loadSuggestionPrompts(): Array<{ title: string; sub: string; content: string }> {
  try {
    const raw = localStorage.getItem('aigx_prompts');
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    const valid = parsed.filter((p): p is {
      id: string; name: string; content: string; tags?: string[]; enabled?: boolean; updated_at?: number;
    } => Boolean(p && typeof p === 'object'
      && (p as { enabled?: boolean }).enabled !== false
      && typeof (p as { content?: unknown }).content === 'string'
      && (p as { content: string }).content.trim() !== ''));
    // 先按更新时间倒序，再按 name 去重（保留最新者），最后裁剪 6 条。
    // name 去重：导入 JSON 按 id 合并、手动新建都可能产生同名条目。
    const seen = new Set<string>();
    return valid
      .sort((a, b) => (b.updated_at ?? 0) - (a.updated_at ?? 0))
      .filter((p) => {
        const key = (p.name || '').trim().toLowerCase();
        if (!key || seen.has(key)) return false;
        seen.add(key);
        return true;
      })
      .slice(0, 6)
      .map((p) => ({
        title: p.name || 'Prompt',
        sub: (p.tags ?? []).slice(0, 3).join(' · '),
        content: p.content,
      }));
  } catch {
    return [];
  }
}

/** 没有自定义提示词时的四项默认建议（new-api 同款语义） */
const DEFAULT_SUGGESTS: Array<{ title: string; content: string }> = [
  { title: '帮我分析一组数据的趋势并给出结论', content: '帮我分析一组数据的趋势并给出结论' },
  { title: '帮我总结一段长文本的要点', content: '帮我总结一段长文本的要点' },
  { title: '写一个可运行的代码示例并解释关键点', content: '写一个可运行的代码示例并解释关键点' },
  { title: '我想学习一个新领域，给我一条完整的学习路径', content: '我想学习一个新领域，给我一条完整的学习路径' },
];

/** 助手配置（本机）——与 chat-state 的 AssistantConfig 同形，默认值集中在此 */
const CFG_KEY = 'aigx_chat_assistant_cfg';

function defaultConfig(): AssistantConfig {
  return {
    model: '',
    reasoning: null,
    titleModel: '',
    titlePrompt: '',
    mode: 'confirm',
    systemPromptEnabled: true,
    systemPrompt: '',
    tools: {},
    apiKeyId: null,
  };
}

function readConfig(): AssistantConfig {
  try {
    const raw = localStorage.getItem(CFG_KEY);
    if (!raw) return defaultConfig();
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const base = defaultConfig();
    return {
      ...base,
      model: typeof parsed.model === 'string' ? parsed.model : base.model,
      reasoning: typeof parsed.reasoning === 'string' ? parsed.reasoning : null,
      titleModel: typeof parsed.titleModel === 'string' ? parsed.titleModel : base.titleModel,
      titlePrompt: typeof parsed.titlePrompt === 'string' ? parsed.titlePrompt : base.titlePrompt,
      mode: parsed.mode === 'full' ? 'full' : 'confirm',
      systemPromptEnabled: parsed.systemPromptEnabled !== false,
      systemPrompt: typeof parsed.systemPrompt === 'string' ? parsed.systemPrompt : base.systemPrompt,
      tools: parsed.tools && typeof parsed.tools === 'object' ? (parsed.tools as Record<string, boolean>) : {},
      apiKeyId: typeof parsed.apiKeyId === 'string' ? parsed.apiKeyId : null,
    };
  } catch {
    return defaultConfig();
  }
}

/** 4 档快捷 pill ↔ 8 档推理等级：同一份状态，两种粒度 */
function effortOf(level: string | null): Effort {
  if (level === 'low' || level === 'minimal') return 'low';
  if (level === 'medium' || level === 'none') return 'medium';
  if (level === 'high' || level === 'xhigh' || level === 'max') return 'high';
  return 'off';
}

function levelOfEffort(effort: Effort): string | null {
  if (effort === 'off') return null;
  if (effort === 'low') return 'low';
  if (effort === 'medium') return 'medium';
  return 'high';
}

/** 附件展开成 OpenAI 的 content blocks（与 ChatDebugger 同一形状） */
function blocksForAttachments(
  attachments: Array<{ kind: 'image' | 'video' | 'audio'; url: string }>,
  text: string,
  files: AsstTextFile[],
): Array<Record<string, unknown>> {
  const blocks: Array<Record<string, unknown>> = [];
  if (text) blocks.push({ type: 'text', text });
  for (const file of files) {
    blocks.push({ type: 'text', text: `\n\n【附件 ${file.name}】\n${file.text}` });
  }
  for (const attachment of attachments) {
    if (attachment.kind === 'image') blocks.push({ type: 'image_url', image_url: { url: attachment.url } });
    if (attachment.kind === 'video') blocks.push({ type: 'video_url', video_url: { url: attachment.url } });
    if (attachment.kind === 'audio') blocks.push({ type: 'audio_url', audio_url: { url: attachment.url } });
  }
  return blocks;
}

/** 界面上的临时提示气泡（流式错误、缺模型等）不算真正的 assistant 回复 */
function isTransient(text: string | undefined): boolean {
  const s = (text ?? '').trim();
  return s === '' || s === '…' || s.startsWith('⚠️') || s.startsWith('⏹');
}

export default function Chat(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  /**
   * toast 的稳定引用。
   *
   * `useToast()` 返回的回调**在实现上是稳定的**（ToastProvider 的 useCallback），
   * 但把它写进 useCallback 的依赖会形成一条「回调 → 依赖 → 回调」的链：
   * 一旦外层重新提供新的函数实例（测试替身、Provider 重挂载），`refreshModels`
   * 就会每次渲染都换引用，挂在它上面的 `useEffect` 随之每帧重跑 →
   * `setModelsLoading(true)` → 再渲染 → 无限循环（实测把页面卡死）。
   * 这里走 ref，把「发提示」从依赖图里摘出去。
   */
  const toastRef = useRef(addToast);
  toastRef.current = addToast;
  const admin = isAdmin();

  /* ── 会话 ── */
  const [sessions, setSessions] = useState<PlaygroundSession[]>([]);
  const [activeId, setActiveId] = useState('');
  const [messages, setMessages] = useState<DebugMessage[]>([]);
  const [loading, setLoading] = useState(true);
  const [historyBusy, setHistoryBusy] = useState(false);
  const sessionsRef = useRef<PlaygroundSession[]>([]);
  const activeRef = useRef('');
  const messagesRef = useRef<DebugMessage[]>([]);
  const loadedRef = useRef(false);
  const saveTimer = useRef<number | null>(null);
  const titledRef = useRef<Set<string>>(new Set(loadTitledIds()));

  /* ── 对话 ── */
  const [config, setConfig] = useState<AssistantConfig>(readConfig);
  const configRef = useRef<AssistantConfig>(config);
  configRef.current = config;
  const [models, setModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [draft, setDraft] = useState('');
  const [images, setImages] = useState<string[]>([]);
  const [files, setFiles] = useState<AsstTextFile[]>([]);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const [streamPhase, setStreamPhase] = useState<'thinking' | 'generating'>('thinking');
  const [lastEventAt, setLastEventAt] = useState(0);
  const [copiedIndex, setCopiedIndex] = useState<number | null>(null);
  const controllerRef = useRef<AbortController | null>(null);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  /* ── 界面 ── */
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [debugOpen, setDebugOpen] = useState(false);
  const [chatSettingsOpen, setChatSettingsOpen] = useState(false);

  const persistConfig = useCallback((next: AssistantConfig, note?: string) => {
    setConfig(next);
    configRef.current = next;
    try {
      localStorage.setItem(CFG_KEY, JSON.stringify(next));
    } catch {
      /* 隐私模式：内存内生效即可 */
    }
    if (note) toastRef.current(note);
  }, []);

  const persistSessions = useCallback((next: PlaygroundSession[]) => {
    sessionsRef.current = next;
    setSessions(next);
  }, []);

  /* ── 模型清单（A2A 走 /api/assistant/models；AIGX 走网关模型列表）── */
  const refreshModels = useCallback(async () => {
    setModelsLoading(true);
    try {
      const res = await api.listModels();
      const raw = Array.isArray(res) ? res : res?.data;
      const list = Array.isArray(raw)
        ? (raw as Array<string | { id?: string }>)
          .map((item) => (typeof item === 'string' ? item : item.id))
          .filter((name): name is string => Boolean(name))
        : [];
      setModels(list);
      if (list.length && !configRef.current.model) {
        persistConfig({ ...configRef.current, model: list[0] });
      }
    } catch {
      setModels([]);
    } finally {
      setModelsLoading(false);
    }
  }, [persistConfig]);

  useEffect(() => { void refreshModels(); }, [refreshModels]);

  /* ── 贴底：新消息进来时滚到底（用户往上翻时不打扰）── */
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const near = el.scrollHeight - el.scrollTop - el.clientHeight < 200;
    if (near) el.scrollTop = el.scrollHeight;
  }, [messages]);

  const saveActiveNow = useCallback(async (): Promise<void> => {
    const id = activeRef.current;
    if (!id || !loadedRef.current) return;
    try {
      await api.replaceChatMessages(id, messagesRef.current.map(toPersistedMessage));
    } catch {
      /* 离线/过期时静默；下次保存会覆盖 */
    }
  }, []);

  /* debounce 持久化（活动会话整表替换到后端） */
  useEffect(() => {
    messagesRef.current = messages;
    if (!loadedRef.current) return;
    if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      saveTimer.current = null;
      void saveActiveNow();
    }, 500);
  }, [messages, saveActiveNow]);

  useEffect(() => () => {
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
      void saveActiveNow();
    }
  }, [saveActiveNow]);

  /* ── 首启：拉会话列表（空则迁移旧 localStorage），定位活动会话 ── */
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      let list: PlaygroundSession[] = [];
      try {
        const res = await api.listChatSessions();
        list = (res.data ?? []).map(toSessionItem);
      } catch {
        list = [];
      }
      if (!list.length) {
        // 旧版本把会话存在 localStorage：一次性搬进后端，搬完清旧键
        if (readLegacySessions().length) list = await importLegacySessions();
      }
      if (cancelled) return;
      let active = '';
      try { active = localStorage.getItem('aigx_playground_active') ?? ''; } catch { /* noop */ }
      if (!list.some((item) => item.id === active)) active = list[0]?.id ?? '';
      sessionsRef.current = list;
      if (!active) {
        let freshId = uid();
        try {
          const created = await api.createChatSession({ model: configRef.current.model ?? '' });
          freshId = created.data?.id ?? freshId;
        } catch {
          /* 后端不可用时退化为本地临时会话（不持久化） */
        }
        if (cancelled) return;
        const fresh: PlaygroundSession = { id: freshId, title: '', model: configRef.current.model ?? '', updated_at: Math.floor(Date.now() / 1000) };
        persistSessions([fresh]);
        activeRef.current = freshId;
        messagesRef.current = [];
        setActiveId(freshId);
        setMessages([]);
        loadedRef.current = true;
        setLoading(false);
        return;
      }
      let msgs: DebugMessage[] = [];
      let model = '';
      try {
        const detail = await api.getChatSession(active);
        msgs = (detail.data?.messages ?? []).filter(isDebugMessage).map(fromPersistedMessage);
        model = detail.data?.session.model ?? '';
      } catch {
        msgs = [];
      }
      if (cancelled) return;
      activeRef.current = active;
      messagesRef.current = msgs;
      persistSessions(list);
      setActiveId(active);
      setMessages(msgs);
      if (model && !configRef.current.model) persistConfig({ ...configRef.current, model });
      loadedRef.current = true;
      setLoading(false);
    })();
    return () => { cancelled = true; };
  }, [persistConfig, persistSessions]);

  /* ── 标题总结（AIGX 独有：A2A 由后端 title 字段负责）── */
  const summarizeTitle = useCallback(async (id: string): Promise<void> => {
    if (id !== activeRef.current) return;
    const msgs = messagesRef.current;
    const excerpts = msgs
      .filter((m) => m.role === 'user' || m.role === 'assistant')
      .filter((m) => m.content.trim())
      .slice(0, 4)
      .map((m) => ({ role: m.role, text: m.content.slice(0, 2000) }));
    if (!excerpts.some((e) => e.role === 'assistant')) return;
    const cfg = configRef.current;
    try {
      const res = await api.testChannelChat({
        channel_id: '',
        protocol: 'openai',
        ...(cfg.titleModel ? { model: cfg.titleModel } : {}),
        system_prompt: cfg.titlePrompt?.trim() || DEFAULT_TITLE_PROMPT,
        message: JSON.stringify(excerpts),
        max_tokens: 120,
        stream: false,
      });
      const raw = res?.data?.content ?? (res?.stream ?? []).map((c) => c.content ?? '').join('');
      const title = cleanTitle(raw);
      if (!title) return;
      await api.renameChatSession(id, title);
      persistSessions(sessionsRef.current.map((item) => (item.id === id ? { ...item, title } : item)));
    } catch {
      /* 静默：总结失败保持原标题 */
    }
  }, [persistSessions]);

  const markTitled = useCallback((id: string): void => {
    titledRef.current.add(id);
    saveTitledIds([...titledRef.current]);
  }, []);

  /* ── 发送 ── */
  const send = useCallback(async (text: string, extraImages: string[], extraFiles: AsstTextFile[]): Promise<void> => {
    const content = text.trim();
    if ((!content && !extraImages.length && !extraFiles.length) || busyRef.current) return;
    const model = configRef.current.model;
    if (!model) {
      addToast(t('请先选择模型'), 'error');
      return;
    }
    const attachments = extraImages.map((url) => ({ kind: 'image' as const, url }));
    /** 上游历史：带图/带附件的消息展开成 content blocks，纯文本仍是字符串 */
    const wireHistory: Array<{ role: string; content: string | Array<Record<string, unknown>> }> =
      messagesRef.current
        .filter((m) => !(m.role === 'assistant' && isTransient(m.content)))
        .map((m) => {
          if (m.role === 'user' && m.attachments?.length) {
            return { role: 'user', content: blocksForAttachments(m.attachments, m.content, []) };
          }
          return { role: m.role, content: m.content };
        });
    wireHistory.push({
      role: 'user',
      content: attachments.length ? blocksForAttachments(attachments, content, extraFiles) : (
        extraFiles.length
          ? blocksForAttachments([], content, extraFiles)
          : content
      ),
    });

    const userMessage: DebugMessage = {
      role: 'user',
      content,
      ...(attachments.length ? { attachments } : {}),
    };
    setMessages((prev) => [...prev, userMessage, { role: 'assistant', content: '…', toolState: 'streaming' }]);
    setDraft('');
    setImages([]);
    setFiles([]);
    setBusy(true);
    busyRef.current = true;
    setStreamPhase('thinking');
    setLastEventAt(Date.now());

    const controller = new AbortController();
    controllerRef.current = controller;
    const body: Record<string, unknown> = {
      channel_id: '',
      protocol: 'openai',
      model,
      message: attachments.length || extraFiles.length
        ? blocksForAttachments(attachments, content, extraFiles)
        : content,
      history: wireHistory.slice(0, -1),
      stream: true,
    };
    const cfg = configRef.current;
    if (cfg.systemPromptEnabled && cfg.systemPrompt.trim()) body.system_prompt = cfg.systemPrompt.trim();
    if (cfg.reasoning) body.reasoning_effort = cfg.reasoning;

    let started = false;
    const patchLast = (patch: (last: DebugMessage) => DebugMessage): void => {
      setMessages((prev) => {
        const next = prev.slice();
        const last = next[next.length - 1];
        if (!last || last.role !== 'assistant') return prev;
        next[next.length - 1] = patch(last);
        return next;
      });
    };

    try {
      await streamChat(
        body,
        (delta: { content: string; isEnd?: boolean; kind?: string; toolCall?: ToolCallDelta; usage?: ChatStreamUsage }) => {
          setLastEventAt(Date.now());
          if (!started) {
            started = true;
            setStreamPhase('generating');
          }
          patchLast((last) => {
            const next: DebugMessage = { ...last };
            if (delta.kind === 'reasoning') {
              next.reasoning = (last.reasoning ?? '') + delta.content;
            } else if (delta.kind === 'tool_call' && delta.toolCall) {
              next.toolCalls = mergeToolCallDelta(last.toolCalls ?? [], delta.toolCall);
              next.toolState = 'streaming';
            } else if (delta.kind === 'usage' && delta.usage) {
              next.usage = delta.usage;
            } else if (delta.kind === 'error' && delta.isEnd) {
              const base = last.content === '…' ? '' : last.content;
              next.content = base ? `${base}\n\n⚠️ ${delta.content}` : `⚠️ ${delta.content}`;
              if (next.toolCalls?.length) next.toolState = 'error';
            } else {
              const base = last.content === '…' ? '' : last.content;
              next.content = delta.isEnd ? base : base + delta.content;
            }
            return next;
          });
        },
        controller.signal,
      );
      patchLast((last) => {
        if (last.toolCalls?.length) return { ...last, toolState: 'done' };
        if (!last.reasoning && (last.content === '…' || !last.content.trim())) {
          return { ...last, content: `⏹ ${t('没有收到回复内容')}`, toolState: 'error' };
        }
        return { ...last, toolState: 'done' };
      });
    } catch (err) {
      const aborted = err instanceof DOMException && err.name === 'AbortError';
      patchLast((last) => ({
        ...last,
        content: aborted
          ? last.content
          : (last.content && last.content !== '…' ? `${last.content}\n\n⚠️ ` : '⚠️ ')
            + (err instanceof Error ? err.message : String(err)),
        toolState: aborted ? 'interrupted' : 'error',
      }));
      if (!aborted) addToast(`${t('发送失败')}：${err instanceof Error ? err.message : String(err)}`, 'error');
    } finally {
      controllerRef.current = null;
      setBusy(false);
      busyRef.current = false;
    }
  }, [addToast, t]);

  const stop = useCallback(() => {
    controllerRef.current?.abort();
  }, []);

  /* 首轮结束后自动总结一次标题（AIGX 独有） */
  useEffect(() => {
    if (loading || !loadedRef.current || !activeId || busy) return;
    if (messages.length < 2) return;
    if (!messages.some((m) => m.role === 'assistant' && m.content.trim() && !isTransient(m.content))) return;
    const current = sessionsRef.current.find((item) => item.id === activeId);
    if (current && current.title && current.title_source === 'user') return;
    if (titledRef.current.has(activeId)) return;
    markTitled(activeId);
    void summarizeTitle(activeId);
  }, [messages, activeId, loading, busy, markTitled, summarizeTitle]);

  /* ── 会话操作 ── */
  const guardBusy = useCallback((): boolean => {
    if (!busyRef.current) return false;
    addToast(t('正在生成回复，请先停止再操作会话'), 'error');
    return true;
  }, [addToast, t]);

  const openSession = useCallback(async (id: string): Promise<void> => {
    setSidebarOpen(false);
    if (id === activeRef.current || guardBusy()) return;
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    await saveActiveNow();
    let msgs: DebugMessage[] = [];
    let model = '';
    try {
      const detail = await api.getChatSession(id);
      msgs = (detail.data?.messages ?? []).filter(isDebugMessage).map(fromPersistedMessage);
      model = detail.data?.session.model ?? '';
    } catch {
      msgs = [];
    }
    activeRef.current = id;
    messagesRef.current = msgs;
    setActiveId(id);
    setMessages(msgs);
    if (model) persistConfig({ ...configRef.current, model, titleModel: configRef.current.titleModel });
    try { localStorage.setItem('aigx_playground_active', id); } catch { /* noop */ }
  }, [guardBusy, persistConfig, saveActiveNow]);

  const newChat = useCallback(async (): Promise<void> => {
    setSidebarOpen(false);
    if (guardBusy()) return;
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    await saveActiveNow();
    let freshId = uid();
    try {
      const created = await api.createChatSession({ model: configRef.current.model ?? '' });
      freshId = created.data?.id ?? freshId;
    } catch {
      /* 后端不可用：本地临时会话兜底 */
    }
    const fresh: PlaygroundSession = {
      id: freshId,
      title: '',
      model: configRef.current.model ?? '',
      updated_at: Math.floor(Date.now() / 1000),
    };
    persistSessions([fresh, ...sessionsRef.current]);
    activeRef.current = freshId;
    messagesRef.current = [];
    setActiveId(freshId);
    setMessages([]);
    setDraft('');
    setImages([]);
    setFiles([]);
  }, [guardBusy, persistSessions, saveActiveNow]);

  const removeSession = useCallback(async (id: string): Promise<void> => {
    if (guardBusy()) return;
    setHistoryBusy(true);
    try {
      await api.deleteChatSession(id);
    } catch {
      /* 已不存在等情况忽略 */
    }
    const rest = sessionsRef.current.filter((item) => item.id !== id);
    persistSessions(rest);
    if (id === activeRef.current) {
      activeRef.current = '';
      messagesRef.current = [];
      setMessages([]);
      if (rest.length) {
        await openSession(rest[0].id);
      } else {
        await newChat();
      }
    }
    setHistoryBusy(false);
  }, [guardBusy, newChat, openSession, persistSessions]);

  const clearHistory = useCallback(async (): Promise<void> => {
    if (guardBusy()) return;
    setHistoryBusy(true);
    try {
      for (const item of sessionsRef.current) {
        try {
          await api.deleteChatSession(item.id);
        } catch {
          /* 单条失败跳过 */
        }
      }
    } finally {
      persistSessions([]);
      setHistoryBusy(false);
    }
    await newChat();
  }, [guardBusy, newChat, persistSessions]);

  const renameSession = useCallback(async (id: string, title: string): Promise<void> => {
    try {
      await api.renameChatSession(id, title);
      persistSessions(sessionsRef.current.map((item) => (
        item.id === id ? { ...item, title, title_source: 'user' } : item
      )));
    } catch (err) {
      addToast(`${t('重命名失败')}：${err instanceof Error ? err.message : String(err)}`, 'error');
    }
  }, [addToast, persistSessions, t]);

  const forkSession = useCallback(async (id: string): Promise<void> => {
    if (guardBusy()) return;
    try {
      let seq = -1;
      if (id === activeRef.current) {
        seq = messagesRef.current.length - 1;
      } else {
        const detail = await api.getChatSession(id);
        seq = (detail.data?.messages?.length ?? 0) - 1;
      }
      if (seq < 0) {
        addToast(t('空会话无可分叉消息'));
        return;
      }
      const res = await api.forkChatSession(id, seq);
      const session = res.data?.session;
      if (!session?.id) throw new Error(t('分叉失败'));
      const copied = res.data?.copied ?? 0;
      const dropped = res.data?.dropped ?? 0;
      persistSessions([toSessionItem(session), ...sessionsRef.current]);
      addToast(dropped > 0
        ? t('已分叉为新会话：复制 {{copied}} 条，超界截断 {{dropped}} 条', { copied, dropped })
        : t('已分叉为新会话：复制 {{copied}} 条', { copied }));
    } catch (err) {
      addToast(`${t('分叉失败')}：${err instanceof Error ? err.message : String(err)}`, 'error');
    }
  }, [addToast, guardBusy, persistSessions, t]);

  /* ── 消息级操作 ── */
  const copyMessage = useCallback((index: number): void => {
    const target = messagesRef.current[index];
    if (!target) return;
    const text = target.reasoning ? `${target.reasoning}\n\n${target.content}` : target.content;
    void navigator.clipboard?.writeText(text).catch(() => undefined);
    setCopiedIndex(index);
    window.setTimeout(() => setCopiedIndex((current) => (current === index ? null : current)), 1500);
  }, []);

  const beginEdit = useCallback((index: number): void => {
    const target = messagesRef.current[index];
    if (!target || target.role !== 'user') return;
    setDraft(target.content);
    setMessages((prev) => prev.slice(0, index));
  }, []);

  const deleteMessage = useCallback((index: number): void => {
    setMessages((prev) => prev.filter((_, i) => i !== index));
  }, []);

  const regenerate = useCallback((index: number): void => {
    const list = messagesRef.current;
    const previousUser = list.slice(0, index).reverse().find((m) => m.role === 'user');
    if (!previousUser) return;
    setMessages(list.slice(0, index));
    void send(previousUser.content, previousUser.attachments?.filter((a) => a.kind === 'image').map((a) => a.url) ?? [], []);
  }, [send]);

  /** 按消息下标分叉当前会话（对齐后端 message_seq = 0-based 含该条） */
  const forkAt = useCallback(async (index: number): Promise<void> => {
    if (guardBusy()) return;
    const source = activeRef.current;
    if (!source) return;
    if (index < 0) {
      addToast(t('空会话无可分叉消息'));
      return;
    }
    try {
      // 先把内存里的最新消息落盘，再让后端按 seq 复制（否则后端只看到旧快照）
      if (saveTimer.current !== null) {
        window.clearTimeout(saveTimer.current);
        saveTimer.current = null;
      }
      await saveActiveNow();
      const res = await api.forkChatSession(source, index);
      const session = res.data?.session;
      if (!session?.id) throw new Error(t('分叉失败'));
      const copied = res.data?.copied ?? 0;
      const dropped = res.data?.dropped ?? 0;
      persistSessions([toSessionItem(session), ...sessionsRef.current]);
      addToast(dropped > 0
        ? t('已分叉为新会话：复制 {{copied}} 条，超界截断 {{dropped}} 条', { copied, dropped })
        : t('已分叉为新会话：复制 {{copied}} 条', { copied }));
    } catch (err) {
      addToast(`${t('分叉失败')}：${err instanceof Error ? err.message : String(err)}`, 'error');
    }
  }, [addToast, guardBusy, persistSessions, saveActiveNow, t]);

  /* ── 附件收集 ── */
  const addImages = useCallback((picked: FileList | File[]): void => {
    const list = Array.from(picked).filter((file) => file.type.startsWith('image/'));
    if (!list.length) return;
    const room = 6 - images.length;
    if (room <= 0) {
      addToast(t('最多 6 张图片'), 'warning');
      return;
    }
    const accepted = list.slice(0, room);
    if (list.length > room) addToast(t('只添加了前 {{n}} 张（上限 6 张）', { n: room }), 'warning');
    for (const file of accepted) {
      if (file.size > 10 * 1024 * 1024) {
        addToast(`${t('图片过大（上限 10MB）')}：${file.name}`, 'error');
        continue;
      }
      const reader = new FileReader();
      reader.onload = () => {
        const url = String(reader.result || '');
        if (url) setImages((prev) => [...prev, url].slice(0, 6));
      };
      reader.readAsDataURL(file);
    }
  }, [addToast, images.length, t]);

  const addFiles = useCallback((picked: FileList | File[]): void => {
    for (const file of Array.from(picked)) {
      const reader = new FileReader();
      reader.onload = () => {
        const text = String(reader.result || '');
        setFiles((prev) => [...prev, { name: file.name, size: file.size, text }].slice(0, 6));
      };
      reader.readAsText(file);
    }
  }, []);

  /* ── 派生 ── */
  const sessionItems: AsstSessionItem[] = useMemo(
    () => sessions.map((item) => ({
      id: item.id,
      title: item.title,
      updated_at: item.updated_at,
      model: item.model,
      parent_session_id: item.parent_session_id,
      title_source: item.title_source,
    })),
    [sessions],
  );

  const currentTitle = useMemo(() => {
    const current = sessions.find((item) => item.id === activeId);
    if (current?.title) return current.title;
    const firstUser = messages.find((m) => m.role === 'user' && m.content.trim());
    const text = (firstUser?.content ?? '').trim().replace(/\s+/g, ' ');
    return text ? (text.length > 40 ? `${text.slice(0, 40)}…` : text) : t('新对话');
  }, [activeId, messages, sessions, t]);

  const suggestions = useMemo(() => {
    const custom = loadSuggestionPrompts();
    if (custom.length) return custom;
    return DEFAULT_SUGGESTS.map((item) => ({ ...item, sub: t('提示词') }));
  }, [t]);

  const modelQuestionable = !modelsLoading && models.length === 0;

  return (
    <div className="asst-layout">
      <AsstSidebar
        sessions={sessionItems}
        activeId={activeId}
        disabled={busy || historyBusy}
        onNew={() => void newChat()}
        onOpen={(id) => void openSession(id)}
        onRemove={(id) => void removeSession(id)}
        onRename={(id, title) => void renameSession(id, title)}
        onFork={(id) => void forkSession(id)}
        onClear={() => void clearHistory()}
        onTitleSettings={() => setSettingsOpen(true)}
        onChatSettings={admin ? () => setChatSettingsOpen(true) : undefined}
      />

      <div className={`asst-main${sidebarOpen ? ' side-open' : ''}`}>
        <AsstTopbar
          title={currentTitle}
          modelLabel={config.model ?? ''}
          onHistory={() => setSidebarOpen((open) => !open)}
          onSettings={() => setSettingsOpen(true)}
          onDebugger={() => setDebugOpen((open) => !open)}
          debuggerActive={debugOpen}
        />

        {debugOpen ? (
          <div className="asst-debug">
            <div className="asst-debug-note">
              {t('调试视图：SSE 报文、tool_calls、协议与参数都在这里；消息不会写回当前会话。')}
              <button type="button" className="asst-link-btn" onClick={() => setDebugOpen(false)}>
                {t('返回对话')}
              </button>
            </div>
            <div className="asst-debug-body">
              <ChatDebugger
                initialMessages={messages}
                onBusyChange={(next) => { busyRef.current = next; setBusy(next); }}
                onForkMessage={(index) => void forkAt(index)}
              />
            </div>
          </div>
        ) : (
          <>
            <div className="asst-scroll" ref={scrollRef}>
              {loading ? (
                <div className="asst-loading">{t('加载中…')}</div>
              ) : messages.length === 0 ? (
                <AsstEmpty
                  suggestions={suggestions}
                  onSend={(text) => void send(text, [], [])}
                  warning={modelQuestionable ? t('没有可用的模型：请先在「模型管理」里启用渠道，或到设置里手动指定模型。') : undefined}
                />
              ) : (
                <div className="asst-flow">
                  <AsstMessages
                    messages={messages}
                    model={config.model ?? ''}
                    busy={busy}
                    copiedIndex={copiedIndex}
                    onCopy={copyMessage}
                    onBeginEdit={beginEdit}
                    onDelete={deleteMessage}
                    onRegenerate={regenerate}
                    onFork={(index) => void forkAt(index)}
                  />
                  <StreamPhaseIndicator
                    active={busy}
                    phase={streamPhase}
                    lastEventAt={lastEventAt}
                  />
                </div>
              )}
            </div>

            <AsstComposer
              draft={draft}
              onDraftChange={setDraft}
              images={images}
              files={files}
              onAddImages={addImages}
              onAddFiles={addFiles}
              onRemoveImage={(index) => setImages((prev) => prev.filter((_, i) => i !== index))}
              onRemoveFile={(index) => setFiles((prev) => prev.filter((_, i) => i !== index))}
              models={models}
              model={config.model}
              modelsLoading={modelsLoading}
              onRefreshModels={() => void refreshModels()}
              onPickModel={(name) => persistConfig({ ...configRef.current, model: name })}
              reasoning={config.reasoning}
              onPickReasoning={(level) => persistConfig({ ...configRef.current, reasoning: level })}
              mode={config.mode}
              modeDisabled
              onModeChange={(mode) => persistConfig({ ...configRef.current, mode })}
              effort={effortOf(config.reasoning)}
              onPickEffort={(value) => persistConfig({ ...configRef.current, reasoning: levelOfEffort(value) })}
              busy={busy}
              awaiting={false}
              canSend={!busy && Boolean(config.model) && Boolean(draft.trim() || images.length || files.length)}
              onSend={() => void send(draft, images, files)}
              onStop={stop}
            />
          </>
        )}
      </div>

      <AsstSettingsDrawer
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        config={config}
        models={models}
        modelsLoading={modelsLoading}
        onRefreshModels={() => void refreshModels()}
        onSave={(next) => persistConfig(next, t('已保存'))}
        admin={admin}
        onOpenDebugger={() => { setSettingsOpen(false); setDebugOpen(true); }}
        titleModel={config.titleModel ?? ''}
        titlePrompt={config.titlePrompt ?? ''}
        onSaveTitleCfg={(next) => {
          persistConfig({ ...configRef.current, titleModel: next.titleModel, titlePrompt: next.titlePrompt });
          try {
            localStorage.setItem(TITLE_CFG_KEY, JSON.stringify({ titleModel: next.titleModel, titlePrompt: next.titlePrompt }));
          } catch { /* 隐私模式 */ }
          addToast(t('标题总结设置已保存'));
        }}
        onSummarizeNow={() => { void summarizeTitle(activeRef.current); addToast(t('已触发标题总结')); }}
      />

      {/* 窄屏左栏：A2A 用 ResponsiveSheet 断点 900，这里沿用 AIGX 的 MobileDrawer */}
      {sidebarOpen && (
        <MobileDrawer open={sidebarOpen} onClose={() => setSidebarOpen(false)} width="min(280px, 86vw)" ariaLabel={t('历史对话')}>
          <AsstSidebar
            sessions={sessionItems}
            activeId={activeId}
            disabled={busy || historyBusy}
            onNew={() => void newChat()}
            onOpen={(id) => void openSession(id)}
            onRemove={(id) => void removeSession(id)}
            onRename={(id, title) => void renameSession(id, title)}
            onFork={(id) => void forkSession(id)}
            onClear={() => void clearHistory()}
            onTitleSettings={() => { setSidebarOpen(false); setSettingsOpen(true); }}
            onChatSettings={admin ? () => { setSidebarOpen(false); setChatSettingsOpen(true); } : undefined}
          />
        </MobileDrawer>
      )}

      {/* 后端「聊天设置」（管理接口，仅管理员）——A2A 没有这一块，属 AIGX 独有 */}
      {admin && chatSettingsOpen && (
        <MobileDrawer
          open={chatSettingsOpen}
          onClose={() => setChatSettingsOpen(false)}
          width="min(92vw, 460px)"
          ariaLabel={t('聊天设置')}
        >
          <div style={{ padding: 16 }}>
            <ChatSettingsPanel />
          </div>
        </MobileDrawer>
      )}
    </div>
  );
}
