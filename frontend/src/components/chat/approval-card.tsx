/**
 * AI 运维 · 高危操作审批卡（AIGX 独有面：A2A 没有人工审批）。
 *
 * 拆出来的原因：这张卡的状态机原先内联在 `pages/Agent.tsx` 的 `respondApproval`
 * 里，是「先关卡片再发请求」——请求失败时卡片已经消失、错误只飘在页面顶部，
 * 用户既看不到「哪一次审批没发出去」，也没有重试入口；而且提交过程中按钮
 * 全部可点，连点两次会向同一个 request_id 发两次 allow（后端第二次拿到
 * `success:false` 但前端**不读** `success`，界面上看起来两次都成功）。
 *
 * 后端语义（`src/agent/runner.rs` + `src/agent/api.rs`，改动前已核对）：
 * - 高危工具挂起 → SSE `approval_request{request_id,name,arguments}`；
 * - 决策后（或超时）后端必发 `approval_resolved{name,approved}`，
 *   **超时按 Denied 处理**（`tokio::time::timeout` 分支 → `ApprovalResult::Denied`）；
 * - `POST /api/agent/approvals/:id` 返回 `{success: ok, data: null}`，
 *   `ok=false` 表示该 request_id 已不在挂起表里（超时被清、或已被别处应答）。
 *
 * 因此前端必须能表达：待批 → 提交中 → 已允许/已拒绝/已超时/已撤销，以及
 * 提交失败后**保留卡片可重试**。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ShieldAlert } from 'lucide-react';
import { api } from '../../api';

export type ApprovalAction = 'allow' | 'deny' | 'remember';

export type ApprovalPhase =
  | 'idle'        // 待批（按钮可点）
  | 'submitting'  // 已发请求，等服务端确认（按钮全禁）
  | 'allowed'     // 已允许（服务端确认）
  | 'denied'      // 已拒绝（服务端确认）
  | 'expired'     // 超时/服务端已判拒，但**不是**本端点的拒绝 → 不可再点
  | 'revoked'     // 前端主动撤销（切会话/停止/清空对话）
  | 'error';      // 提交失败，卡片保留可重试

export interface ApprovalState {
  phase: ApprovalPhase;
  /** 当前挂起的 request_id；null = 没有待批请求 */
  requestId: string | null;
  /** 工具名（服务端下发） */
  name: string;
  /** 工具参数原文（服务端下发，可能为 '{}'） */
  args: string;
  /** 最后一次提交的动作（重试与「已拒绝/已允许」归因用） */
  lastAction: ApprovalAction | null;
  /** 提交失败原因（phase === 'error' 时展示） */
  error: string | null;
}

export const IDLE_APPROVAL: ApprovalState = {
  phase: 'idle',
  requestId: null,
  name: '',
  args: '',
  lastAction: null,
  error: null,
};

export type ApprovalEvent =
  | { type: 'request'; requestId: string; name: string; args: string }
  | { type: 'submit'; action: ApprovalAction }
  | { type: 'submitFailed'; message: string }
  /** `byAction` = 本端点这次点的动作（服务端确认时带上，避免依赖 lastAction 到达顺序） */
  | { type: 'resolve'; approved: boolean; byAction?: ApprovalAction }
  | { type: 'revoke' }
  | { type: 'reset' };

/**
 * 审批状态机（纯函数）。
 *
 * 关键判据：
 * 1. `submit` 只在 `idle` / `error` 两个阶段被接受 —— `submitting` 期间重复点击
 *    是幂等的（返回原 state），避免连点两次 allow。
 * 2. `submitFailed` **不清 requestId**（原缺陷就是这里清了，卡片直接消失），
 *    只把阶段降到 `error`，卡片留在界面上可重试。
 * 3. `resolve(false)` 且本端没有提交过拒绝 → `expired`（后端超时按 Denied 处理）；
 *    本端提交过 `deny` → `denied`。两者按钮都禁用，但文案不同（超时要提示
 *    「重新发起」而不是「你拒绝了」）。
 * 4. `resolve` 到达时若仍在 `submitting`，说明服务端先确认了 → 直接落终态。
 */
