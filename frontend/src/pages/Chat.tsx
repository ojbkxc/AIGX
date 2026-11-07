import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  BarChart3, Code2, GraduationCap, ClipboardList, SlidersHorizontal,
} from 'lucide-react';
import { api } from '../api';
import ChatDebugger, { type DebugMessage } from '../components/ChatDebugger';
import SessionDrawer from '../components/chat/SessionDrawer';
import { useToast } from '../components/Toast';
import './Chat.css';

/** 读取「已启用」提示词并映射为空状态建议卡片（无则用 new-api 同款默认四项） */
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
    // name 去重：导入 JSON 按 id 合并、手动新建都可能产生同名条目，
    // 空状态卡片不应重复展示同一条提示词。
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

const DEFAULT_PROMPTS: Array<{ icon: typeof BarChart3; content: string }> = [
  { icon: BarChart3, content: '帮我分析一组数据的趋势并给出结论' },
  { icon: ClipboardList, content: '帮我总结一段长文本的要点' },
  { icon: Code2, content: '写一个可运行的代码示例并解释关键点' },
  { icon: GraduationCap, content: '我想学习一个新领域，给我一条完整的学习路径' },
];

/** 本机会话存储：多会话列表 + 当前活动会话（旧单会话键在首启时迁移） */
const SESSIONS_KEY = 'aigx_playground_sessions';
const ACTIVE_KEY = 'aigx_playground_active';
const LEGACY_KEY = 'aigx_playground_messages';
/** 旧本机会话（仅迁移期读取；含消息体） */
interface LegacySession {
  id: string;
  title: string;
  model: string;
  messages: DebugMessage[];
  updated_at: number;
}

/** 会话摘要（列表用；消息按需从后端拉） */
interface PlaygroundSession {
  id: string;
  title: string;
  model: string;
  updated_at: number;
}

