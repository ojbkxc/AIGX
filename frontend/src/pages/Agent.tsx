import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Bot, Send, Plus, Square, Trash2, ShieldAlert, Terminal, Eye, Wrench,
  CheckCircle2, XCircle, Loader2, MessageSquarePlus,
} from 'lucide-react';
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
  tool_calls?: { type: string; name?: string; arguments?: string; ok?: boolean; text?: string; request_id?: string; approved?: boolean }[] | null;
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

/** 工具轨迹步骤（决策回放用） */
interface ToolStep {
  name: string;
  args?: string;
  ok?: boolean;
  text?: string;
  kind: 'call' | 'approval' | 'result';
  approved?: boolean;
}

interface ChatMsg {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  toolSteps?: ToolStep[];
}

const fmtSessionTime = (ts: number): string => {
  const d = new Date(ts * 1000);
  const now = Date.now();
  const diff = now - d.getTime();
  if (diff < 60_000) return '刚刚';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  return d.toLocaleDateString();
};

/** 高危工具的中文描述（审批弹卡展示用，参照 cc-haha 的 PermissionRequestTitle 语义） */
const TOOL_LABELS: Record<string, string> = {
  aigx_user_delete: '删除用户（不可逆）',
  aigx_user_manage: '启用/禁用用户',
  aigx_channel_delete: '删除渠道（不可逆）',
  aigx_order_delete: '删除订单',
  aigx_pricing_upsert: '新增/更新模型定价',
  aigx_pricing_delete: '删除模型定价',
  aigx_logs_cleanup: '清理日志',
  aigx_key_delete: '删除 API 密钥（不可逆）',
  aigx_group_upsert: '新增/更新用户分组',
};