export function reduceApproval(state: ApprovalState, ev: ApprovalEvent): ApprovalState {
  switch (ev.type) {
    case 'request': {
      // 同一 request_id 重复下发（重连/重放）：不重置已提交的状态
      if (state.requestId === ev.requestId && state.phase !== 'idle') return state;
      // 正在提交旧请求时来了新请求：旧卡片先落「已撤销」，新请求接管
      return {
        phase: 'idle',
        requestId: ev.requestId,
        name: ev.name,
        args: ev.args,
        lastAction: null,
        error: null,
      };
    }
    case 'submit': {
      if (!state.requestId) return state;
      if (state.phase !== 'idle' && state.phase !== 'error') return state;
      return { ...state, phase: 'submitting', lastAction: ev.action, error: null };
    }
    case 'submitFailed': {
      if (!state.requestId) return state;
      // 已落终态时忽略迟到的失败回调（例如 resolve 先到）
      if (state.phase === 'allowed' || state.phase === 'denied'
        || state.phase === 'expired' || state.phase === 'revoked') return state;
      return { ...state, phase: 'error', error: ev.message };
    }
    case 'resolve': {
      if (!state.requestId) return state;
      if (state.phase === 'revoked') return state;
      if (ev.approved) {
        return { ...state, phase: 'allowed', error: null };
      }
      // approved === false：区分「我拒的」与「服务端超时/外部撤销」。
      // 判据取两处并集：本端最近一次提交的动作，或服务端确认时携带的动作
      // （挂载方直接按「点按钮 → 发请求 → 拿响应」的链路调 approveResultEvent，
      //  不必先自己 dispatch 一次 submit 才能被认成「我拒的」）。
      const byMe = state.lastAction === 'deny' || ev.byAction === 'deny';
      return { ...state, phase: byMe ? 'denied' : 'expired', error: null };
    }
    case 'revoke': {
      if (!state.requestId) return state;
      return { ...state, phase: 'revoked' };
    }
    case 'reset':
      return IDLE_APPROVAL;
    default:
      return state;
  }
}

/** 卡片是否还占着界面（有 requestId 且未 reset） */
export function approvalVisible(state: ApprovalState): boolean {
  return state.requestId !== null;
}

/**
 * 把 `POST /api/agent/approvals/:id` 的响应翻译成状态机事件。
 *
 * 关键：后端成功/失败都是 **HTTP 200**，差别只在响应体的 `success`：
 * - `success: true`  → 请求确实命中了挂起项，按本端点的动作落终态；
 * - `success: false` → 该 request_id 已不在挂起表（超时被清、已被别处应答）。
 *   旧实现不读 `success`，一律当成成功关卡片——用户以为批准了，工具其实没执行。
 *
 * 这里把它映射成 `resolve{approved:false}`：本端点提交过 deny 时落「已拒绝」，
 * 其余（allow/remember 晚到）落「已过期」，按钮随即禁用。
 */
export function approveResultEvent(action: ApprovalAction, res: unknown): ApprovalEvent {
  const ok = (res as { success?: unknown } | null | undefined)?.success;
  if (ok === false) return { type: 'resolve', approved: false, byAction: action };
  return { type: 'resolve', approved: action !== 'deny', byAction: action };
}

/** 提交抛出异常（网络/401/500）→ 保留卡片可重试 */
export function approveFailureEvent(err: unknown): ApprovalEvent {
  return {
    type: 'submitFailed',
    message: err instanceof Error ? err.message : String(err),
  };
}

/** 三个动作按钮是否可点（提交中/终态一律禁用） */
export function approvalActionsEnabled(state: ApprovalState): boolean {
  return state.requestId !== null && (state.phase === 'idle' || state.phase === 'error');
}

/** 终态文案 key（中文原文即 i18n key） */
export function approvalPhaseLabel(phase: ApprovalPhase): string {
  switch (phase) {
    case 'submitting':
      return '处理中';
    case 'allowed':
      return '已允许';
    case 'denied':
      return '已拒绝';
    case 'expired':
      return '已过期';
    case 'revoked':
      return '已失效';
    case 'error':
      return '操作失败';
    default:
      return '';
  }
}

export interface ApprovalCardProps {
  state: ApprovalState;
  /** 点动作按钮（allow/deny/remember）；组件内部不做请求 */
  onAction: (action: ApprovalAction) => void;
  /** 工具中文名（后端 /api/agent/config 的 tools[].label） */
  toolLabel?: string;
  /** 工具风险等级（readonly/low/high） */
  risk?: string;
  /** 点遮罩的提示态（原来内联在 Agent.tsx 的 approvalWarn） */
  warn?: boolean;
  /** 点遮罩回调（只提示，不自动拒绝） */
  onMaskClick?: () => void;
}