const uid = (): string => `pg-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

/* ── LLM 标题总结（对照上游 assistant_history.rs title / assistant-message-operations）──
 * 上游由后端总结（config.title_model/title_prompt + titleGenerated 幂等位）；
 * AIGX 无该后端，用 Playground 同一条 chat_test 链在前端完成（用户/管理员共用）。
 * 可配 titleModel / titlePrompt（localStorage），首轮对话后自动一次，失败静默。 */
const TITLE_CFG_KEY = 'aigx_chat_title_cfg';
const TITLED_KEY = 'aigx_chat_titled';
/** 默认提示词（上游 DEFAULT_TITLE_PROMPT 同语义：短标题、无引号无解释） */
const DEFAULT_TITLE_PROMPT =
  '你是会话标题生成器。根据对话摘录生成一个不超过 12 个字的简洁标题，直接输出标题本身，不要引号、标点或任何解释。';

/** 标题总结配置（本机偏好；titleModel 留空 = 渠道默认首选模型） */
interface ChatTitleCfg {
  titleModel?: string;
  titlePrompt?: string;
}

function loadTitleCfg(): ChatTitleCfg {
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

/** 已自动总结过的会话 id（成败都记一次，防止每条消息都打一次 LLM）；本地封顶 200 条 */
function loadTitledIds(): string[] {
  try {
    const raw = localStorage.getItem(TITLED_KEY);
    const arr = raw ? (JSON.parse(raw) as unknown) : [];
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === 'string') : [];
  } catch {
    return [];
  }
}

function saveTitledIds(ids: string[]): void {
  try {
    localStorage.setItem(TITLED_KEY, JSON.stringify(ids.slice(-200)));
  } catch { /* 隐私模式等：静默（下次会重复尝试，可接受） */ }
}

/** 从模型回复里清洗出标题：去首尾引号/空白、截断（模型偶尔带引号或解释） */
function cleanTitle(raw: string): string {
  return raw.trim().replace(/^["'「『『]+/, '').replace(/["'」』』]+$/, '').split('\n')[0].trim().slice(0, 40);
}

const isDebugMessage = (m: unknown): m is DebugMessage => Boolean(
  m && typeof m === 'object' && ((m as DebugMessage).role === 'user' || (m as DebugMessage).role === 'assistant'),
);

function readSessions(): LegacySession[] {
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

/**
 * Chat — new-api 游乐园（Playground）布局。
 *
 * 无侧栏、无模式切换：单一对话流 + 底部输入区；输入区底部工具行对齐 new-api：
 *   [历史 + 参数(带启用数徽标) + 清空] ··· [模型选择器 + 发送/停止]
 * 多会话历史存 localStorage（会话列表 + 当前活动会话，debounce 保存），
 * 旧版单会话键 `aigx_playground_messages` 首启时自动迁移为首个会话。
 */
export default function Chat(): JSX.Element {
  const { t } = useTranslation();

  const [messages, setMessages] = useState<DebugMessage[]>([]);
  const [model, setModel] = useState('');
  const [sessions, setSessions] = useState<PlaygroundSession[]>([]);
  const [activeId, setActiveId] = useState('');
  const [drawerOpen, setDrawerOpen] = useState(false);
  /** 标题总结设置弹层（headerExtra 挂抽屉头部；本机偏好存 localStorage） */
  const [titleCfgOpen, setTitleCfgOpen] = useState(false);
  const [cfgDraft, setCfgDraft] = useState<ChatTitleCfg>(() => loadTitleCfg());
  const [loading, setLoading] = useState(true);
  const saveTimer = useRef<number | null>(null);
  const loadedRef = useRef(false);
  const sessionsRef = useRef<PlaygroundSession[]>([]);
  const activeRef = useRef('');
  const messagesRef = useRef<DebugMessage[]>([]);
  const modelRef = useRef('');
  /** 流式生成中标记（ChatDebugger 上报）：生成中禁止切换/新建/删除会话，
   *  否则切换前的落盘会把流式中间态的半截回复写进当前会话 */
  const busyRef = useRef(false);
  const addToast = useToast();

  /** 会话摘要列表（后端返回；消息按需拉取） */
  const persistSessions = (next: PlaygroundSession[]): void => {
    sessionsRef.current = next;
    setSessions(next);
  };

  /** 把当前消息整表落盘后端（debounce 调用；编辑/重发会改写历史，所以是替换不是追加） */
  const saveActiveNow = useCallback(async (): Promise<void> => {
    const id = activeRef.current;
    if (!id || !loadedRef.current) return;
    try {
      await api.replaceChatMessages(
        id,
        messagesRef.current.map((m) => ({ role: m.role, content: m.content })),
      );
    } catch {
      /* 离线/过期时静默；下次保存会覆盖 */
    }
  }, []);

  /** 会话操作守卫（对齐 Agent 页 `if (busy) return`）：生成中拦截并提示 */
  const guardBusy = useCallback((): boolean => {
    if (!busyRef.current) return false;
    addToast(t('正在生成回复，请先停止再操作会话'), 'error');
    return true;
  }, [addToast, t]);

  /* ── 会话 fork（对照上游 fork：仅 assistant 消息可分叉，截取含本条；
   *   AIGX 消息无 id 用下标；走既有 createChatSession + replaceChatMessages，
   *   无需新后端端点。后端不可用时退化为本地临时会话（与新建会话同口径）。）── */
  const forkSession = useCallback(async (idx: number): Promise<void> => {
    if (guardBusy()) {
      setDrawerOpen(false);
      return;
    }
    const src = messagesRef.current;
    const target = src[idx];
    if (!target || target.role !== 'assistant') return;
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    await saveActiveNow();
    const sliced = src.slice(0, idx + 1).map((m) => ({ role: m.role, content: m.content }));
    const curTitle = sessionsRef.current.find((s) => s.id === activeRef.current)?.title || '';
    const baseTitle = curTitle && curTitle !== '新会话' ? curTitle : t('会话');
    const forkTitle = `${baseTitle} · ${t('分叉')}`;
    let newId = `local-fork-${Date.now().toString(36)}`;
    try {
      const created = await api.createChatSession({ title: forkTitle, model: modelRef.current });
      const id = created.data?.id;
      if (id) {
        newId = id;
        await api.replaceChatMessages(newId, sliced, false);
      }
    } catch {
      /* 后端不可用：本地临时会话兜底（消息不持久化，与 createSession 同口径） */
    }
    persistSessions([
      { id: newId, title: forkTitle, model: modelRef.current, updated_at: Math.floor(Date.now() / 1000) },
      ...sessionsRef.current,
    ]);
    activeRef.current = newId;
    messagesRef.current = sliced;
    setActiveId(newId);
    setMessages(sliced);
    setDrawerOpen(false);
    addToast(t('已从该消息分叉为新会话'));
  }, [addToast, guardBusy, saveActiveNow, t]);

  /* ── LLM 标题总结：首轮对话后自动一次；「新会话」或空标题才触发，
   *   用户手动改过名的会话永不覆盖（对齐上游 titleGenerated 幂等语义）── */
  const titledRef = useRef<Set<string>>(new Set(loadTitledIds()));
  const summarizeTitle = useCallback(async (id: string): Promise<void> => {
    // 只总结当前活动会话（消息在内存里；其余会话切换时自然满足条件）
    if (id !== activeRef.current) return;
    const msgs = messagesRef.current;
    if (!msgs.length) return;
    const excerpts = msgs
      .filter((m) => m.role === 'user' || m.role === 'assistant')
      .filter((m) => m.content.trim())
      .slice(0, 4)
      .map((m) => ({ role: m.role, text: m.content.slice(0, 2000) }));
    // 需完成一轮对话（上游同口径：摘录里没有 assistant 回复就不总结）
    if (!excerpts.some((e) => e.role === 'assistant')) return;
    const cfg = loadTitleCfg();
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
      persistSessions(sessionsRef.current.map((s) => (s.id === id ? { ...s, title } : s)));
    } catch {
      /* 静默：总结失败保持原标题（本地偏好/网络/渠道问题都不该扰动对话） */
    }
  }, []);

  const markTitled = useCallback((id: string): void => {
    titledRef.current.add(id);
    saveTitledIds([...titledRef.current]);
  }, []);

  /** 保存标题总结本机偏好（titleModel/titlePrompt 可配） */
  const saveTitleCfg = (): void => {
    try {
      localStorage.setItem(TITLE_CFG_KEY, JSON.stringify(cfgDraft));
    } catch { /* 隐私模式等：静默，本次会话内仍用草稿值 */ }
    setTitleCfgOpen(false);
    addToast(t('标题总结设置已保存'));
  };

  /** 手动重试当前会话标题（自动轮失败后的出口） */
  const manualTitle = async (): Promise<void> => {
    if (guardBusy()) return;
    if (!activeRef.current || !messagesRef.current.length) return;
    markTitled(activeRef.current);
    await summarizeTitle(activeRef.current);
    const cur = sessionsRef.current.find((s) => s.id === activeRef.current);
    addToast(cur?.title && cur.title !== '新会话'
      ? t('标题已更新')
      : t('未能生成标题（检查渠道与模型配置）'));
  };

  useEffect(() => {
    if (loading || !loadedRef.current || !activeId) return;
    if (busyRef.current) return;
    if (messages.length < 2) return;
    if (!messages.some((m) => m.role === 'assistant' && m.content.trim())) return;
    const cur = sessionsRef.current.find((s) => s.id === activeId);
    if (!cur || (cur.title && cur.title !== '新会话')) return;
    if (titledRef.current.has(activeId)) return;
    markTitled(activeId);
    void summarizeTitle(activeId);
  }, [messages, activeId, loading, markTitled, summarizeTitle]);

  /** 一次性迁移旧 localStorage 会话到后端（成功后清旧键） */
  const importLegacySessions = useCallback(async (): Promise<PlaygroundSession[]> => {
    const legacyList = readSessions();
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
          await api.replaceChatMessages(
            id,
            s.messages.map((m) => ({ role: m.role, content: m.content })),
            false,
          );
        }
        // 单位统一：后端会话摘要的 updated_at 是「秒」（渲染时 ×1000），
        // 旧 localStorage 数据是 ms —— 入库与本地 state 都按秒存
        imported.push({ id, title: s.title || '新会话', model: s.model, updated_at: updatedSec > 0 ? updatedSec : Math.floor(Date.now() / 1000) });
      } catch {
        /* 单条失败跳过，不阻塞其余 */
      }
    }
    try {
      localStorage.removeItem(SESSIONS_KEY);
      localStorage.removeItem(ACTIVE_KEY);
      localStorage.removeItem(LEGACY_KEY);
    } catch { /* noop */ }
    return imported;
  }, []);

  // 首启：拉后端会话列表（空则一次性导入旧 localStorage 会话），定位活动会话
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      let list: PlaygroundSession[] = [];
      try {
        const res = await api.listChatSessions();
        list = (res.data ?? []).map((s) => ({
          id: s.id,
          title: s.title,
          model: s.model,
          updated_at: s.updated_at,
        }));
      } catch {
        list = [];
      }
      if (!list.length) {
        list = await importLegacySessions();
      }
      if (cancelled) return;
      let active = '';
      try { active = localStorage.getItem(ACTIVE_KEY) ?? ''; } catch { /* noop */ }
      if (!list.some((s) => s.id === active)) active = list[0]?.id ?? '';
      sessionsRef.current = list;
      if (!active) {
        // 一个会话都没有：建一个新的（首条消息落盘时再自动命名）
        let freshId = uid();
        try {
          const created = await api.createChatSession({ model: '' });
          freshId = created.data?.id ?? freshId;
        } catch {
          // 后端不可用时退化为本地临时会话（不持久化）
        }
        if (cancelled) return;
        const fresh = { id: freshId, title: '新会话', model: '', updated_at: Math.floor(Date.now() / 1000) };
        persistSessions([fresh]);
        activeRef.current = freshId;
        messagesRef.current = [];
        setActiveId(freshId);
        setMessages([]);
        loadedRef.current = true;
        setLoading(false);
        return;
      }
      // 载入活动会话的消息
      let msgs: DebugMessage[] = [];
      let model = '';
      try {
        const detail = await api.getChatSession(active);
        msgs = (detail.data?.messages ?? [])
          .filter((m) => isDebugMessage(m))
          .map((m) => ({ role: m.role, content: m.content }));
        model = detail.data?.session.model ?? '';
      } catch {
        msgs = [];
      }
      if (cancelled) return;
      activeRef.current = active;
      messagesRef.current = msgs;
      modelRef.current = model;
      // 关键：把已加载的会话列表写进 state。`sessionsRef` 只是渲染期读的快照，
      // 不 setState 的话抽屉永远显示空列表（历史 bug：加载 30 条会话但列表空白）。
      persistSessions(list);
      setActiveId(active);
      setMessages(msgs);
      setModel(model);
      loadedRef.current = true;
      setLoading(false);
    })();
    return () => { cancelled = true; };
  }, [importLegacySessions]);

  // debounce 持久化（活动会话整表替换到后端）
  useEffect(() => {
    messagesRef.current = messages;
    if (!loadedRef.current) return;
    if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      saveTimer.current = null;
      void saveActiveNow();
    }, 500);
  }, [messages, saveActiveNow]);

  // 卸载时立即落盘
  useEffect(() => () => {
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
      void saveActiveNow();
    }
  }, [saveActiveNow]);

  const suggestions = useMemo(() => {
    const custom = loadSuggestionPrompts();
    if (custom.length) {
      return custom.map((s, i) => ({
        icon: DEFAULT_PROMPTS[i % DEFAULT_PROMPTS.length].icon,
        title: s.title,
        sub: s.sub,
        content: s.content,
      }));
    }
    return DEFAULT_PROMPTS.map((p) => ({ ...p, title: p.content, sub: t('提示词') }));
  }, [t]);

  const handleMessagesChange = (next: DebugMessage[]): void => {
    setMessages(next);
  };

  const handleModelChange = (next: string): void => {
    modelRef.current = next;
    setModel(next);
  };

  const handleClear = (): void => {
    messagesRef.current = [];
    setMessages([]);
    void saveActiveNow();
  };

  /** 切换会话：先落盘当前会话，再从后端载入目标会话（ChatDebugger 以 key 重挂载） */
  const switchSession = async (id: string): Promise<void> => {
    if (id === activeRef.current) {
      setDrawerOpen(false);
      return;
    }
    // 生成中切换会把半截回复落盘到当前会话（流式中间态），直接拦截。
    // 拦截也收起抽屉：sidebar 变体点项不再自动关，窄屏覆盖层形态下抽屉
    // 不收起会盖住 toast 与「历史会话」按钮，后续操作全被挡。
    if (guardBusy()) {
      setDrawerOpen(false);
      return;
    }
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    await saveActiveNow();
    let msgs: DebugMessage[] = [];
    let model = '';
    try {
      const detail = await api.getChatSession(id);
      msgs = (detail.data?.messages ?? [])
        .filter((m) => isDebugMessage(m))
        .map((m) => ({ role: m.role, content: m.content }));
      model = detail.data?.session.model ?? '';
    } catch {
      msgs = [];
    }
    activeRef.current = id;
    messagesRef.current = msgs;
    modelRef.current = model;
    setActiveId(id);
    setMessages(msgs);
    setModel(model);
    setDrawerOpen(false);
  };

  /** 新建会话（后端创建，消息清空） */
  const createSession = async (): Promise<void> => {
    // 拦截同样收起抽屉（理由见 switchSession）
    if (guardBusy()) {
      setDrawerOpen(false);
      return;
    }
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    await saveActiveNow();
    let freshId = uid();
    try {
      const created = await api.createChatSession({ model: modelRef.current });
      freshId = created.data?.id ?? freshId;
    } catch {
      // 后端不可用：本地临时会话兜底
    }
    persistSessions([{ id: freshId, title: '新会话', model: modelRef.current, updated_at: Math.floor(Date.now() / 1000) }, ...sessionsRef.current]);
    activeRef.current = freshId;
    messagesRef.current = [];
    setActiveId(freshId);
    setMessages([]);
    setDrawerOpen(false);
  };

  /** 删除会话（二次确认由共享 SessionDrawer 内建） */
  const deleteSession = async (id: string): Promise<void> => {
    if (guardBusy()) return;
    try {
      await api.deleteChatSession(id);
    } catch {
      /* 已不存在等情况忽略 */
    }
    if (id !== activeRef.current) {
      if (saveTimer.current !== null) {
        window.clearTimeout(saveTimer.current);
        saveTimer.current = null;
      }
      await saveActiveNow();
    }
    const rest = sessionsRef.current.filter((s) => s.id !== id);
    if (id === activeRef.current) {
      if (!rest.length) {
        // 删的是最后一个会话：先把 ref 清空再新建，否则 createSession 会
        // 拿含刚删会话的 sessionsRef 重建列表，被删的会话「复活」。
        persistSessions([]);
        activeRef.current = '';
        messagesRef.current = [];
        await createSession();
        return;
      }
      const target = rest[0];
      activeRef.current = target.id;
      messagesRef.current = [];
      modelRef.current = target.model;
      setActiveId(target.id);
      setMessages([]);
      setModel(target.model);
      try {
        const detail = await api.getChatSession(target.id);
        const msgs = (detail.data?.messages ?? [])
          .filter((m) => isDebugMessage(m))
          .map((m) => ({ role: m.role, content: m.content }));
        messagesRef.current = msgs;
        setMessages(msgs);
      } catch { /* 空消息列表兜底 */ }
    }
    persistSessions(rest);
  };

  return (
    <div className="pg-shell">
      {/* 历史会话抽屉（共享 SessionDrawer：sidebar 变体，对齐 AI 运维页）——
          内嵌在 .pg-shell 左端把消息区往右推，不遮挡全局侧边栏；
          窄屏（<900px）由组件 CSS 自动降级为 fixed 覆盖层。切换/新建会话后
          由 switchSession/createSession 里的 setDrawerOpen(false) 收起。 */}
      <SessionDrawer
        variant="sidebar"
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        title={t('历史会话')}
        sessions={sessions.map((s) => ({
          id: s.id,
          title: s.title || t('新会话'),
          meta: new Date(s.updated_at * 1000).toLocaleDateString(),
        }))}
        activeId={activeId}
        onSelect={(id) => void switchSession(id)}
        onCreate={() => void createSession()}
        onDelete={(id) => void deleteSession(id)}
        headerExtra={(
          <span className="sd-title-cfg">
            <button
              type="button"
              className="sd-title-gear"
              title={t('标题总结设置')}
              onClick={() => setTitleCfgOpen((v) => !v)}
            >
              <SlidersHorizontal size={13} />
            </button>
            {titleCfgOpen && (
              <div className="sd-title-pop">
                <label>{t('标题模型（留空 = 渠道默认首选）')}</label>
                <input
                  className="form-input"
                  placeholder="gpt-4o-mini"
                  value={cfgDraft.titleModel ?? ''}
                  onChange={(e) => setCfgDraft({ ...cfgDraft, titleModel: e.target.value })}
                />
                <label>{t('标题提示词')}</label>
                <textarea
                  className="form-input"
                  rows={3}
                  value={cfgDraft.titlePrompt ?? ''}
                  placeholder={DEFAULT_TITLE_PROMPT}
                  onChange={(e) => setCfgDraft({ ...cfgDraft, titlePrompt: e.target.value })}
                />
                <div className="sd-title-pop-actions">
                  <button type="button" className="btn btn-secondary btn-sm" onClick={saveTitleCfg}>
                    {t('保存')}
                  </button>
                  <button type="button" className="btn btn-secondary btn-sm" onClick={() => void manualTitle()}>
                    {t('立即总结当前会话')}
                  </button>
                </div>
                <p className="sd-title-pop-hint">
                  {t('首轮对话结束后自动总结一次标题；失败保持原标题，可在此手动重试。')}
                </p>
              </div>
            )}
          </span>
        )}
      />
      <ChatDebugger
        key={loading ? 'loading' : activeId}
        playground
        playgroundModel={model}
        onPlaygroundModelChange={handleModelChange}
        initialMessages={loading ? [] : messages}
        onMessagesChange={handleMessagesChange}
        suggestionPrompts={suggestions}
        onClearMessages={handleClear}
        hasMessages={messages.length > 0}
        onOpenSessions={() => setDrawerOpen((v) => !v)}
        onBusyChange={(b) => { busyRef.current = b; }}
        onForkMessage={(idx) => void forkSession(idx)}
      />
    </div>
  );
}