export default function Agent(): JSX.Element {
  const { t } = useTranslation();
  const [sessions, setSessions] = useState<AgentSession[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [pending, setPending] = useState<PendingApproval | null>(null);
  const [deleteConfirm, setDeleteConfirm] = useState<AgentSession | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const msgIdRef = useRef(0);
  const bottomRef = useRef<HTMLDivElement | null>(null);

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

  // 加载会话详情（含工具轨迹回放）
  const loadSession = useCallback(async (id: string) => {
    setActiveId(id);
    setMessages([]);
    try {
      const res = await api.getAgentSession(id);
      const data = (res as unknown as { data?: { messages?: AgentMsg[] } }).data;
      const msgs = (data?.messages ?? []).map((m) => {
        // tool_calls 数组即决策回放轨迹（tool_call/approval/tool_result 混排）
        const steps: ToolStep[] = (m.tool_calls ?? []).map((tc) => {
          if (tc.type === 'tool_call') return { kind: 'call', name: tc.name ?? '', args: tc.arguments, ok: true };
          if (tc.type === 'approval_request') return { kind: 'approval', name: tc.name ?? '', args: tc.arguments };
          if (tc.type === 'approval_resolved') return { kind: 'approval', name: tc.name ?? '', approved: tc.approved };
          return { kind: 'result', name: tc.name ?? '', ok: tc.ok, text: tc.text };
        });
        return {
          id: String(++msgIdRef.current),
          role: (m.role === 'assistant' ? 'assistant' : 'user') as ChatMsg['role'],
          content: m.content,
          toolSteps: steps.length > 0 ? steps : undefined,
        };
      });
      setMessages(msgs);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const newSession = useCallback(async () => {
    try {
      const res = await api.createAgentSession('');
      const data = (res as unknown as { data?: { id: string } }).data;
      if (data) {
        await loadSessions();
        await loadSession(data.id);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [loadSessions, loadSession]);

  const removeSession = useCallback(async (s: AgentSession) => {
    setDeleteConfirm(null);
    try {
      await api.deleteAgentSession(s.id);
      if (activeId === s.id) {
        setActiveId(null);
        setMessages([]);
      }
      await loadSessions();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [activeId, loadSessions]);

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
            const pushStep = (step: ToolStep): ChatMsg => ({
              ...m,
              toolSteps: [...(m.toolSteps ?? []), step],
            });
            if (ev.type === 'final' && typeof ev.content === 'string') {
              return { ...m, content: m.content + ev.content };
            }
            if (ev.type === 'error' && typeof ev.message === 'string') {
              return { ...m, content: m.content + `\n⚠️ ${ev.message}` };
            }
            if (ev.type === 'tool_call' && typeof ev.name === 'string') {
              return pushStep({ kind: 'call', name: ev.name, args: ev.arguments, ok: true });
            }
            if (ev.type === 'approval_resolved' && typeof ev.name === 'string') {
              return pushStep({ kind: 'approval', name: ev.name, approved: ev.approved !== false });
            }
            if (ev.type === 'tool_result' && typeof ev.name === 'string') {
              return pushStep({ kind: 'result', name: ev.name, ok: ev.ok === true, text: ev.text });
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

  // 消息流自动滚底
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages]);

  const activeSession = sessions.find((s) => s.id === activeId);

  /** 工具轨迹步骤渲染（决策回放：调用 → 审批 → 结果） */
  const renderStep = (step: ToolStep, i: number): ReactNode => {
    if (step.kind === 'call') {
      return (
        <div className="agent-step agent-step-call" key={i}>
          <Wrench size={12} />
          <code>{step.name}</code>
          {step.args && step.args !== '{}' && (
            <span className="agent-step-args" title={step.args}>{step.args}</span>
          )}
        </div>
      );
    }
    if (step.kind === 'approval') {
      return (
        <div className={`agent-step agent-step-approval ${step.approved === false ? 'denied' : ''}`} key={i}>
          <ShieldAlert size={12} />
          <span>{t('人工审批')}{step.approved === undefined ? '' : step.approved ? t('通过') : t('拒绝')}</span>
        </div>
      );
    }
    return (
      <div className={`agent-step agent-step-result ${step.ok ? 'ok' : 'fail'}`} key={i}>
        {step.ok ? <CheckCircle2 size={12} /> : <XCircle size={12} />}
        <span>{step.name}</span>
        {step.text && (
          <details className="agent-step-detail">
            <summary>{t('结果')}</summary>
            <pre>{step.text.length > 400 ? `${step.text.slice(0, 400)}…` : step.text}</pre>
          </details>
        )}
      </div>
    );
  };

  return (
    <div className="agent-shell">
      {/* 左栏：会话列表（open-webui 侧栏观感） */}
      <aside className="agent-sessions">
        <div className="agent-sessions-head">
          <span className="agent-sessions-title">
            <Bot size={15} />
            {t('AI 运维会话')}
          </span>
          <button type="button" className="agent-icon-btn" onClick={() => void newSession()} title={t('新建会话')}>
            <Plus size={15} />
          </button>
        </div>
        <div className="agent-sessions-list">
          {sessions.map((s) => (
            <div
              key={s.id}
              role="button"
              tabIndex={0}
              className={`agent-session-item ${s.id === activeId ? 'active' : ''}`}
              onClick={() => void loadSession(s.id)}
              onKeyDown={(e) => { if (e.key === 'Enter') void loadSession(s.id); }}
            >
              <span className="agent-session-title">{s.title}</span>
              <span className="agent-session-meta">
                <span className="agent-session-time">{fmtSessionTime(s.created_at)}</span>
                {s.role === 'operator' && (
                  <span className="agent-session-role operator"><Terminal size={10} />{t('运维员')}</span>
                )}
              </span>
              <button
                type="button"
                className="agent-session-del"
                title={t('删除会话')}
                onClick={(e) => { e.stopPropagation(); setDeleteConfirm(s); }}
              >
                <Trash2 size={12} />
              </button>
            </div>
          ))}
          {sessions.length === 0 && <div className="agent-sessions-empty">{t('暂无会话')}</div>}
        </div>
      </aside>

      {/* 右栏：消息流 + 输入（open-webui 居中 58rem 消息流） */}
      <main className="agent-main">
        <div className="agent-messages">
          {messages.map((m) => (
            <div key={m.id} className={`agent-msg agent-msg-${m.role}`}>
              <span className="agent-msg-icon">
                {m.role === 'user' ? <Terminal size={14} /> : <Bot size={14} />}
              </span>
              <div className="agent-msg-body">
                {m.toolSteps && m.toolSteps.length > 0 && (
                  <div className="agent-tool-steps">
                    {m.toolSteps.map(renderStep)}
                  </div>
                )}
                <div className="agent-msg-content">{m.content || (busy ? '' : '')}</div>
              </div>
            </div>
          ))}
          {busy && (
            <div className="agent-msg agent-msg-assistant">
              <span className="agent-msg-icon"><Bot size={14} /></span>
              <div className="agent-msg-body">
                <div className="agent-typing"><Loader2 size={13} className="agent-spin" />{t('思考中…')}</div>
              </div>
            </div>
          )}
          {messages.length === 0 && !busy && (
            <div className="agent-empty">
              <div className="agent-empty-title">{t('AI 运维工作台')}</div>
              <p className="agent-empty-sub">{t('用 AI 运维你的网关——查渠道、看用户、测健康、排故障')}</p>
              <div className="agent-empty-prompts">
                {[
                  { icon: <Terminal size={13} />, text: t('看下系统状态，哪些渠道挂了？') },
                  { icon: <Eye size={13} />, text: t('今天的用量与成本报表') },
                  { icon: <MessageSquarePlus size={13} />, text: t('列出全部用户和密钥') },
                ].map((p, i) => (
                  <button
                    key={i}
                    type="button"
                    className="agent-empty-prompt"
                    onClick={() => { setInput(p.text); }}
                  >
                    {p.icon}<span>{p.text}</span>
                  </button>
                ))}
              </div>
            </div>
          )}
          <div ref={bottomRef} />
        </div>

        {error && <div className="agent-error">{error}<button type="button" onClick={() => setError('')}>×</button></div>}

        <div className="agent-input-row">
          <div className="agent-input-shell">
            <textarea
              className="agent-input"
              value={input}
              rows={1}
              placeholder={activeId
                ? t('输入运维指令，例如：看下系统状态 / 哪些渠道挂了 / 今天用量')
                : t('先在左侧选择或新建会话')}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void send(); } }}
              disabled={!activeId}
            />
            <div className="agent-input-meta">
              <span className="agent-input-context">
                {activeSession
                  ? `${activeSession.model || '默认模型'}${activeSession.role === 'operator' ? ` · ${t('运维员')}` : ` · ${t('观察员')}`}`
                  : '—'}
              </span>
              {busy ? (
                <button type="button" className="agent-send-fab stop" onClick={stop} title={t('停止')}>
                  <Square size={14} />
                </button>
              ) : (
                <button
                  type="button"
                  className="agent-send-fab"
                  onClick={() => void send()}
                  disabled={!activeId || !input.trim()}
                  title={t('发送')}
                >
                  <Send size={15} />
                </button>
              )}
            </div>
          </div>
        </div>
      </main>

      {/* 审批弹卡（cc-haha PermissionDialog 语义：标题/工具/参数/三态选择） */}
      {pending && (
        <div className="agent-approval-mask" onClick={() => void respondApproval('deny')}>
          <div className="agent-approval-card" onClick={(e) => e.stopPropagation()}>
            <div className="agent-approval-head">
              <ShieldAlert size={18} className="agent-approval-icon" />
              <div>
                <div className="agent-approval-title">{t('高危操作需要审批')}</div>
                <div className="agent-approval-sub">{t('Agent 请求执行以下写操作，请确认')}</div>
              </div>
            </div>
            <div className="agent-approval-tool">
              <code className="agent-approval-name">{pending.name}</code>
              <span className="agent-approval-desc">{TOOL_LABELS[pending.name] ?? t('高危写操作')}</span>
            </div>
            {pending.arguments && pending.arguments !== '{}' && (
              <pre className="agent-approval-args">{pending.arguments}</pre>
            )}
            <div className="agent-approval-actions">
              <button type="button" className="agent-approval-deny" onClick={() => void respondApproval('deny')}>{t('拒绝')}</button>
              <button type="button" className="agent-approval-remember" onClick={() => void respondApproval('remember')}>{t('本会话总是允许')}</button>
              <button type="button" className="agent-approval-once" onClick={() => void respondApproval('allow')}>{t('允许一次')}</button>
            </div>
          </div>
        </div>
      )}

      {/* 删除会话确认 */}
      {deleteConfirm && (
        <div className="agent-approval-mask" onClick={() => setDeleteConfirm(null)}>
          <div className="agent-approval-card" onClick={(e) => e.stopPropagation()}>
            <div className="agent-approval-head">
              <Trash2 size={18} className="agent-approval-icon danger" />
              <div>
                <div className="agent-approval-title">{t('删除会话')}</div>
                <div className="agent-approval-sub">
                  {t('确定删除')}「{deleteConfirm.title}」？{t('该会话的全部消息将一并删除，不可恢复。')}
                </div>
              </div>
            </div>
            <div className="agent-approval-actions">
              <button type="button" className="agent-approval-deny" onClick={() => setDeleteConfirm(null)}>{t('取消')}</button>
              <button type="button" className="agent-approval-once" onClick={() => void removeSession(deleteConfirm)}>{t('删除')}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
