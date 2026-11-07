/**
 * 路由回放类型（fe-pages #14/#31）。
 *
 * **形状契约锚点（与后端同构，勿改字段名）**——后端投影出处：
 * - handler：`src/api/admin/logs.rs:692-716 handle_get_route_replay`
 * - 字段类型：`src/log/mod.rs`
 *   - `candidate_channels: Vec<String>`（只有渠道 id）
 *   - `filtered_channels: Vec<FilteredChannel{channel_id, reason}>`（mod.rs:391）
 *   - `selected_channel: Option<String>`、`attempts: u32`（**计数非数组**，mod.rs:116）、
 *     `phase: String`（mod.rs:124）
 *   - `route_config: Option<RouteConfigSnapshot{strategy, sticky_enabled}>`（mod.rs:401）
 *   - `route_truncated: usize`
 *   - `account_skips: Vec<AccountSkipGroup{channel_id, skipped[], truncated}>`（mod.rs:438）
 * - 日志只存 attempts 计数、**无逐次尝试明细**（上游 A2A 有 attemptDetails；
 *   AIGX 补数据见 be-fix #32）——「执行时间线」在无明细时整节不渲染。
 *
 * e2e spec 必须按本清单构造 mock（防止「前端假定形状」假绿）。
 */

/** 被过滤渠道（含原因） */
export interface RouteReplayFilteredItem {
  channel_id: string;
  reason: string;
}

/** 路由策略快照（选路那一刻的形态） */
export interface RouteReplayConfigSnapshot {
  strategy?: string;
  sticky_enabled?: boolean;
}

/** 逐账号跳过明细行（#26 有界形状；归属由分组承载） */
export interface AccountSkipRow {
  account_id: string;
  account_name?: string;
  reason?: string;
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

/** 按渠道分组的逐账号跳过（每渠道每请求最多 64 条，超出记 truncated） */
export interface AccountSkipGroup {
  channel_id: string;
  skipped: AccountSkipRow[];
  /** 超出上限未记录的条数（组头显示「还有 n 条未记录」） */
  truncated?: number;
}

/** 路由回放响应（GET /api/routing/replay/{request_id}，与后端投影同构的扁平形状） */
export interface RouteReplaySnapshot {
  log_id?: string | number;
  request_id?: string;
  model?: string;
  origin_model?: string;
  created_at?: number;
  status_code?: number;
  /** 尝试次数（**计数**；非逐次明细） */
  attempts?: number;
  /** 执行阶段（forward/stream/…；日志最终态） */
  phase?: string;
  /** 候选有序表：只有渠道 id（名字缺省，前端有名字渠道映射则用、没有显示 id） */
  candidate_channels?: string[];
  filtered_channels?: RouteReplayFilteredItem[];
  /** 最终承载渠道（id） */
  selected_channel?: string | null;
  route_config?: RouteReplayConfigSnapshot | null;
  /** resolve 层候选截断计数（filtered 上限溢出） */
  route_truncated?: number;
  account_skips?: AccountSkipGroup[];
}
