import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Bot, Send, Plus, RefreshCw } from 'lucide-react';
import { api } from '../api';
import './Agent.css';

/** 会话条目（后端 AgentSession 序列化形状） */
interface AgentSession {
  id: string;
  title: string;
  model: string;
  channel: string;
  role: string;
  created_at: number;
}

/** 单条消息（后端 AgentMessage 序列化形状） */
interface AgentMsg {
  role: string;
  content: string;
  tool_calls?: unknown;
  tool_result?: unknown;
}

/** SSE 推的事件（后端 AgentEvent 序列化形状） */
interface AgentEvent {
  type: string;
  turn?: number;
  name?: string;
  arguments?: string;
  ok?: boolean;
  text?: string;
  content?: string;
  message?: string;
  request_id?: string;
}

/** 待审批请求 */
interface PendingApproval {
  requestId: string;
  name: string;
  arguments: string;
}

interface ToolEvent {
  name: string;
  ok: boolean;
}

interface ChatMsg {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  toolEvents?: ToolEvent[];
}

export default function Agent(): JSX.Element {
  const { t } = useTranslation();
  const [sessions, setSessions] = useState<AgentSession[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [pending, setPending] = useState<PendingApproval | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const msgIdRef = useRef(0);

  const respondApproval = useCallback(async (action: 'allow' | 'deny' | 'remember') => {
    if (!pending) return;
    const rid = pending.requestId;
    setPending(null);
    try {
      await api.agentApprove(rid, action);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [pending]);

  // 加载会话列表
  const loadSessions = useCallback(async () => {
    try {
      const res = await api.listAgentSessions();
      const list = (res as unknown as { data?: AgentSession[] }).data ?? [];
      setSessions(list);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void loadSessions();
  }, [loadSessions]);

  // 加载会话详情
  const loadSession = useCallback(async (id: string) => {
    setActiveId(id);
    setMessages([]);
    try {
      const res = await api.getAgentSession(id);
      const data = (res as unknown as { data?: { messages?: AgentMsg[] } }).data;
      const msgs = (data?.messages ?? []).map((m) => ({
        id: String(++msgIdRef.current),
        role: (m.role === 'assistant' ? 'assistant' : 'user') as ChatMsg['role'],
        content: m.content,
      }));
      setMessages(msgs);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const newSession = useCallback(async () => {
    try {
      const res = await api.createAgentSession('');
      const data = (res as unknown as { data?: { id: string; title: string } }).data;
      if (data) {
        await loadSessions();
        await loadSession(data.id);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [loadSessions, loadSession]);

  const send = useCallback(async () => {
    const text = input.trim();
    if (!text || !activeId || busy) return;
    setInput('');
    setBusy(true);
    setError('');
    const userMsg: ChatMsg = { id: String(++msgIdRef.current), role: 'user', content: text };
    setMessages((prev) => [...prev, userMsg]);
    // 占位 assistant 气泡，流式往里填
    const asstId = String(++msgIdRef.current);
    setMessages((prev) => [...prev, { id: asstId, role: 'assistant', content: '' }]);

    const controller = new AbortController();
    abortRef.current = controller;
    try {
      await api.agentChatStream(activeId, text, (ev: AgentEvent) => {
        if (ev.type === 'approval_request' && typeof ev.request_id === 'string') {
          setPending({ requestId: ev.request_id, name: ev.name ?? '', arguments: ev.arguments ?? '' });
          return;
        }
        setMessages((prev) =>
          prev.map((m) => {
            if (m.id !== asstId) return m;
            if (ev.type === 'final' && typeof ev.content === 'string') {
              return { ...m, content: m.content + ev.content };
            }
            if (ev.type === 'error' && typeof ev.message === 'string') {
              return { ...m, content: m.content + `\n⚠️ ${ev.message}` };
            }
            if (ev.type === 'tool_call' && typeof ev.name === 'string') {
              const te = m.toolEvents ?? [];
              return { ...m, toolEvents: [...te, { name: ev.name, ok: true }] };
            }
            if (ev.type === 'tool_result' && typeof ev.name === 'string') {
              const te = m.toolEvents ? [...m.toolEvents] : [];
              if (te.length > 0) {
                te[te.length - 1] = { name: te[te.length - 1].name, ok: ev.ok === true };
              }
              return { ...m, toolEvents: te };
            }
            return m;
          }),
        );
      }, controller.signal);
    } catch (e) {
      if ((e as Error).name !== 'AbortError') {
        setError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      setBusy(false);
      abortRef.current = null;
    }
  }, [input, activeId, busy]);

  const stop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  return (
    <div className="agent-shell">
      {/* 左栏：会话列表 */}
      <aside className="agent-sessions">
        <div className="agent-sessions-head">
          <Bot size={16} />
          <span>{t('AI 运维会话')}</span>
          <button type="button" className="agent-new-btn" onClick={() => void newSession()} title={t('新建会话')}>
            <Plus size={15} />
          </button>
        </div>
        <div className="agent-sessions-list">
          {sessions.map((s) => (
            <button
              key={s.id}
              type="button"
              className={`agent-session-item ${s.id === activeId ? 'active' : ''}`}
              onClick={() => void loadSession(s.id)}
            >
              <span className="agent-session-title">{s.title}</span>
              <span className="agent-session-model">{s.model || '—'}</span>
            </button>
          ))}
          {sessions.length === 0 && <div className="agent-sessions-empty">{t('暂无会话')}</div>}
        </div>
      </aside>

      {/* 右栏：消息流 + 输入 */}
      <main className="agent-main">
        <div className="agent-messages">
          {messages.map((m) => (
            <div key={m.id} className={`agent-msg agent-msg-${m.role}`}>
              <div className="agent-msg-bubble">
                {m.toolEvents && m.toolEvents.length > 0 && (
                  <div className="agent-tool-row">
                    {m.toolEvents.map((te, i) => (
                      <span key={i} className={`agent-tool-chip ${te.ok ? 'ok' : 'fail'}`}>
                        {te.name}
                      </span>
                    ))}
                  </div>
                )}
                <div className="agent-msg-content">{m.content || (busy ? '…' : '')}</div>
              </div>
            </div>
          ))}
          {messages.length === 0 && (
            <div className="agent-empty">
              <Bot size={40} strokeWidth={1.2} />
              <p>{t('用 AI 运维你的网关——查渠道、看用户、测健康、排故障')}</p>
            </div>
          )}
        </div>

        {error && <div className="agent-error">{error}</div>}

        <div className="agent-input-row">
          <input
            className="agent-input"
            value={input}
            placeholder={t('输入运维指令，例如：看下系统状态 / 哪些渠道挂了 / 今天用量')}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void send(); } }}
            disabled={!activeId}
          />
          {busy ? (
            <button type="button" className="agent-stop-btn" onClick={stop} title={t('停止')}>
              <RefreshCw size={15} className="spin" />
            </button>
          ) : (
            <button type="button" className="agent-send-btn" onClick={() => void send()} disabled={!activeId || !input.trim()}>
              <Send size={15} />
            </button>
          )}
        </div>
      </main>

      {/* 审批弹层 */}
      {pending && (
        <div className="agent-approval-mask" onClick={() => void respondApproval('deny')}>
          <div className="agent-approval-card" onClick={(e) => e.stopPropagation()}>
            <div className="agent-approval-title">{t('高危操作审批')}</div>
            <div className="agent-approval-name">{pending.name}</div>
            <div className="agent-approval-args">{pending.arguments}</div>
            <div className="agent-approval-actions">
              <button type="button" className="agent-approval-deny" onClick={() => void respondApproval('deny')}>{t('拒绝')}</button>
              <button type="button" className="agent-approval-once" onClick={() => void respondApproval('allow')}>{t('允许一次')}</button>
              <button type="button" className="agent-approval-remember" onClick={() => void respondApproval('remember')}>{t('本会话总是允许')}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}