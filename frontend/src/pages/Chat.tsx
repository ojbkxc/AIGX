import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  BarChart3, Code2, GraduationCap, ClipboardList, History, MessageSquarePlus, Trash2, X,
} from 'lucide-react';
import ChatDebugger, { type DebugMessage } from '../components/ChatDebugger';
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
/** 本机最多保留的会话数（localStorage 有 5MB 上限，防无限增长）。 */
const MAX_SESSIONS = 50;

/** 一条游乐园会话（消息 + 模型快照，updated_at 用于列表排序） */
interface PlaygroundSession {
  id: string;
  title: string;
  model: string;
  messages: DebugMessage[];
  updated_at: number;
}

const uid = (): string => `pg-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;

const isDebugMessage = (m: unknown): m is DebugMessage => Boolean(
  m && typeof m === 'object' && ((m as DebugMessage).role === 'user' || (m as DebugMessage).role === 'assistant'),
);

/** 用首条用户消息生成会话标题（截 30 字符） */
function autoTitle(messages: DebugMessage[]): string {
  const first = messages.find((m) => m.role === 'user' && m.content.trim());
  const text = first?.content.trim() ?? '';
  return text.length > 30 ? `${text.slice(0, 30)}…` : text;
}

function emptySession(model = ''): PlaygroundSession {
  return { id: uid(), title: '', model, messages: [], updated_at: Date.now() };
}

function readSessions(): PlaygroundSession[] {
  try {
    const raw = localStorage.getItem(SESSIONS_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((s): s is PlaygroundSession => Boolean(s && typeof s === 'object' && typeof (s as PlaygroundSession).id === 'string'))
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
  const [loading, setLoading] = useState(true);
  const saveTimer = useRef<number | null>(null);
  const loadedRef = useRef(false);
  const sessionsRef = useRef<PlaygroundSession[]>([]);
  const activeRef = useRef('');
  const messagesRef = useRef<DebugMessage[]>([]);
  const modelRef = useRef('');

  /** 会话列表：state + ref + localStorage 三处同步 */
  const persistSessions = (next: PlaygroundSession[]): void => {
    sessionsRef.current = next;
    setSessions(next);
    try {
      localStorage.setItem(SESSIONS_KEY, JSON.stringify(next));
    } catch { /* 存储写满静默降级 */ }
  };

  /** 把当前消息/模型写回活动会话（标题为空时用首条用户消息自动命名） */
  // 全量状态读的都是 ref + setState，依赖恒为空，可安全稳定引用，
  // 供 debounce/卸载两个 effect 直接依赖而不触发重跑。
  const saveActiveNow = useCallback((): void => {
    const id = activeRef.current;
    if (!id) return;
    const next = sessionsRef.current.map((s) => (s.id === id
      ? {
        ...s,
        messages: messagesRef.current,
        model: modelRef.current,
        title: s.title || autoTitle(messagesRef.current),
        updated_at: Date.now(),
      }
      : s));
    persistSessions(next);
  }, []);

  // 首启：恢复多会话列表（无则迁移旧单会话），定位活动会话
  useEffect(() => {
    let cancelled = false;
    const timer = window.setTimeout(() => {
      let list = readSessions();
      let active = '';
      try { active = localStorage.getItem(ACTIVE_KEY) ?? ''; } catch { /* noop */ }
      if (!list.length) {
        let legacy: DebugMessage[] = [];
        try {
          const raw = localStorage.getItem(LEGACY_KEY);
          const parsed = raw ? JSON.parse(raw) as unknown : [];
          if (Array.isArray(parsed)) legacy = parsed.filter(isDebugMessage);
        } catch { /* 损坏数据静默丢弃 */ }
        const migrated: PlaygroundSession = {
          id: uid(),
          title: autoTitle(legacy),
          model: '',
          messages: legacy,
          updated_at: Date.now(),
        };
        list = [migrated];
        active = migrated.id;
        try { localStorage.removeItem(LEGACY_KEY); } catch { /* noop */ }
      }
      if (!list.some((s) => s.id === active)) active = list[0].id;
      const current = list.find((s) => s.id === active) ?? list[0];
      if (cancelled) return;
      sessionsRef.current = list;
      activeRef.current = current.id;
      messagesRef.current = current.messages;
      modelRef.current = current.model;
      setSessions(list);
      setActiveId(current.id);
      setMessages(current.messages);
      setModel(current.model);
      loadedRef.current = true;
      setLoading(false);
      try {
        localStorage.setItem(SESSIONS_KEY, JSON.stringify(list));
        localStorage.setItem(ACTIVE_KEY, current.id);
      } catch { /* noop */ }
    }, 0);
    return () => { cancelled = true; window.clearTimeout(timer); };
  }, []);

  // debounce 持久化（活动会话）
  useEffect(() => {
    messagesRef.current = messages;
    if (!loadedRef.current) return;
    if (saveTimer.current !== null) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(() => {
      saveTimer.current = null;
      saveActiveNow();
    }, 500);
  }, [messages, saveActiveNow]);

  // 卸载时立即落盘
  useEffect(() => () => {
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
      saveActiveNow();
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
    saveActiveNow();
  };

  const handleClear = (): void => {
    messagesRef.current = [];
    setMessages([]);
    saveActiveNow();
  };

  /** 切换会话：先落盘当前会话，再载入目标会话（ChatDebugger 以 key 重挂载） */
  const switchSession = (id: string): void => {
    if (id === activeRef.current) {
      setDrawerOpen(false);
      return;
    }
    const target = sessionsRef.current.find((s) => s.id === id);
    if (!target) return;
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    saveActiveNow();
    activeRef.current = id;
    messagesRef.current = target.messages;
    modelRef.current = target.model;
    setActiveId(id);
    setMessages(target.messages);
    setModel(target.model);
    try { localStorage.setItem(ACTIVE_KEY, id); } catch { /* noop */ }
    setDrawerOpen(false);
  };

  /** 新建会话（沿用当前模型，消息清空） */
  const createSession = (): void => {
    if (saveTimer.current !== null) {
      window.clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    saveActiveNow();
    const created = emptySession(modelRef.current);
    persistSessions([created, ...sessionsRef.current].slice(0, MAX_SESSIONS));
    activeRef.current = created.id;
    messagesRef.current = [];
    setActiveId(created.id);
    setMessages([]);
    try { localStorage.setItem(ACTIVE_KEY, created.id); } catch { /* noop */ }
    setDrawerOpen(false);
  };

  /** 删除会话：删的是活动会话时切到最近一条，空列表则补一条新会话 */
  const deleteSession = (id: string): void => {
    const rest = sessionsRef.current.filter((s) => s.id !== id);
    if (id === activeRef.current) {
      if (!rest.length) rest.push(emptySession(modelRef.current));
      const target = rest[0];
      activeRef.current = target.id;
      messagesRef.current = target.messages;
      modelRef.current = target.model;
      setActiveId(target.id);
      setMessages(target.messages);
      setModel(target.model);
      try { localStorage.setItem(ACTIVE_KEY, target.id); } catch { /* noop */ }
    }
    persistSessions(rest);
  };

  return (
    <div className="pg-shell">
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
        onOpenSessions={() => setDrawerOpen(true)}
      />

      {/* 历史会话抽屉（对齐运维页会话列表观感） */}
      <aside className={`pg-sessions ${drawerOpen ? 'open' : ''}`}>
        <div className="pg-sessions-head">
          <span><History size={14} /> {t('历史会话')}</span>
          <button type="button" className="pg-sessions-close" title={t('关闭')} onClick={() => setDrawerOpen(false)}>
            <X size={14} />
          </button>
        </div>
        <button type="button" className="pg-sessions-new" onClick={createSession}>
          <MessageSquarePlus size={14} />
          {t('新会话')}
        </button>
        <div className="pg-sessions-list">
          {sessions.length === 0 && <div className="pg-sessions-empty">{t('暂无会话')}</div>}
          {sessions.map((s) => (
            <div key={s.id} className={`pg-sessions-item ${s.id === activeId ? 'active' : ''}`}>
              <button type="button" className="pg-sessions-item-btn" title={s.title || t('新会话')} onClick={() => switchSession(s.id)}>
                <span className="pg-sessions-item-title">{s.title || t('新会话')}</span>
                <span className="pg-sessions-item-meta">
                  {t('{{count}} 条消息', { count: s.messages.length })} · {new Date(s.updated_at).toLocaleDateString()}
                </span>
              </button>
              <button type="button" className="pg-sessions-item-del" title={t('删除')} onClick={() => deleteSession(s.id)}>
                <Trash2 size={12} />
              </button>
            </div>
          ))}
        </div>
      </aside>
      {drawerOpen && <div className="pg-sessions-mask" onClick={() => setDrawerOpen(false)} />}
    </div>
  );
}
