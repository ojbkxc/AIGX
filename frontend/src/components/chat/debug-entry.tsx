/**
 * AIGX 独有面 · 调试入口（给 shell 成员 import 用，不需要改 `pages/Chat.tsx`）。
 *
 * A2A 没有「调试」这个概念：它的对话面只有消息流。AIGX 多出来的三样东西都
 * 在这里集中导出，避免 shell 成员分别去摸 63KB 的 ChatDebugger 内部：
 *
 * 1. `ChatDebugger` —— 对话调试器本体（SSE 报文、tool_calls 增量、参数面板、
 *    多模态附件、TTS/转写）。63KB，走默认导出。
 * 2. `DebugTrafficPanel` —— 上游原始报文面（系统设置 → 网关 → 调试模式）。
 * 3. `BoundedPre` —— 大报文的有界渲染（超出预算折叠 + 显式展开）。
 *
 * ── 怎么挂进新结构（A2A 对齐后的 /chat 与 /agent）────────────────────────
 *
 * /chat（`pages/Chat.tsx`，shell 成员持有）：顶栏已有「调试」按钮切调试视图，
 * 挂的就是本模块的 `ChatDebugger`（`Chat.test.tsx` 已钉住这条入口）：
 *
 * ```tsx
 * import ChatDebugger from '../components/chat/debug-entry'; // 或直接 ChatDebugger
 * // 调试视图里：
 * <ChatDebugger playground playgroundModel={model} onPlaygroundModelChange={setModel}
 *               channelModels={models} hasMessages={messages.length > 0}
 *               onClearMessages={clearMessages} onOpenSessions={() => setDrawer(true)} />
 * ```
 *
 * /agent（`pages/Agent.tsx`）：AIGX 独有的「AI 运维」工作台本身不是调试器，
 * 但两样东西可以挂过去：
 *
 * ```tsx
 * // ① 审批卡（AIGX 独有：A2A 没有人工审批）
 * import { ApprovalCard, useAgentApproval } from '../components/chat/approval-card';
 * const approval = useAgentApproval({ onError: setError });
 * //   SSE approval_request → approval.request(ev.request_id, ev.name ?? '', ev.arguments ?? '')
 * //   SSE approval_resolved → approval.resolve(ev.approved !== false)
 * //   切会话/停止/清空   → approval.revoke()
 * //   渲染               → <ApprovalCard state={approval.state} onAction={approval.respond} />
 *
 * // ② 渠道锁定徽标（AIGX 独有：[agent].channel 强制走该渠道）
 * import { ChannelLockBadge, useChannelLock } from '../components/chat/channel-lock';
 * const lock = useChannelLock(agentChannel);
 * <ChannelLockBadge lock={lock} />
 * // ModelPicker 的 lockToChannel 用 lock.lockPicker（不是「有 channel 就锁」）
 *
 * // ③ 流式阶段的时间戳种子（修「新请求一发出就显示停滞」）
 * import { seedStreamPhase } from '../components/chat/debug-stream-state';
 * const seed = seedStreamPhase();
 * setStreamPhase(seed.phase); setLastEventAt(seed.lastEventAt);
 * ```
 *
 * 设置页（`components/AgentSettingsPanel.tsx` 之外的系统设置面）可挂
 * `DebugTrafficPanel`（它自带开关与列表，不依赖任何父级状态）。
 */
export { default as ChatDebugger } from '../ChatDebugger';
export { default as DebugTrafficPanel } from '../DebugTrafficPanel';
export { BoundedPre, boundedText } from './debug-traffic-view';
export { ApprovalCard, useAgentApproval } from './approval-card';
export { ChannelLockBadge, useChannelLock } from './channel-lock';
export { seedStreamPhase } from './debug-stream-state';
