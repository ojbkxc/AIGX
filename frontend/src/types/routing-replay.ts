/**
 * 路由回放 + 粘滞会话类型（fe-pages 任务 #14）。
 *
 * 独立成文件的原因：types/index.ts 正被 fe-lists-fix（任务 #13）并行编辑，
 * 共享大文件会让双方的 commit 互相卷入对方的在制品（共享工作区纪律）。
 * 新文件零冲突；后端就绪后如需并入 barrel 再统一迁移。
 */

/** 路由回放候选条目（ordered/blocked 通用；快照当时事实，只读不重算） */
export interface RouteReplayCandidateItem {
  id: string | number;
  name?: string;
  provider?: string;
  priority?: number;
  weight?: number;
  /** blocked 侧：断路器/过滤状态（open/half_open/cooldown/…），缺省 = 普通过滤 */
  state?: string;
  /** 进入/被跳过的原因（后端 describe 口径） */
  reason?: string;
}

/** 路由回放尝试明细（每次真实上游调用一条；对照上游 attemptDetails 口径） */
export interface RouteReplayAttemptItem {
  index?: number;
  channel_id?: string | number;
  channel_name?: string;
  provider?: string;
  account?: string;
  /** 执行阶段（forward/stream/retry/…；缺省按一次调用渲染） */
  phase?: string;
  status?: number;
  duration_ms?: number;
  error?: string;
  /** 非致命备注（如自动重试说明） */
  notice?: string;
}

/** 路由回放快照（GET /api/routing/replay/{request_id}；be-fix #12-B 契约。
 *  字段宽容：缺什么隐藏什么，不因快照字段不全而报错） */
export interface RouteReplaySnapshot {
  request_id?: string | number;
  model?: string;
  status?: number;
  duration_ms?: number;
  first_response_ms?: number;
  error?: string | null;
  /** 最终承载渠道（id 或名称；ordered 内以 id 比对高亮） */
  selected?: string | number;
  route?: {
    strategy?: string;
    sticky?: string;
    sticky_hit?: boolean;
    session_id?: string;
    ordered?: RouteReplayCandidateItem[];
    blocked?: RouteReplayCandidateItem[];
    config_snapshot?: {
      captured_at?: number;
      provider_count?: number;
      account_count?: number;
    };
  };
  attempts?: RouteReplayAttemptItem[];
}

/** 粘滞会话列表（GET /api/routing/sticky；对照上游 routing_workspace get_sticky） */
export interface RouteStickyList {
  entries: Array<{
    /** 会话粘滞标识（后端口径原样回传，逐条清除时原样带上） */
    id: string;
    channel_id?: string | number;
    channel_name?: string;
    provider?: string;
    /** 过期时刻（秒/毫秒自适应用 expiryMillis 口径）；缺省不显示 */
    expires_at?: number;
    updated_at?: number;
  }>;
  now?: number;
  sticky?: string;
}
