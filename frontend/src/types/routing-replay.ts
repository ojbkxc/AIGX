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

/** 粘滞条目（GET /api/routing/sticky；对照 Agent2API routing_workspace get_sticky）。
 *
 * **形状契约锚点（后端为源，勿按前端假设改字段）**——后端投影出处：
 * `src/api/admin/routing.rs handle_get_route_sticky`（be-fix #38，已入库）。
 *
 * - `id` = 内部编码 `session_id` + U+001F + `model`（**原样回传**用于清除；
 *   展示需按 U+001F 拆分，见 RouteSticky.tsx `splitStickyId`）
 * - 时间/剩余量：`expires_at` / `updated_at` 是 **unix 秒**；
 *   `*_remaining_secs` 是剩余秒数（`sticky_remaining_secs=0` 表示已出粘性窗口、
 *   下次选路重走 HRW，但条目还在硬 TTL 内）
 * - 后端**不返回**渠道名/协议域（只有 channel_id） */
export interface RouteStickyEntry {
  /** 内部编码 `session_id\u{1f}model`（清除时原样回传） */
  id: string;
  channel_id?: string | null;
  /** 硬过期时刻（unix 秒） */
  expires_at?: number;
  /** 建立/最后写入时刻（unix 秒） */
  updated_at?: number;
  /** 距退出粘性窗口的剩余秒数（0 = 已不再影响选路） */
  sticky_remaining_secs?: number;
  /** 距硬过期（被物理清除）的剩余秒数 */
  hard_remaining_secs?: number;
}

/** 粘滞列表响应体（GET /api/routing/sticky 的 data）。
 *
 * 后端按 `ROUTE_STICKY_MAX_SNAPSHOT = 500` 截断（无分页参数），
 * 超出部分由 `total` + `truncated` 回执。 */
export interface RouteStickyList {
  entries: RouteStickyEntry[];
  /** 过滤后总数（截断前） */
  total?: number;
  /** 是否被快照上限截断（true 时 entries 只是一部分） */
  truncated?: boolean;
  /** 服务端当前时刻（unix 秒） */
  now?: number;
  /** 当前粘性档位：auto / session / turn / off */
  sticky?: string;
}

/** 按渠道分组的逐账号跳过（每渠道每请求最多 64 条，超出记 truncated） */
export interface AccountSkipGroup {
  channel_id: string;
  skipped: AccountSkipRow[];
  /** 超出上限未记录的条数（组头显示「还有 n 条未记录」） */
  truncated?: number;
}

/** 逐次上游尝试明细（#32 落库后的时间线数据源）。
 *  形状锚点：src/log/mod.rs:491-515 AttemptDetail（serde camelCase）+
 *  :140-145（Vec + truncated）+ :479-480 ROUTE_REPLAY_MAX_ATTEMPTS=64；
 *  投影 logs.rs:719-720。**有数据才序列化**——开关关闭/旧日志缺省无此字段
 *  （mod.rs:2286 断言不出现），前端形状守卫：缺省 = 整节不渲染。
 *  与上游 attemptDetails 差异（be-fix 契约）：无 accountId/reason/retries[]
 *  （账号轮换在 AccountBridge 内部）；account 恒空串（UI 对空值隐藏）。 */
export interface RouteReplayAttemptItem {
  /** 序号（1 起，与 attempts 计数同口径） */
  index: number;
  channel_id: string;
  channel_name: string;
  /** 渠道协议域（channel_type；账号型恒 "account"） */
  provider: string;
  /** 恒空串（渠道级明细拿不到账号名；UI 隐藏空值） */
  account: string;
  /** 本轮尝试的上游真实模型名（映射后） */
  model: string;
  /** 亲和粘滞命中（本渠道被置顶） */
  stickyHit: boolean;
  /** 开始时刻（unix 秒） */
  startedAt: number;
  /** 结束时刻（unix 秒；null = 未定论） */
  finishedAt: number | null;
  /** HTTP 状态码（null = 传输层失败/未定论） */
  status: number | null;
  /** 失败摘要（成功 null；截断 300 字符） */
  error: string | null;
  /** 提示（契约位，v1 恒 null） */
  notice: string | null;
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
  /** 逐次上游尝试明细（#32；时间线数据源。缺省无此字段 = 不渲染时间线） */
  attempt_details?: RouteReplayAttemptItem[];
  /** 超出 64 条上限未记录的尝试条数（不静默丢；0 时后端省略） */
  attempt_details_truncated?: number;
}
