import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Bot, Send, Square, ShieldAlert, Terminal, Eye, Wrench, Brain,
  CheckCircle2, XCircle, History, MessageSquarePlus, Trash2, Lock,
} from 'lucide-react';
import ModelPicker from '../components/ModelPicker';
import SessionDrawer from '../components/chat/SessionDrawer';
import EffortPicker, { type Effort } from '../components/chat/EffortPicker';
import StreamPhaseIndicator from '../components/chat/StreamPhase';
import { usePasteGuard } from '../hooks/usePasteGuard';
import MessageViewer from '../components/MessageViewer';
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

/** 会话列表项（后端 AgentSession） */
interface SessionItem {
  id: string;
  title: string;
  model: string;
  role: 'observer' | 'operator';
  created_at: number;
}


/**
 * Agent — AI 运维工作台（对齐 /chat 聊天页 + cc-haha 交互模式）。
 *
 * 单一对话流 + 底部输入区（new-api 游乐园布局）+ 可收起会话抽屉。
 * 会话是后端概念（上下文 + 审计载体），首次发送时懒创建；
 * 历史会话可从抽屉恢复（决策回放 + 上下文续聊）。
 * 输入区底部工具行：[角色徽标 + 历史 + 清空] ··· [模型选择器 + 发送/停止]。
 */