/**
 * 审批卡（视觉沿用 Agent.css 既有 `.agent-approval-*` 类，不新增样式）。
 *
 * 挂载方式见交付说明：把 `pages/Agent.tsx` 里那段内联 JSX 换成
 * `<ApprovalCard state={approval} onAction={respondApproval} ... />`。
 */
export function ApprovalCard({
  state,
  onAction,
  toolLabel,
  risk,
  warn = false,
  onMaskClick,
}: ApprovalCardProps): JSX.Element | null {
  const { t } = useTranslation();
  if (!approvalVisible(state)) return null;

  const enabled = approvalActionsEnabled(state);

  return (
    <div className="agent-approval-mask" onClick={() => onMaskClick?.()}>
      <div className="agent-approval-card" onClick={(e) => { e.stopPropagation(); }}>
        <div className="agent-approval-head">
          <ShieldAlert size={18} className="agent-approval-icon" />
          <div>
            <div className="agent-approval-title">{t('高危操作需要审批')}</div>
            <div className="agent-approval-sub">{t('Agent 请求执行以下写操作，请确认')}</div>
          </div>
        </div>
        <div className="agent-approval-tool">
          <code className="agent-approval-name">{state.name}</code>
          <span className="agent-approval-desc">{toolLabel ?? t('高危写操作')}</span>
          {risk ? (
            <span className={`agent-risk-badge ${risk}`}>
              {risk === 'readonly' ? t('只读') : risk === 'low' ? t('低危') : t('高危')}
            </span>
          ) : null}
        </div>
        {state.args && state.args !== '{}' && (
          <pre className="agent-approval-args">{state.args}</pre>
        )}
        {/* 终态/提交中/失败：把状态显式说出来，别让按钮只是变灰。
            每个阶段走**字面量** t()（不是 t(变量)）：i18n 扫描器只认字面量，
            动态 key 会让英文界面回落中文且脚本扫不出来。 */}
        {state.phase === 'submitting' && (
          <div className="agent-approval-warn" role="status">{t('处理中')}</div>
        )}
        {state.phase === 'allowed' && (
          <div className="agent-approval-warn" role="status">{t('已允许')}</div>
        )}
        {state.phase === 'denied' && (
          <div className="agent-approval-warn" role="status">{t('已拒绝')}</div>
        )}
        {state.phase === 'expired' && (
          <div className="agent-approval-warn" role="status">
            {t('已过期')} · {t('审批已失效，请重新发起请求')}
          </div>
        )}
        {state.phase === 'revoked' && (
          <div className="agent-approval-warn" role="status">{t('已失效')}</div>
        )}
        {state.phase === 'error' && (
          <div className="agent-approval-warn" role="status">
            {t('操作失败')}{state.error ? ` · ${state.error}` : ''}
          </div>
        )}
        {state.phase === 'idle' && warn && (
          <div className="agent-approval-warn">{t('请显式选择「允许」或「拒绝」')}</div>
        )}
        <div className="agent-approval-actions">
          <button
            type="button"
            className="agent-approval-deny"
            disabled={!enabled}
            onClick={() => onAction('deny')}
          >
            {t('拒绝')}
          </button>
          <button
            type="button"
            className="agent-approval-remember"
            disabled={!enabled}
            onClick={() => onAction('remember')}
          >
            {t('本会话总是允许')}
          </button>
          <button
            type="button"
            className="agent-approval-once"
            disabled={!enabled}
            onClick={() => onAction('allow')}
          >
            {t('允许一次')}
          </button>
        </div>
        {/* 失败重试：phase==='error' 时卡片保留，给一个显式重试入口
            （原实现是「先关卡片再发请求」，失败后连重试对象都没了） */}
        {state.phase === 'error' && state.lastAction && (
          <div className="agent-approval-actions">
            <button
              type="button"
              className="agent-approval-once"
              onClick={() => {
                if (state.lastAction) onAction(state.lastAction);
              }}
            >
              {t('重试')}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/**
 * 审批卡与后端之间的薄接线（挂载方用这一个 hook，别自己拼顺序）。
 *
 * 用法（`pages/Agent.tsx`）：
 * ```tsx
 * const approval = useAgentApproval({
 *   onError: (m) => setError(m),        // 兼容原有错误条
 * });
 * // SSE：ev.type === 'approval_request' →
 * //   approval.request(ev.request_id, ev.name ?? '', ev.arguments ?? '')
 * //      并且原本的桌面通知逻辑照旧
 * // SSE：ev.type === 'approval_resolved' → approval.resolve(ev.approved !== false)
 * // 切会话/清空/停止 → approval.revoke()
 * // 渲染：<ApprovalCard state={approval.state} onAction={approval.respond} ... />
 * ```
 *
 * 顺序是这里唯一的事：**先** dispatch `submit`（按钮立刻禁用，连点不再发第二次请求），
 * **再** await 后端，**最后**用 `approveResultEvent` 把响应（含 `success:false`）
 * 翻成终态；抛错则 `submitFailed` 保留卡片可重试。
 */
export function useAgentApproval(opts: {
  /** 审批接口（默认 api.agentApprove；测试可注入） */
  submit?: (requestId: string, action: ApprovalAction) => Promise<unknown>;
  /** 失败回调（原页面用它写顶部错误条） */
  onError?: (message: string) => void;
} = {}): {
  state: ApprovalState;
  request: (requestId: string, name: string, args: string) => void;
  respond: (action: ApprovalAction) => void;
  resolve: (approved: boolean) => void;
  revoke: () => void;
  reset: () => void;
} {
  const [state, setState] = useState<ApprovalState>(IDLE_APPROVAL);
  // 卸载后不再 setState：审批请求可能比组件活得久（切页/切会话）
  const aliveRef = useRef(true);
  useEffect(() => {
    aliveRef.current = true;
    return () => { aliveRef.current = false; };
  }, []);

  const submitRef = useRef(opts.submit);
  submitRef.current = opts.submit;
  const onErrorRef = useRef(opts.onError);
  onErrorRef.current = opts.onError;
  /** 用 ref 镜像 state：respond 里要读最新 requestId/phase，而回调不宜频繁重建 */
  const stateRef = useRef(state);
  stateRef.current = state;
  /**
   * 已在发出的 request_id（同步标记，不依赖 state 重渲染）。
   *
   * 只靠 state.phase 挡重复提交是不够的：同一批事件里连点两下时两次 respond
   * 读到的是同一个旧 state（还没重渲染），两次都会发出去。真机点击虽然通常
   * 会被重渲染隔开，但快速双击/自动化脚本会撞上——用 ref 当场置位才可靠。
   */
  const inFlightRef = useRef<string | null>(null);

  const request = useCallback((requestId: string, name: string, args: string) => {
    // 新请求接管：旧请求视为已撤销，清掉在途标记（旧响应回来会被 isStale 判据忽略）
    inFlightRef.current = null;
    setState((s) => reduceApproval(s, { type: 'request', requestId, name, args }));
  }, []);

  const respond = useCallback((action: ApprovalAction) => {
    const cur = stateRef.current;
    if (!cur.requestId) return;
    if (cur.phase !== 'idle' && cur.phase !== 'error') return; // 终态：不发第二次
    const rid = cur.requestId;
    if (inFlightRef.current === rid) return; // 连点：同一次提交正在进行
    inFlightRef.current = rid;
    setState((s) => reduceApproval(s, { type: 'submit', action }));
    void (async () => {
      try {
        const fn = submitRef.current ?? ((id, act) => api.agentApprove(id, act));
        const res = await fn(rid, action);
        if (!aliveRef.current) return;
        setState((s) => reduceApproval(s, approveResultEvent(action, res)));
      } catch (err) {
        if (!aliveRef.current) return;
        const ev = approveFailureEvent(err);
        setState((s) => reduceApproval(s, ev));
        const msg = err instanceof Error ? err.message : String(err);
        onErrorRef.current?.(msg);
      } finally {
        // 只清自己这条：期间若已被新请求接管，不要清掉新请求的标记
        if (inFlightRef.current === rid) inFlightRef.current = null;
      }
    })();
  }, []);

  const resolve = useCallback((approved: boolean) => {
    setState((s) => reduceApproval(s, { type: 'resolve', approved }));
  }, []);

  const revoke = useCallback(() => {
    setState((s) => reduceApproval(s, { type: 'revoke' }));
  }, []);

  const reset = useCallback(() => {
    setState(() => reduceApproval(IDLE_APPROVAL, { type: 'reset' }));
  }, []);

  return { state, request, respond, resolve, revoke, reset };
}
