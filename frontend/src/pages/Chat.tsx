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
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { MobileDrawer } from '../components/ui';
import { useToast } from '../components/Toast';
import ChatSettingsPanel from '../components/ChatSettingsPanel';
import { isAdmin } from '../lib/utils';
import { mergeToolCallDelta } from '../lib/toolCallAdapter';
import type { ChatStreamUsage, ToolCallDelta } from '../types';
import {
  MAX_IMAGES,
  MAX_TEXT_FILES,
  clearAttachments,
  planAttachmentAdd,
  prepareImage,
  textFileOutcome,
  type AttachmentSkipReason,
  type FileLike,
} from '../components/chat/asst-attach';
import { modelAvailability, modelSupportsImages, resolveEffortOptions } from '../components/chat/asst-model';
import { deleteMentionsTools } from '../components/chat/asst-message-ops';
import { streamChat, type AssistantConfig } from './chat-state';
import {
  DEFAULT_TITLE_PROMPT,
  TITLE_CFG_KEY,
  assistantConfigFromWire,
  cleanTitle,
  importLegacySessions,
  fromPersistedMessage,
  isDebugMessage,
  loadAssistantConfigFromServer,
  loadTitledIds,
  pushAssistantConfigToServer,
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
  /** 服务端配置是否已同步过一次：同步前不把本机默认值推上去（否则会用本机
   *  默认覆盖用户在另一端选好的模型）。同步完成（成功或端点不可用）后置 true。 */
  const configSyncedRef = useRef(false);
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
  /** 删除消息的确认弹窗（A2A 删除要二次确认，且提示工具结果一并删除） */
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const persistConfig = useCallback((next: AssistantConfig, note?: string) => {
    setConfig(next);
    configRef.current = next;
    try {
      localStorage.setItem(CFG_KEY, JSON.stringify(next));
    } catch {
      /* 隐私模式：内存内生效即可 */
    }
    // 服务端为权威副本（跨端同步）；同步过一次之后再推，避免首帧默认值覆盖远端。
    if (configSyncedRef.current) {
      void pushAssistantConfigToServer(next);
    }
    if (note) toastRef.current(note);
  }, []);

  /* ── 首启：从服务端对齐助手配置（端点未就绪则纯本机）──
   * 库里那份是权威：有就覆盖本机缓存；库里为空（首次落库）就把本机那份上行一次，
   * 老用户无感迁移。端点不存在/请求失败时保持本机配置不动（与改造前一致）。 */
  useEffect(() => {
    let cancelled = false;
    void loadAssistantConfigFromServer().then(({ available, config: wire }) => {
      if (cancelled) return;
      if (available && wire) {
        const next = assistantConfigFromWire(wire, configRef.current);
        setConfig(next);
        configRef.current = next;
        try {
          localStorage.setItem(CFG_KEY, JSON.stringify(next));
        } catch {
          /* 写不进缓存也仍按服务端那份渲染 */
        }
      } else if (available) {
        // 端点可用但库里没存过：本机这份上行迁移
        void pushAssistantConfigToServer(configRef.current);
      }
      configSyncedRef.current = true;
    });
    return () => { cancelled = true; };
  }, []);

  const persistSessions = useCallback((next: PlaygroundSession[]) => {
    sessionsRef.current = next;
    setSessions(next);
  }, []);

  /* ── 模型清单（A2A 走 /api/assistant/models；AIGX 走网关模型列表）── */
  /** 逐模型能力位（`/api/models/available` 的 `capabilities`）：判断读图支持用 */
  const [modelCaps, setModelCaps] = useState<Record<string, string[]>>({});
  /** 逐模型声明的思考档位（`/api/models/meta` 的 `reasoning_levels`；管理员才有） */
  const [declaredLevels, setDeclaredLevels] = useState<Record<string, string[]>>({});
  const refreshModels = useCallback(async () => {
    setModelsLoading(true);
    try {
      const res = await api.listModels();
      const raw = Array.isArray(res) ? res : res?.data;
      const rows = Array.isArray(raw) ? (raw as Array<string | { id?: string; capabilities?: unknown }>) : [];
      const list = rows
        .map((item) => (typeof item === 'string' ? item : item.id))
        .filter((name): name is string => Boolean(name));
      const caps: Record<string, string[]> = {};
      for (const item of rows) {
        if (!item || typeof item === 'string') continue;
        const kinds = Array.isArray(item.capabilities) ? item.capabilities.filter((k): k is string => typeof k === 'string') : [];
        if (item.id && kinds.length) caps[item.id] = kinds;
      }
      setModels(list);
      setModelCaps(caps);
      if (list.length && !configRef.current.model) {
        persistConfig({ ...configRef.current, model: list[0] });
      }
    } catch {
      setModels([]);
      setModelCaps({});
    } finally {
      setModelsLoading(false);
    }
  }, [persistConfig]);

  useEffect(() => { void refreshModels(); }, [refreshModels]);

  /**
   * 逐模型声明的思考档位（可选数据源）。
   *
   * `/api/models/meta` 是**管理员**接口，普通用户 403、旧后端也可能没有 ——
   * 因此：方法缺失（测试替身/老前端）直接跳过、请求失败静默降级，都不影响
   * 模型清单本身。拿不到就退回「本地四档」，`resolveEffortOptions` 会如实标
   * `source: 'fallback'`。
   */
  useEffect(() => {
    const read = (api as { listModelMeta?: () => Promise<{ data?: Record<string, { reasoning_levels?: unknown }> }> }).listModelMeta;
    if (typeof read !== 'function') return;
    let cancelled = false;
    void read()
      .then((res) => {
        if (cancelled) return;
        const table = res?.data && typeof res.data === 'object' ? res.data : {};
        const next: Record<string, string[]> = {};
        for (const [name, meta] of Object.entries(table)) {
          const levels = meta?.reasoning_levels;
          if (Array.isArray(levels) && levels.length) {
            next[name] = levels.filter((level): level is string => typeof level === 'string');
          }
        }
        setDeclaredLevels(next);
      })
      .catch(() => { /* 403 / 老后端：没有声明就用兜底四档 */ });
    return () => { cancelled = true; };
  }, []);

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

  /* ── 附件收集（判据全在 `asst-attach.ts`：上限、类型、读取中占额度）── */
  /** 已添加张数/个数的 ref 镜像：连续拖两次时 state 还没刷新，闭包里读到的是旧值 */
  const imagesRef = useRef<string[]>([]);
  imagesRef.current = images;
  const filesRef = useRef<AsstTextFile[]>([]);
  filesRef.current = files;
  /** 读取中的张数（异步读取也要占额度，否则快速连拖能突破上限） */
  const pendingImagesRef = useRef(0);
  const pendingFilesRef = useRef(0);
  const [attachmentsReading, setAttachmentsReading] = useState(false);
  /** 附件代次：发送/切会话清空后，迟到的读取结果不再落回输入区 */
  const attachGenRef = useRef(0);

  /** 清空输入区附件（发送成功 / 切会话 / 新建对话三处同一判据）。
   *  代次 +1 让**迟到的读取结果作废**：用户在图片还在 canvas 降采样时就点了发送，
   *  那次读取落地时不能把图塞回已经清空的输入区。 */
  const resetAttachments = useCallback((): void => {
    attachGenRef.current += 1;
    pendingImagesRef.current = 0;
    pendingFilesRef.current = 0;
    setAttachmentsReading(false);
    const empty = clearAttachments();
    setImages(empty.images);
    setFiles(empty.files);
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
    // 发送成功即清空附件（A2A 同口径：清空发生在「发送 / 切会话 / 新建对话」，
    // 不是「切模型」——切模型只提示不丢用户刚贴的图）
    resetAttachments();
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
  }, [addToast, t, resetAttachments]);

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
    // 切会话清附件：上一段对话贴的图不该跟着进新会话（A2A 同口径）
    setDraft('');
    resetAttachments();
    if (model) persistConfig({ ...configRef.current, model, titleModel: configRef.current.titleModel });
    try { localStorage.setItem('aigx_playground_active', id); } catch { /* noop */ }
  }, [guardBusy, persistConfig, resetAttachments, saveActiveNow]);

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
    resetAttachments();
  }, [guardBusy, persistSessions, saveActiveNow, resetAttachments]);

  const doRemoveSession = useCallback(async (id: string): Promise<void> => {
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

  /** 删除会话：**必须二次确认**。旧 SessionDrawer 内建这个确认，换成左栏后
   *  不能因为「自己画行」就把这层保护丢掉（裸删是不可恢复操作）。 */
  const removeSession = useCallback((id: string): void => {
    if (guardBusy()) return;
    const title = sessionsRef.current.find((item) => item.id === id)?.title || t('未命名对话');
    setConfirmState({
      title: t('删除会话'),
      message: `${t('删除后不可恢复，确认删除')}「${title}」？`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: () => { void doRemoveSession(id); },
    });
  }, [doRemoveSession, guardBusy, t]);

  const doClearHistory = useCallback(async (): Promise<void> => {
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

  /** 清空全部：整库删会话，破坏面比删一条大，同样要确认。 */
  const clearHistory = useCallback((): void => {
    if (guardBusy()) return;
    setConfirmState({
      title: t('清空全部会话'),
      message: t('将删除本机保存的全部历史对话，且无法恢复。确认继续？'),
      confirmText: t('清空'),
      danger: true,
      onConfirm: () => { void doClearHistory(); },
    });
  }, [doClearHistory, guardBusy, t]);

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
    const target = messagesRef.current[index];
    if (!target) return;
    // 删除要二次确认，且带工具调用的消息必须写明「关联工具结果一并删除」
    // （A2A `assistant-message-actions.tsx:42-45` 的同一句话）—— 一条纯文本消息
    // 说「工具结果会删」会让人以为删错了东西，所以两种文案分开。
    setConfirmState({
      title: t('删除消息'),
      message: deleteMentionsTools({ hasToolCalls: Boolean(target.toolCalls?.length) })
        ? t('删除这条消息？模型消息关联的工具结果也会一并删除。')
        : t('删除这条消息？'),
      confirmText: t('删除'),
      danger: true,
      onConfirm: () => {
        setMessages((prev) => prev.filter((_, i) => i !== index));
      },
    });
  }, [t]);

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

  const syncReading = useCallback((): void => {
    setAttachmentsReading(pendingImagesRef.current > 0 || pendingFilesRef.current > 0);
  }, []);

  const skipText = useCallback((reason: AttachmentSkipReason, name: string): string => {
    switch (reason) {
      case 'image-too-large':
        return `${t('图片过大（上限 10MB）')}：${name}`;
      case 'too-many-images':
        return t('最多 6 张图片');
      case 'file-too-large':
        return `${t('文本附件过大（上限 512KB）')}：${name}`;
      case 'too-many-files':
        return t('一次最多 3 个文本附件');
      default:
        return t('只支持图片和 JSON / .info 文本文件，已跳过：{{name}}', { name });
    }
  }, [t]);

  const addImages = useCallback((picked: FileList | File[]): void => {
    const incoming = Array.from(picked) as unknown as FileLike[];
    const plan = planAttachmentAdd({
      imageCount: imagesRef.current.length,
      fileCount: filesRef.current.length,
      pendingImages: pendingImagesRef.current,
      pendingFiles: pendingFilesRef.current,
    }, incoming);
    for (const skipped of plan.skipped) {
      addToast(skipText(skipped.reason, skipped.name), skipped.reason === 'too-many-images' ? 'warning' : 'error');
    }
    if (!plan.images.length) return;
    const accepted = new Set<FileLike>(plan.images);
    const generation = attachGenRef.current;
    pendingImagesRef.current += plan.images.length;
    syncReading();
    for (const file of Array.from(picked)) {
      if (!accepted.has(file as unknown as FileLike)) continue;
      void prepareImage(file)
        .then((url) => {
          if (generation !== attachGenRef.current) return;
          if (!url) return;
          setImages((prev) => (prev.length >= MAX_IMAGES ? prev : [...prev, url]));
        })
        .catch((error: unknown) => {
          addToast(`${t('读取图片失败')}：${error instanceof Error ? error.message : String(error)}`, 'error');
        })
        .finally(() => {
          pendingImagesRef.current = Math.max(0, pendingImagesRef.current - 1);
          syncReading();
        });
    }
  }, [addToast, skipText, syncReading, t]);

  const addFiles = useCallback((picked: FileList | File[]): void => {
    const incoming = Array.from(picked) as unknown as FileLike[];
    const plan = planAttachmentAdd({
      imageCount: imagesRef.current.length,
      fileCount: filesRef.current.length,
      pendingImages: pendingImagesRef.current,
      pendingFiles: pendingFilesRef.current,
    }, incoming);
    // 分流：图片走图片链路（拖进来一张 png 到「文本文件」入口时不该被当 JSON 拒掉）
    const imagePicked = Array.from(picked).filter((file) => file.type.startsWith('image/'));
    if (imagePicked.length) addImages(imagePicked);
    for (const skipped of plan.skipped) {
      if (skipped.reason === 'too-many-images' || skipped.reason === 'image-too-large') continue;
      addToast(skipText(skipped.reason, skipped.name), 'error');
    }
    if (!plan.textFiles.length) return;
    const accepted = new Set<FileLike>(plan.textFiles);
    const generation = attachGenRef.current;
    pendingFilesRef.current += plan.textFiles.length;
    syncReading();
    for (const file of Array.from(picked)) {
      if (!accepted.has(file as unknown as FileLike)) continue;
      void file.text()
        .then((raw: string) => {
          if (generation !== attachGenRef.current) return;
          const outcome = textFileOutcome(file.name, file.size, raw);
          if (outcome.kind === 'empty') {
            addToast(t('「{{name}}」是空文件', { name: file.name }), 'error');
            return;
          }
          if (outcome.kind === 'binary') {
            addToast(t('「{{name}}」看起来是二进制文件，只接收文本', { name: file.name }), 'error');
            return;
          }
          setFiles((prev) => (prev.length >= MAX_TEXT_FILES ? prev : [...prev, outcome.file]));
        })
        .catch((error: unknown) => {
          addToast(`${t('读取文件失败')}：${error instanceof Error ? error.message : String(error)}`, 'error');
        })
        .finally(() => {
          pendingFilesRef.current = Math.max(0, pendingFilesRef.current - 1);
          syncReading();
        });
    }
  }, [addImages, addToast, skipText, syncReading, t]);

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

  /**
   * 模型可用性 / 读图能力 / 思考档位（三处判据都在 `asst-model.ts`）。
   *
   * `modelAvailability` 的「空列表 = 未知」很关键：拉清单失败时 `models` 会是
   * 空数组，若按「不在列表里」判失效，用户会在网络抖一下之后看到「模型不可用」，
   * 而他选的模型其实好好的。
   */
  const availability = useMemo(() => modelAvailability(models, config.model), [models, config.model]);
  const supportsImages = useMemo(
    () => (config.model ? modelSupportsImages({ capabilities: modelCaps[config.model] }) : undefined),
    [config.model, modelCaps],
  );
  const effortResolution = useMemo(
    () => resolveEffortOptions({ declared: config.model ? declaredLevels[config.model] : undefined }),
    [config.model, declaredLevels],
  );
  /**
   * 候选里补上「当前值」。
   *
   * 老配置可能存着 `xhigh`/`max`（A2A 八档时代写进去的）。候选按本网关值域
   * 收窄后它不在列表里，若直接渲染，胶囊显示 `Max` 而下拉里一个都没选中 ——
   * 看起来像界面坏了。补进候选让它**可见可改**，`effortDropped` 那行同时说明
   * 这些档位本网关不认。不静默把用户的选择改成 `off`。
   */
  const effortLevels = useMemo(() => {
    const base = effortResolution.options as string[];
    const current = config.reasoning;
    if (current && !base.includes(current)) return [...base, current];
    return base;
  }, [effortResolution.options, config.reasoning]);

  return (
    <div className="asst-layout">
      <AsstSidebar
        sessions={sessionItems}
        activeId={activeId}
        disabled={busy || historyBusy}
        onNew={() => void newChat()}
        onOpen={(id) => void openSession(id)}
        onRemove={removeSession}
        onRename={(id, title) => void renameSession(id, title)}
        onFork={(id) => void forkSession(id)}
        onClear={clearHistory}
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
              canSend={!busy && !attachmentsReading && Boolean(config.model) && Boolean(draft.trim() || images.length || files.length)}
              onSend={() => void send(draft, images, files)}
              onStop={stop}
              modelAvailable={availability.available}
              modelSupportsImages={supportsImages}
              effortLevels={effortLevels}
              effortDropped={effortResolution.dropped}
              reading={attachmentsReading}
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
            onRemove={removeSession}
            onRename={(id, title) => void renameSession(id, title)}
            onFork={(id) => void forkSession(id)}
            onClear={clearHistory}
            onTitleSettings={() => { setSidebarOpen(false); setSettingsOpen(true); }}
            onChatSettings={admin ? () => { setSidebarOpen(false); setChatSettingsOpen(true); } : undefined}
          />
        </MobileDrawer>
      )}

      {/* 删除消息确认（A2A：删除必须二次确认，带工具调用时提示工具结果一并删除） */}
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

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
