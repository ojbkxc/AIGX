import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { Activity, Bot, Send, Square, ShieldAlert, Terminal, Eye, Brain,
  History, MessageSquarePlus, Trash2, Lock, Settings,
} from 'lucide-react';
import ModelPicker from '../components/ModelPicker';
import SessionDrawer from '../components/chat/SessionDrawer';
import EffortPicker, { type Effort } from '../components/chat/EffortPicker';
import StreamPhaseIndicator from '../components/chat/StreamPhase';
import ToolCallCard from '../components/chat/ToolCallCard';
import AgentSettingsPanel from '../components/AgentSettingsPanel';
import { MobileDrawer } from '../components/ui';
import { usePasteGuard } from '../hooks/usePasteGuard';
import MessageViewer from '../components/MessageViewer';
import { api } from '../api';
import {
  appendStep,
  finalizeItems,
  itemsFromSteps,
  phaseOverrides,
  stepFromEvent,
  stepsFromTrace,
  toToolCallStates,
  type ToolAgentItem,
} from '../components/chat/tool-steps';
import { consumePurgeAiHandoff, touchedPurge, type PurgeAiStorage } from '../lib/purge-ai-bridge';
import AgentOpsPanel from '../components/agent-ops-panel';
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

/** 工具轨迹（决策回放用）。
 *
 *  AIGX 的 SSE 只给 `name`（没有 call id），所以「调用 ↔ 结果」的配对在
 *  `tool-steps.ts` 里按同名先进先出做（见那边的文件头）。这里只存配对后的
 *  **一次调用一项**，渲染交给共享的 `ToolCallCard` —— 与 /chat 同一张卡、
 *  同一套三态，两个页面不会各说一套「成功/失败」。 */
interface ChatMsg {
  id: string;
  role: 'user' | 'assistant';
  content: string;
  toolItems?: ToolAgentItem[];
}

/** 会话列表项（后端 AgentSession） */
interface SessionItem {
  id: string;
  title: string;
  model: string;
  role: 'observer' | 'operator';
  created_at: number;
}


/** 破甲 → AI 运维交接用的 sessionStorage 面（模块级常量：引用恒定，可安全进 effect 依赖） */
const handoffStore: PurgeAiStorage = {
  get: (k) => window.sessionStorage.getItem(k),
  set: (k, v) => window.sessionStorage.setItem(k, v),
  remove: (k) => window.sessionStorage.removeItem(k),
};

/** 右栏断点（与 Agent.css 的 `.agent-ops` 媒体查询同一数值；改一处必须同改另一处） */
const OPS_NARROW_MQ = '(max-width: 1100px)';

/**
 * 窄屏判定：窄屏下右栏「运行信息」不再常驻，改为按钮打开的覆盖层抽屉。
 * 与 SessionDrawer 的 `useIsNarrow` 同形态（监听断点变化，窗口拖动/旋转即时切换）。
 */