export default function Agent(): JSX.Element {
  const { t } = useTranslation();
  const [messages, setMessages] = useState<ChatMsg[]>([]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [pending, setPending] = useState<PendingApproval | null>(null);
  const [model, setModel] = useState('');
  /** 思考强度：默认关（不发送 reasoning_effort），按任务复杂度点选低/中/高 */
  const [effort, setEffort] = useState<Effort>(() => {
    // 隐私模式/禁用存储时 getItem 可能抛 SecurityError，降级为默认「关」
    let saved: string | null = null;
    try { saved = localStorage.getItem('aigx_agent_effort'); } catch { /* noop */ }
    return saved === 'low' || saved === 'medium' || saved === 'high' ? (saved as Effort) : 'off';
  });
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [role, setRole] = useState<'observer' | 'operator'>('observer');
  /** sessionId 的 ref 镜像：配置加载竞态保护用（恢复历史后禁止被配置初值覆盖模型） */
  const sessionIdRef = useRef<string | null>(null);
  useEffect(() => { sessionIdRef.current = sessionId; }, [sessionId]);
  const abortRef = useRef<AbortController | null>(null);
  const msgIdRef = useRef(0);
  const bottomRef = useRef<HTMLDivElement | null>(null);
  /** 粘贴守卫（共享 hook）：大段内容粘贴瞬间禁用 Enter 发送 */
  const pasteGuard = usePasteGuard();
  /** 流式阶段：思考中 → 正在生成（cc-haha ToolUseLoader 阶段动词模式） */
  const [streamPhase, setStreamPhase] = useState<'thinking' | 'generating'>('thinking');
  /** 最近一次流式事件时间（StreamPhase 停滞检测用） */
  const [lastEventAt, setLastEventAt] = useState(() => Date.now());
  /** 会话抽屉（cc-haha 会话列表 Web 化：桌面侧栏、移动端覆盖层） */
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [sessions, setSessions] = useState<SessionItem[]>([]);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  /** 审批卡遮罩误触提示：点遮罩只提示，不自动拒绝（审批必须显式选择） */
  const [approvalWarn, setApprovalWarn] = useState(false);

  /** 工具中文名（审批卡展示）：从 /api/agent/config 的 tools[] 派生，不再手写标签表 */
  const [toolLabels, setToolLabels] = useState<Map<string, string>>(new Map());
  /** 工具风险等级（审批卡展示用，readonly/low/high） */
  const [toolRisks, setToolRisks] = useState<Map<string, string>>(new Map());
  /** 配置 model 为空、后端自动发现的生效模型（展示「自动：xxx」） */
  const [effectiveHint, setEffectiveHint] = useState('');
  /** 渠道锁定：[agent].channel 非空时只走该渠道，ModelPicker 锁定到渠道模型列表 */
  const [agentChannel, setAgentChannel] = useState('');
  const [agentChannelName, setAgentChannelName] = useState('');
  const [channelModels, setChannelModels] = useState<string[]>([]);

  // 启动时取 Agent 配置：模型选择器初值（显式配置优先，空则用自动发现的生效模型）
  // + 工具标签表（后端 registry 的 label 单一来源）
  useEffect(() => {
    let mounted = true;
    api.getAgentConfig()
      .then((res) => {
        if (!mounted) return;
        const data = (res as unknown as {
          data?: {
            model?: string;
            effectiveModel?: string | null;
            channel?: string;
            tools?: Array<{ name: string; label: string; risk?: string }>;
          };
        }).data;
        // 竞态保护：用户已恢复历史会话时，配置初值不得覆盖会话自己的模型
        if (!sessionIdRef.current) {
          setModel(data?.model || data?.effectiveModel || '');
        }
        // 「自动：xxx」提示仅当配置未显式指定 model、生效模型由自动发现得出时展示
        setEffectiveHint(!data?.model && data?.effectiveModel ? data.effectiveModel : '');
        if (data?.tools?.length) {
          setToolLabels(new Map(data.tools.map((tt) => [tt.name, tt.label])));
          setToolRisks(new Map(data.tools.map((tt) => [tt.name, tt.risk ?? ''])));
        }
        // 渠道锁定：[agent].channel 非空时拉该渠道的模型列表，ModelPicker 锁定展示
        const ch = (data?.channel ?? '').trim();
        if (ch) {
          setAgentChannel(ch);
          api.listChannels()
            .then((res2) => {
              if (!mounted) return;
              const list = (res2 as unknown as {
                data?: Array<{ id: string; name: string; models?: string[]; discovered_models?: string[] }>;
              }).data ?? [];
              const found = list.find((c) => String(c.id) === ch);
              if (found) {
                setAgentChannelName(found.name);
                setChannelModels(Array.from(new Set([
                  ...(found.models ?? []),
                  ...(found.discovered_models ?? []),
                ])));
              }
            })
            .catch(() => { /* 渠道列表失败静默降级：选择器不锁定，发送时后端仍只走锁定渠道 */ });
        }
      })
      .catch(() => { /* 配置读取失败静默降级：选择器空值，发送时走配置模型 */ });
    return () => { mounted = false; };
  }, []);

  /** 拉会话列表（后端按 created_at 倒序） */
  const loadSessions = useCallback(async (): Promise<void> => {
    setSessionsLoading(true);
    try {
      const res = await api.listAgentSessions();
      const list = (res as unknown as { data?: SessionItem[] }).data ?? [];
      setSessions(Array.isArray(list) ? list : []);
    } catch {
      /* 列表失败静默：抽屉里显示空态 */
    } finally {
      setSessionsLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadSessions();
  }, [loadSessions]);

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

  /** 清空当前对话（断开会话关联，下条消息开新会话；服务端会话保留作审计载体） */
  const clearAll = useCallback(() => {
    setMessages([]);
    setSessionId(null);
    // 残留审批卡指向旧会话的 request_id，必须一并清掉（点了会误审批旧请求）
    setPending(null);
  }, []);

  /** 恢复历史会话：拉详情，工具轨迹（tool_calls JSON 数组）还原成步骤条 */
  const restoreSession = useCallback(async (id: string): Promise<void> => {
    if (busy) return;
    // 切换前清掉残留审批卡：它指向旧会话的 request_id，留着会误审批旧请求
    setPending(null);
    try {
      const res = await api.getAgentSession(id);
      const data = (res as unknown as {
        data?: { session?: SessionItem; messages?: Array<{ role: string; content: string; tool_calls?: unknown }> };
      }).data;
      if (!data) throw new Error(t('加载会话失败'));
      setSessionId(data.session?.id ?? id);
      setRole(data.session?.role === 'operator' ? 'operator' : 'observer');
      if (data.session?.model) setModel(data.session.model);
      const restored: ChatMsg[] = [];
      for (const m of data.messages ?? []) {
        if (m.role !== 'user' && m.role !== 'assistant') continue;
        const steps: ToolStep[] = [];
        const mid = String(++msgIdRef.current);
        if (m.role === 'assistant' && Array.isArray(m.tool_calls)) {
          for (const raw of m.tool_calls as Array<Record<string, unknown>>) {
            const type = raw.type as string;
            if (type === 'tool_call') {
              steps.push({ kind: 'call', name: String(raw.name ?? ''), args: String(raw.arguments ?? ''), ok: true });
            } else if (type === 'approval_request') {
              steps.push({ kind: 'approval', name: String(raw.name ?? ''), approved: undefined });
            } else if (type === 'approval_resolved') {
              steps.push({ kind: 'approval', name: String(raw.name ?? ''), approved: raw.approved === true });
            } else if (type === 'tool_result') {
              steps.push({ kind: 'result', name: String(raw.name ?? ''), ok: raw.ok === true, text: typeof raw.text === 'string' ? raw.text : undefined });
            }
          }
        }
        restored.push({ id: mid, role: m.role as 'user' | 'assistant', content: m.content, toolSteps: steps.length ? steps : undefined });
      }
      setMessages(restored);
      setError('');
      setDrawerOpen(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [busy, t]);

  /** 删除历史会话（二次确认由共享 SessionDrawer 内建；当前会话被删时断开前端关联） */
  const deleteSession = useCallback(async (id: string): Promise<void> => {
    try {
      await api.deleteAgentSession(id);
      setSessions((prev) => prev.filter((s) => s.id !== id));
      if (sessionId === id) {
        setSessionId(null);
        setMessages([]);
        // 同 restoreSession：当前会话被删时审批卡也必须清掉
        setPending(null);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [sessionId]);

  /**
   * 把后端自环 LLM failover 的原始失败串翻译成用户可读的「原因 + 建议动作」。
   * 后端失败来自 `agent llm failover`（channel None / all accounts failed /
   * transport error 等），原始串是英文且不指向可操作项——这里按已知征状归一。
   */
  const describeAgentError = useCallback((raw: string): string => {
    const s = raw.toLowerCase();
    if (s.includes('no available channel') || s.includes('no bridge available')) {
      return t('该模型当前没有可用渠道或账号（可能均被禁用或正在冷却中）。请稍后重试，或在「渠道管理」检查该模型的渠道/账号状态，或更换其他模型。');
    }
    if (s.includes('all accounts failed')) {
      return t('该模型的可用账号均不可用（账号可能都在冷却中或需要重新登录）。请稍后重试，或到账号管理检查账号状态，或更换其他模型。');
    }
    if (s.includes('transport error') || s.includes('error sending request')) {
      return t('上游渠道网络不可达（可能正在维护或存在网络异常）。请稍后重试，或更换其他模型。');
    }
    if (s.includes('authentication error') || s.includes('auth failed') || s.includes(' 401')) {
      return t('上游渠道鉴权失败（渠道密钥可能已失效）。请联系管理员检查该渠道的密钥配置。');
    }
    if (s.includes('rate limited') || s.includes(' 429')) {
      return t('上游渠道限流，请稍后重试。');
    }
    if (s.includes('timed out')) {
      return t('上游请求超时，请稍后重试。');
    }
    if (s.includes('model not found') || s.includes('模型不存在')) {
      return t('该模型在上游不存在，请检查模型映射或更换其他模型。');
    }
    return raw;
  }, [t]);

  const send = useCallback(async (override?: string) => {
    const text = (override ?? input).trim();
    if (!text || busy) return;
    setInput('');
    setBusy(true);
    setError('');
    setStreamPhase('thinking');

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
        // 每个流式事件都推进停滞计时（StreamPhase 用）
        setLastEventAt(Date.now());
        if (ev.type === 'approval_request' && typeof ev.request_id === 'string') {
          setPending({ requestId: ev.request_id, name: ev.name ?? '', arguments: ev.arguments ?? '' });
          // 用户切走标签页时用桌面通知提醒审批挂起（需一次性授权）
          if (document.hidden && typeof Notification !== 'undefined' && Notification.permission === 'granted') {
            const n = new Notification(t('AI 运维：等待审批'), {
              body: `${t('工具')}: ${ev.name ?? ''}`,
              tag: 'aigx-approval',
            });
            // 点击通知回到本页处理审批
            n.onclick = () => { window.focus(); n.close(); };
          }
          return;
        }
        if (ev.type === 'final' || ev.type === 'tool_call' || ev.type === 'tool_result') {
          setStreamPhase('generating');
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
              return { ...m, content: m.content + `\n⚠️ ${describeAgentError(ev.message)}` };
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
      }, controller.signal, model || undefined, effort === 'off' ? undefined : effort, role);
      // 会话可能被自动命名，刷新抽屉列表
      void loadSessions();
    } catch (e) {
      if ((e as Error).name !== 'AbortError') {
        setError(describeAgentError(e instanceof Error ? e.message : String(e)));
      }
    } finally {
      setBusy(false);
      abortRef.current = null;
    }
  }, [input, busy, sessionId, model, role, effort, t, loadSessions, describeAgentError]);

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
      {/* 会话抽屉（共享 SessionDrawer：桌面侧栏 / 移动端覆盖层，删除带二次确认） */}
      <SessionDrawer
        variant="sidebar"
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        sessions={sessions.map((s) => ({
          id: s.id,
          title: s.title,
          meta: `${s.role === 'operator' ? t('运维员') : t('观察员')} · ${new Date(s.created_at * 1000).toLocaleDateString()}`,
        }))}
        activeId={sessionId}
        loading={sessionsLoading}
        onSelect={(id) => void restoreSession(id)}
        onCreate={clearAll}
        onDelete={(id) => void deleteSession(id)}
      />

      {/* open-webui 居中 58rem 消息流（对齐聊天页 chat-debugger-messages） */}
      <div className="agent-main">
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
                {m.role === 'assistant' && m.content
                  ? <MessageViewer content={m.content} />
                  : <div className="agent-msg-content">{m.content}</div>}
              </div>
            </div>
          ))}
          {busy && (
            <div className="agent-msg agent-msg-assistant">
              <span className="agent-msg-icon"><Bot size={14} /></span>
              <div className="agent-msg-body">
                {/* 共享流式阶段指示（含停滞检测） */}
                <StreamPhaseIndicator
                  active={busy}
                  phase={streamPhase}
                  lastEventAt={lastEventAt}
                />
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
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) {
                  // 粘贴大内容时紧接着的 Enter 是误触发，忽略
                  if (pasteGuard.shouldSuppressEnter()) {
                    e.preventDefault();
                    return;
                  }
                  e.preventDefault();
                  void send();
                }
              }}
              onPaste={pasteGuard.onPaste}
            />
            <div className="agent-input-meta">
              <div className="agent-input-left">
                {/* 角色切换：观察员只读，运维员解锁写工具（高危仍走审批） */}
                <button
                  type="button"
                  className={`agent-role-pill ${role === 'operator' ? 'operator' : ''}`}
                  disabled={busy}
                  onClick={() => {
                    setRole((r) => (r === 'observer' ? 'operator' : 'observer'));
                    // 切到运维员时顺手请求桌面通知授权：切走标签页时审批挂起才能提醒
                    if (typeof Notification !== 'undefined') {
                      if (Notification.permission === 'default') {
                        void Notification.requestPermission();
                      } else if (Notification.permission === 'denied') {
                        // 通知被拒：引导用户到浏览器站点设置里手动开启
                        setError(t('浏览器通知已被拒绝，请在浏览器站点设置中开启通知后重试'));
                      }
                    }
                  }}
                  title={role === 'observer' ? t('观察员：只读工具。点击切换为运维员') : t('运维员：可执行写工具（高危仍需审批）。点击切回观察员')}
                >
                  <Terminal size={12} />
                  {role === 'observer' ? t('观察员') : t('运维员')}
                </button>
                {/* 思考强度（共享 EffortPicker）：点按循环 关 → 低 → 中 → 高 */}
                <span className="agent-role-pill">
                  <Brain size={12} />
                  <EffortPicker storageKey="aigx_agent_effort" value={effort} onChange={setEffort} disabled={busy} />
                </span>
                <button
                  type="button"
                  className="agent-icon-btn"
                  title={t('会话列表')}
                  onClick={() => setDrawerOpen((v) => !v)}
                >
                  <History size={15} />
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
                {/* 渠道锁定徽标：[agent].channel 非空时提示仅走该渠道 */}
                {agentChannel && (
                  <span className="agent-channel-lock" title={t('Agent 已锁定渠道，只走该渠道的请求')}>
                    <Lock size={11} />
                    {agentChannelName || agentChannel}
                  </span>
                )}
                <ModelPicker
                  value={model}
                  onChange={setModel}
                  compact
                  channelModels={channelModels}
                  lockToChannel={!!(agentChannel && channelModels.length > 0)}
                />
                {/* 自动发现生效模型提示（配置未显式指定 model 时） */}
                {effectiveHint && (
                  <span className="agent-effort-hint" title={t('配置未指定模型，自动发现该生效模型')}>
                    {t('自动')}：{effectiveHint}
                  </span>
                )}
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
      </div>

      {/* 审批弹卡（cc-haha PermissionDialog 语义：标题/工具/参数/三态选择） */}
      {pending && (
        <div className="agent-approval-mask" onClick={() => setApprovalWarn(true)}>
          <div className="agent-approval-card" onClick={(e) => { e.stopPropagation(); setApprovalWarn(false); }}>
            <div className="agent-approval-head">
              <ShieldAlert size={18} className="agent-approval-icon" />
              <div>
                <div className="agent-approval-title">{t('高危操作需要审批')}</div>
                <div className="agent-approval-sub">{t('Agent 请求执行以下写操作，请确认')}</div>
              </div>
            </div>
            <div className="agent-approval-tool">
              <code className="agent-approval-name">{pending.name}</code>
              <span className="agent-approval-desc">{toolLabels.get(pending.name) ?? t('高危写操作')}</span>
              {(() => {
                const r = toolRisks.get(pending.name);
                if (!r) return null;
                const label = r === 'readonly' ? t('只读') : r === 'low' ? t('低危') : t('高危');
                return <span className={`agent-risk-badge ${r}`}>{label}</span>;
              })()}
            </div>
            {pending.arguments && pending.arguments !== '{}' && (
              <pre className="agent-approval-args">{pending.arguments}</pre>
            )}
            {approvalWarn && (
              <div className="agent-approval-warn">{t('请显式选择「允许」或「拒绝」')}</div>
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
