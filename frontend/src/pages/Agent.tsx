import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Bot, Send, Square, ShieldAlert, Terminal, Eye, Wrench,
  CheckCircle2, XCircle, Loader2, MessageSquarePlus, Trash2,
} from 'lucide-react';
import ModelPicker from '../components/ModelPicker';
import { api } from '../api';
import './Agent.css';

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
  approved?: boolean;
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

/** 高危工具的中文描述（审批弹卡展示用，参照 cc-haha 的 PermissionRequestTitle 语义） */
const TOOL_LABELS: Record<string, string> = {
  aigx_user_delete: '删除用户（不可逆）',
  aigx_user_manage: '启用/禁用用户',
  aigx_channel_delete: '删除渠道（不可逆）',
  aigx_channel_add: '新增渠道',
  aigx_channel_enable: '启用渠道',
  aigx_channel_disable: '禁用渠道',
  aigx_order_delete: '删除订单',
  aigx_pricing_upsert: '新增/更新模型定价',
  aigx_pricing_delete: '删除模型定价',
  aigx_logs_cleanup: '清理日志',
  aigx_key_delete: '删除 API 密钥（不可逆）',
  aigx_key_add: '新增 API 密钥',
  aigx_group_upsert: '新增/更新用户分组',
};

/**
 * Agent — AI 运维工作台（对齐 /chat 聊天页形态）。
 *
 * 无侧栏、无会话列表：单一对话流 + 底部输入区（new-api 游乐园布局）。
 * 会话是后端概念（上下文 + 审计载体），首次发送时懒创建，不暴露给用户。
 * 输入区底部工具行：[角色徽标 + 清空] ··· [模型选择器 + 发送/停止]。
 */
export default function Agent(): JSX.Element {
  const { t } = useTranslation();
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [pending, setPending] = useState<PendingApproval | null>(null);
  const [model, setModel] = useState('');
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [role, setRole] = useState<'observer' | 'operator'>('observer');
  const abortRef = useRef<AbortController | null>(null);
  const msgIdRef = useRef(0);
  const bottomRef = useRef<HTMLDivElement | null>(null);

  // 启动时取 Agent 配置的模型作为模型选择器初值（可换）
  useEffect(() => {
    let mounted = true;
    api.getAgentConfig()
      .then((res) => {
        if (mounted) setModel((res as unknown as { data?: { model?: string } }).data?.model ?? '');
      })
      .catch(() => { /* 配置读取失败静默降级：选择器空值，发送时走配置模型 */ });
    return () => { mounted = false; };
  }, []);

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

  const clearAll = useCallback(() => {
    setMessages([]);
    // 会话保留在服务端（审计载体），前端只断开关联：下条消息开新会话
    setSessionId(null);
  }, []);

  const send = useCallback(async (override?: string) => {
    const text = (override ?? input).trim();
    if (!text || busy) return;
    setInput('');
    setBusy(true);
    setError('');

    let sid = sessionId;
    if (!sid) {
      // 懒创建会话：首次发送时落一个（角色默认观察员，可切）
      try {
        const res = await api.createAgentSession('', role);
        const data = (res as unknown as { data?: { id: string } }).data;
        if (!data?.id) throw new Error(t('创建会话失败'));
        sid = data.id;
        setSessionId(sid);
      } catch (e) {
        setBusy(false);
        setError(e instanceof Error ? e.message : String(e));
        return;
      }
    }

    const userMsg: ChatMsg = { id: String(++msgIdRef.current), role: 'user', content: text };
    setMessages((prev) => [...prev, userMsg]);
    // 占位 assistant 气泡，流式往里填
    const asstId = String(++msgIdRef.current);
    setMessages((prev) => [...prev, { id: asstId, role: 'assistant', content: '' }]);

    const controller = new AbortController();
    abortRef.current = controller;
    try {
      await api.agentChatStream(sid, text, (ev: AgentEvent) => {
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
      }, controller.signal, model || undefined);
    } catch (e) {
      if ((e as Error).name !== 'AbortError') {
        setError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      setBusy(false);
      abortRef.current = null;
    }
  }, [input, busy, sessionId, model, role, t]);

  const stop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  // 消息流自动滚底
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, pending]);

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

  const hasMessages = messages.length > 0;

  return (
    <div className="agent-shell">
      {/* open-webui 居中 58rem 消息流（对齐聊天页 chat-debugger-messages） */}
      <div className="agent-messages">
        {messages.length === 0 && (
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
                  onClick={() => { void send(p.text); }}
                >
                  {p.icon}<span>{p.text}</span>
                </button>
              ))}
            </div>
          </div>
        )}
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
              <div className="agent-msg-content">{m.content}</div>
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
        <div ref={bottomRef} />
      </div>

      {error && <div className="agent-error">{error}<button type="button" onClick={() => setError('')}>×</button></div>}

      {/* open-webui 式三段式圆角大输入框（对齐聊天页 chat-debugger-input-*） */}
      <div className="agent-input-row">
        <div className="agent-input-shell">
          <textarea
            className="agent-input"
            value={input}
            rows={2}
            placeholder={t('输入运维指令，Enter 发送，Shift+Enter 换行')}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void send(); } }}
          />
          <div className="agent-input-meta">
            <div className="agent-input-left">
              {/* 角色切换：观察员只读，运维员解锁写工具（高危仍走审批） */}
              <button
                type="button"
                className={`agent-role-pill ${role === 'operator' ? 'operator' : ''}`}
                disabled={busy}
                onClick={() => { setRole((r) => (r === 'observer' ? 'operator' : 'observer')); }}
                title={role === 'observer' ? t('观察员：只读工具。点击切换为运维员') : t('运维员：可执行写工具（高危仍需审批）。点击切回观察员')}
              >
                <Terminal size={12} />
                {role === 'observer' ? t('观察员') : t('运维员')}
              </button>
              <button
                type="button"
                className="agent-icon-btn"
                title={t('清空对话')}
                disabled={busy || !hasMessages}
                onClick={clearAll}
              >
                <Trash2 size={15} />
              </button>
            </div>
            <div className="agent-input-actions">
              <ModelPicker value={model} onChange={setModel} compact />
              {busy ? (
                <button type="button" className="agent-send-fab stop" onClick={stop} title={t('停止')}>
                  <Square size={14} />
                </button>
              ) : (
                <button
                  type="button"
                  className="agent-send-fab"
                  onClick={() => void send()}
                  disabled={!input.trim()}
                  title={t('发送')}
                >
                  <Send size={15} />
                </button>
              )}
            </div>
          </div>
        </div>
      </div>

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
    </div>
  );
}