function useIsNarrowOps(): boolean {
  const [narrow, setNarrow] = useState<boolean>(
    () => typeof window !== 'undefined' && Boolean(window.matchMedia?.(OPS_NARROW_MQ).matches),
  );
  useEffect(() => {
    // `matchMedia` 不是所有环境都有（jsdom 就没有）：缺了按「宽屏」处理——
    // 宽屏分支只会多渲染一个面板，不会抛错把整页打成错误态。
    const mq = window.matchMedia?.(OPS_NARROW_MQ);
    if (!mq) return undefined;
    const onChange = (): void => setNarrow(mq.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  return narrow;
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
  const [role, setRole] = useState<'observer' | 'operator'>('operator');
  /** sessionId 的 ref 镜像：配置加载竞态保护用（恢复历史后禁止被配置初值覆盖模型） */
  const sessionIdRef = useRef<string | null>(null);
  useEffect(() => { sessionIdRef.current = sessionId; }, [sessionId]);
  const abortRef = useRef<AbortController | null>(null);
  /** 本轮是否碰过破甲工具（流式期间写入 ref，收尾时转成 state 触发「去破甲控制台」入口） */
  const purgeTouchedRef = useRef(false);
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
  /** 「助手设置」抽屉（迁自「系统设置 → 运维 → AI 助手」的 AgentSettingsPanel） */
  const [settingsOpen, setSettingsOpen] = useState(false);
  /** 右栏「运行信息」：桌面常驻，窄屏由输入区按钮打开覆盖层抽屉 */
  const [opsOpen, setOpsOpen] = useState(false);
  const opsNarrow = useIsNarrowOps();
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
  /** 破甲控制台带过来的交接正文（「问 AI」按钮；一次性，读完即清） */
  const [handoffPrompt, setHandoffPrompt] = useState('');
  /** 本轮对话是否碰过破甲工具（决定要不要显示「去破甲控制台看结果」） */
  const [purgeTouched, setPurgeTouched] = useState(false);

  // 破甲 → AI 运维的一次性交接：挂载时取走（`consume` 读完即删，离开再回来不会重复填）。
  // 放在 effect 里而不是 useState 惰性初值：惰性初值也会在 SSR/重挂时跑，而这里需要
  // 「只取一次、并且真的把存储清掉」——惰性初值做不到后半句。
  useEffect(() => {
    const carried = consumePurgeAiHandoff(handoffStore);
    if (carried) {
      setInput(carried);
      setHandoffPrompt(carried);
    }
  }, []);

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
        const mid = String(++msgIdRef.current);
        // 历史会话落库的是**轨迹**（tool_call / approval_* / tool_result 三种条目
        // 各自一条），不是配对后的卡片。这里一次配对成「一次调用一张卡」，
        // 与流式期间 `appendStep` 走同一段判据（同名 FIFO），不会出现
        // 「历史里是成功、流式时是失败」这种两套口径。
        const items = m.role === 'assistant' && Array.isArray(m.tool_calls)
          ? itemsFromSteps(stepsFromTrace(m.tool_calls))
          : [];
        restored.push({
          id: mid,
          role: m.role as 'user' | 'assistant',
          content: m.content,
          toolItems: items.length ? items : undefined,
        });
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
    // 跨模型 failover 已穷尽：后端已按「模型: 原因」逐条给出（`AllModelsFailed`），
    // 这里**必须原样分行展示**。落到下面任何一个单因分支都会把 A/B 各自的原因
    // 压成一句「鉴权失败」——运维就看不到试了哪几个模型、各自为什么不行，
    // 这与「把每个模型的失败原因如实报出」相反。
    if (s.includes('all candidate models failed')) {
      const detail = raw.slice(raw.toLowerCase().indexOf('all candidate models failed') +
        'all candidate models failed'.length);
      const lines = detail
        .replace(/^[:：]\s*/, '')
        .split(' | ')
        .map((line) => line.trim())
        .filter(Boolean);
      if (lines.length > 0) {
        return `${t('已依次尝试以下模型，均失败：')}\n${lines.map((line) => `· ${line}`).join('\n')}`;
      }
    }
    // 暂时性失败已自动重试过若干轮才报到前端：优先给出「稍后再试」，
    // 别让用户以为配置坏了（后端 TransientExhausted 变体）。
    if (s.includes('failed after') && s.includes('rounds')) {
      return t('上游服务暂时不可用，已自动重试多轮仍未恢复。请稍等片刻后再试，或更换其他模型。');
    }
    if (s.includes('no available channel') || s.includes('no bridge available')) {
      return t('该模型当前没有可用供应商或账号（可能均被禁用或正在冷却中）。请稍后重试，或在「供应商」检查该模型的供应商/账号状态，或更换其他模型。');
    }
    if (s.includes('all accounts failed')) {
      return t('该模型的可用账号均不可用（账号可能都在冷却中或需要重新登录）。请稍后重试，或到账号管理检查账号状态，或更换其他模型。');
    }
    if (s.includes('transport error') || s.includes('error sending request')) {
      return t('上游供应商网络不可达（可能正在维护或存在网络异常）。请稍后重试，或更换其他模型。');
    }
    if (s.includes('authentication error') || s.includes('auth failed') || s.includes(' 401')) {
      return t('上游供应商鉴权失败（供应商密钥可能已失效）。请联系管理员检查该供应商的密钥配置。');
    }
    if (s.includes('rate limited') || s.includes(' 429')) {
      return t('上游供应商限流，请稍后重试。');
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
    // 新一轮开始：上一轮「碰过破甲」的结论不能带到这一轮（入口是否显示要跟着本轮走）
    purgeTouchedRef.current = false;
    setPurgeTouched(false);

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
    /** 用户中止（不是错误：卡片按「已取消」收尾，不报错也不洗成完成） */
    let aborted = false;
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
            if (ev.type === 'final' && typeof ev.content === 'string') {
              return { ...m, content: m.content + ev.content };
            }
            if (ev.type === 'error' && typeof ev.message === 'string') {
              return { ...m, content: m.content + `\n⚠️ ${describeAgentError(ev.message)}` };
            }
            // 跨模型切换成功：后端把「前面那些模型为何失败」一并送来。
            // 不是错误（本轮有答复），但属于「每个模型的失败原因如实报出」
            // 在成功路径上的那一半，不能丢。
            if (ev.type === 'notice' && typeof ev.message === 'string') {
              return {
                ...m,
                content: `${m.content}\n🔀 ${t('已自动切换模型，前面模型失败原因：')} ${ev.message}`,
              };
            }
            // 工具事件统一进「配对层」：调用/审批/结果三种事件都会更新**同一张卡**
            // （此前是各 push 一行，读起来像流水账，且「这次成没成」要人眼在相邻
            // 两行之间拼）。
            const step = stepFromEvent(ev);
            if (step) {
              // 「碰过破甲吗」只记 ref：流式期间每条事件都 setMessages，再多一次 setState
              // 就是每事件一轮重渲染；收尾时统一转成 state。
              if (step.kind === 'call' && touchedPurge([{ name: step.name, args: step.args }])) {
                purgeTouchedRef.current = true;
              }
              return { ...m, toolItems: appendStep(m.toolItems ?? [], step) };
            }
            return m;
          }),
        );
      }, controller.signal, model || undefined, effort === 'off' ? undefined : effort, role);
      // 会话可能被自动命名，刷新抽屉列表
      void loadSessions();
    } catch (e) {
      aborted = (e as Error).name === 'AbortError';
      if (!aborted) {
        setError(describeAgentError(e instanceof Error ? e.message : String(e)));
      }
    } finally {
      // 收尾：还在跑/等审批的调用按「这一轮结束了」定性；用户中止则标已取消。
      // 不做「看起来成功就算成功」的猜测 —— 没有结果就是没有结果（卡片会标
      // 「结果未回灌」），这是 A2A 的口径，也是「失败被画成成功」的解药。
      setMessages((prev) => prev.map((m) => (
        m.id === asstId && m.toolItems?.length
          ? { ...m, toolItems: finalizeItems(m.toolItems, { cancelled: aborted }) }
          : m
      )));
      setBusy(false);
      abortRef.current = null;
      // 「本轮碰过破甲吗」：流式期间在事件回调里只置 ref（零重渲染），收尾时读一次
      // 再落 state。判据在 `touchedPurge`（工具名 + `aigx_config_set` 的 key 参数），
      // 不靠模型自述（它可能说「已调整破甲」而其实只读了一次）。
      if (purgeTouchedRef.current) setPurgeTouched(true);
    }
  }, [input, busy, sessionId, model, role, effort, t, loadSessions, describeAgentError]);

  const stop = useCallback(() => {
    abortRef.current?.abort();
  }, []);

  // 消息流自动滚底
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, pending]);

  /** 工具轨迹卡片（决策回放：调用 → 审批 → 结果，一次调用一张卡）。
   *
   *  与 /chat 共用 `ToolCallCard`：逐条三态、失败着色、非法 JSON 标签、凭证遮蔽、
   *  长输出折叠全都在那一张卡里。`labels`/`risks` 来自 `/api/agent/config` 的
   *  `tools[]`（后端 registry 是中文名与风险位的唯一来源），`phases` 来自配对层
   *  —— `ok:false` 的结果文本不一定长得像错误，只有后端那个布尔位说得准。 */
  const renderToolCard = (items: ToolAgentItem[], streaming: boolean): JSX.Element => (
    <ToolCallCard
      calls={toToolCallStates(items)}
      state={streaming ? 'streaming' : 'done'}
      phases={phaseOverrides(items)}
      labels={toolLabels}
      risks={toolRisks}
    />
  );

  const hasMessages = messages.length > 0;

  return (
    <div className="agent-page">
      {/* 页面只有一个主体：AI 运维对话工作台。
          此前这里还挂着一整块 A2A 客户端检测面板（检测本机装了哪些 AI 编码客户端、
          帮它们接入本网关）——那是「给外部客户端配网关」，与「用 AI 运维网关」无关，
          已整体下架；腾出的右栏改挂 `AgentOpsPanel`（只读运行信息）。
          布局：左会话抽屉 · 中对话流 · 右运行信息（窄屏右栏降级为抽屉）。 */}
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

      {/* 助手设置抽屉（迁自「系统设置 → 运维 → AI 助手」）：面板自取数自保存 */}
      <MobileDrawer
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        width="min(92vw, 460px)"
        ariaLabel={t('助手设置')}
      >
        <div style={{ padding: 16 }}>
          <AgentSettingsPanel />
        </div>
      </MobileDrawer>

      {/* open-webui 居中 58rem 消息流（对齐聊天页 chat-debugger-messages） */}
      <div className="agent-main">
        <div className="agent-messages">
          {messages.length === 0 && (
            <div className="agent-empty">
              <div className="agent-empty-title">{t('AI 运维工作台')}</div>
              <p className="agent-empty-sub">{t('用 AI 运维你的网关——查供应商、看用户、测健康、排故障')}</p>
              <div className="agent-empty-prompts">
                {[
                  { icon: <Terminal size={13} />, text: t('看下系统状态，哪些供应商挂了？') },
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
                {m.toolItems && m.toolItems.length > 0 && renderToolCard(m.toolItems, busy && m.id === messages[messages.length - 1]?.id)}
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

        {/* 破甲 → AI 运维的交接横幅：告诉用户「输入框里那段是破甲控制台的配置摘要」，
            并给一条「不想问就清掉」的出口（否则用户会以为是不小心粘进来的东西）。
            只在本次交接仍是输入框内容的主体时显示：用户改过或清空后自动收起。 */}
        {handoffPrompt && input === handoffPrompt ? (
          <div
            className="agent-handoff-note"
            role="status"
            data-testid="agent-purge-handoff"
            style={{
              display: 'flex', alignItems: 'center', gap: 8,
              maxWidth: '58rem', width: 'calc(100% - 32px)', alignSelf: 'center',
              margin: '0 16px 8px', padding: '8px 12px',
              border: '1px solid var(--border-color)', borderRadius: 12, fontSize: 13,
              background: 'rgba(127, 127, 127, 0.06)',
            }}
          >
            <span>
              {t('输入框里是「破甲控制台」的当前配置摘要（未保存的改动已标注）。发送后 AI 会先体检再给方案，写操作仍需你在审批卡上确认。')}
            </span>
            <button
              type="button"
              onClick={() => { setInput(''); setHandoffPrompt(''); }}
              style={{
                marginLeft: 'auto', border: 'none', background: 'transparent',
                color: 'inherit', cursor: 'pointer', fontSize: 13, textDecoration: 'underline',
              }}
            >
              {t('清掉')}
            </button>
          </div>
        ) : null}

        {/* AI → 破甲的回跳入口：本轮 AI 真的动过破甲（工具轨迹判据，不看模型自述），
            才显示「去破甲控制台看结果」——没动过就显示等于噪音。 */}
        {purgeTouched ? (
          <div
            className="agent-purge-link"
            role="status"
            data-testid="agent-purge-link"
            style={{
              display: 'flex', alignItems: 'center', gap: 8,
              maxWidth: '58rem', width: 'calc(100% - 32px)', alignSelf: 'center',
              margin: '0 16px 8px', padding: '8px 12px',
              border: '1px solid var(--border-color)', borderRadius: 12, fontSize: 13,
              background: 'rgba(127, 127, 127, 0.06)',
            }}
          >
            <span>{t('本轮 AI 动过破甲配置（改完记得去页面核对生效项与体检）')}</span>
            <Link to="/purge" style={{ marginLeft: 'auto', textDecoration: 'underline', color: 'inherit' }}>
              {t('去破甲控制台看结果')}
            </Link>
          </div>
        ) : null}

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
                    // 切到运维员时顺手请求桌面通知授权：切走标签页时审批挂起才能提醒。
                    // 通知是增强能力：被拒不影响角色切换，静默降级（绝不 setError 污染页面错误态）。
                    if (typeof Notification !== 'undefined' && Notification.permission === 'default') {
                      void Notification.requestPermission();
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
                  title={t('助手设置')}
                  onClick={() => setSettingsOpen(true)}
                >
                  <Settings size={15} />
                </button>
                {/* 窄屏才有：右栏「运行信息」降级为覆盖层，用这个按钮打开 */}
                {opsNarrow && (
                  <button
                    type="button"
                    className="agent-icon-btn"
                    title={t('运行信息')}
                    aria-label={t('运行信息')}
                    onClick={() => setOpsOpen(true)}
                  >
                    <Activity size={15} />
                  </button>
                )}
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
                  <span className="agent-channel-lock" title={t('Agent 已锁定供应商，只走该供应商的请求')}>
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

      {/* 右栏「运行信息」（只读运维观测面）：桌面常驻；窄屏改由输入区按钮打开覆盖层。
          内容全部来自已有管理员接口（/api/purge runtime、/api/channels、/api/logs/requests），
          不新增后端面；取数失败只影响本栏，不牵连对话主流程。 */}
      {opsNarrow ? (
        <MobileDrawer
          open={opsOpen}
          onClose={() => setOpsOpen(false)}
          width="min(88vw, 380px)"
          ariaLabel={t('运行信息')}
        >
          <AgentOpsPanel onClose={() => setOpsOpen(false)} />
        </MobileDrawer>
      ) : (
        <AgentOpsPanel />
      )}

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
    </div>
  );
}
