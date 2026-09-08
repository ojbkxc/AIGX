import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { MessageSquare, Plus, Trash2, PanelLeftClose, PanelLeftOpen } from 'lucide-react';
import ChatDebugger, { type DebugMessage } from '../components/ChatDebugger';
import './Chat.css';

interface ChatSession {
  id: string;
  title: string;
  messages: DebugMessage[];
  updated_at: number;
}

const STORAGE_KEY = 'aigx_chat_sessions';

function loadSessions(): ChatSession[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((s): s is ChatSession =>
      Boolean(s && typeof s === 'object' && (s as ChatSession).id && Array.isArray((s as ChatSession).messages)),
    );
  } catch {
    return [];
  }
}

function saveSessions(sessions: ChatSession[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(sessions));
  } catch {
    // 本地存储写满/被禁用时静默降级
  }
}

function autoTitle(messages: DebugMessage[]): string {
  const first = messages.find((m) => m.role === 'user' && m.content.trim());
  const text = (first?.content ?? '').trim().replace(/\s+/g, ' ');
  return text ? text.slice(0, 20) : '';
}

/** 行 ID：secure context 用 crypto.randomUUID，http 环境回退时间戳随机串 */
function genId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function newSession(now: number): ChatSession {
  return { id: genId(), title: '', messages: [], updated_at: now };
}

/**
 * Chat — 用户聊天工作区（P0）。
 *
 * 左侧会话列表（新建/切换/删除，localStorage 持久化），右侧复用
 * ChatDebugger（hideToolbar 精简形态）。审美参照 open-webui：
 * 窄边栏 + 居中对话流 + 顶部悬浮模型 pill。
 */
/** 读取「已启用」提示词并映射为 ChatDebugger 的空状态建议卡片 */
function loadSuggestionPrompts(): Array<{ title: string; sub: string; content: string }> {
  try {
    const raw = localStorage.getItem('aigx_prompts');
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((p): p is { id: string; name: string; content: string; tags?: string[]; enabled?: boolean } =>
        Boolean(p && typeof p === 'object' && (p as { enabled?: boolean }).enabled !== false),
      )
      .slice(0, 6)
      .map((p) => ({
        title: p.name || 'Prompt',
        sub: (p.tags ?? []).slice(0, 3).join(' · '),
        content: p.content,
      }))
      .filter((p) => p.content);
  } catch {
    return [];
  }
}

export default function Chat(): JSX.Element {
  const { t } = useTranslation();
  const [sessions, setSessions] = useState<ChatSession[]>(() => {
    const loaded = loadSessions();
    return loaded.length ? loaded : [newSession(Date.now())];
  });
  const [activeId, setActiveId] = useState<string>(() => loadSessions()[0]?.id ?? '');
  const [sidebarOpen, setSidebarOpen] = useState<boolean>(() => window.innerWidth > 900);

  const active = useMemo(
    () => sessions.find((s) => s.id === activeId) ?? sessions[0],
    [sessions, activeId],
  );

  // 首次挂载兜底：本地有历史但没有可激活项时选中第一条
  useEffect(() => {
    if (!active && sessions.length) setActiveId(sessions[0].id);
  }, [active, sessions]);

  // 消息变化持久化 + 会话按更新时间排序 + 首条用户消息自动标题
  const handleMessagesChange = (messages: DebugMessage[]): void => {
    setSessions((prev) => {
      const current = prev.find((s) => s.id === activeId);
      if (!current) return prev;
      const title = current.title || autoTitle(messages);
      const next = prev
        .map((s) => (s.id === activeId
          ? { ...s, title: title || s.title, messages, updated_at: Date.now() }
          : s))
        .sort((a, b) => b.updated_at - a.updated_at);
      saveSessions(next);
      return next;
    });
  };

  const handleNew = (): void => {
    const session = newSession(Date.now());
    setSessions((prev) => {
      const next = [session, ...prev].sort((a, b) => b.updated_at - a.updated_at);
      saveSessions(next);
      return next;
    });
    setActiveId(session.id);
  };

  const handleDelete = (id: string): void => {
    setSessions((prev) => {
      const next = prev.filter((s) => s.id !== id);
      if (!next.length) next.push(newSession(Date.now()));
      saveSessions(next);
      if (id === activeId) {
        const fallback = next.find((s) => s.id !== id) ?? next[0];
        setActiveId(fallback.id);
      }
      return next;
    });
  };

  const handleSelect = (id: string): void => {
    setActiveId(id);
    if (window.innerWidth <= 900) setSidebarOpen(false);
  };

  return (
    <div className={`chat-shell ${sidebarOpen ? 'chat-shell-sidebar-open' : 'chat-shell-sidebar-closed'}`}>
      <aside className="chat-sidebar">
        <div className="chat-sidebar-head">
          <button
            type="button"
            className="btn btn-outline btn-sm chat-sidebar-close"
            title={t('收起会话列表')}
            onClick={() => setSidebarOpen(false)}
          >
            <PanelLeftClose size={14} />
          </button>
          <button type="button" className="btn btn-primary btn-sm chat-new-btn" onClick={handleNew}>
            <Plus size={14} />
            {t('新会话')}
          </button>
        </div>
        <div className="chat-sidebar-list">
          {sessions.length === 0 && (
            <div className="chat-sidebar-empty">{t('暂无会话')}</div>
          )}
          {sessions.map((s) => (
            <div
              key={s.id}
              className={`chat-session ${s.id === (active?.id ?? '') ? 'active' : ''}`}
              role="button"
              tabIndex={0}
              onClick={() => handleSelect(s.id)}
              onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); handleSelect(s.id); } }}
            >
              <MessageSquare size={14} className="chat-session-icon" />
              <span className="chat-session-title">{s.title || t('新会话')}</span>
              <button
                type="button"
                className="chat-session-delete"
                title={t('删除会话')}
                onClick={(e) => { e.stopPropagation(); handleDelete(s.id); }}
              >
                <Trash2 size={13} />
              </button>
            </div>
          ))}
        </div>
      </aside>

      <div className="chat-overlay" onClick={() => setSidebarOpen(false)} />

      <main className="chat-main">
        {!sidebarOpen && (
          <button
            type="button"
            className="btn btn-outline btn-sm chat-sidebar-open"
            title={t('展开会话列表')}
            onClick={() => setSidebarOpen(true)}
          >
            <PanelLeftOpen size={14} />
          </button>
        )}
        {active && (
          <ChatDebugger
            key={active.id}
            initialMessages={active.messages}
            onMessagesChange={handleMessagesChange}
            hideToolbar
            suggestionPrompts={loadSuggestionPrompts()}
          />
        )}
      </main>
    </div>
  );
}
