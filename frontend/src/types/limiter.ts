/**
 * 账号限制器条件组类型（#29）。
 *
 * **形状契约锚点（与后端同构，camelCase 线格式，勿改字段名）**——出处：
 * - `src/account_session/limiter.rs:118-159`（LimiterCondition / LimiterGroup）
 * - 枚举 snake_case：LimiterAction:38 / LimiterTarget:47 / LimiterMetric:56 /
 *   LimiterComparison:78 / LimiterReset:92（serde rename_all = "snake_case"）
 * - LimiterWindow：limiter.rs:99-113（reset + seconds + startsAt/endsAt +
 *   utcOffsetMinutes）
 * - 硬上限：MAX_GROUPS=64 / MAX_CONDITIONS=32 / MAX_CONDITION_MODELS=128
 *   （limiter.rs:155-159）
 * - 写入：PATCH/创建 body 的 `limiter_groups`（account_sessions.rs:152-160、
 *   :456-459）——缺省=保持原值；空数组=清空并回落扁平双读（groups_from_flat）；
 *   校验失败整单 400（文案见 limiter.rs validate）。
 * - 双读：limiter_groups 非空优先；为空回落扁平 limiters（存量零迁移）。
 */

export type LimiterAction = 'skip' | 'disable';
export type LimiterTarget = 'models' | 'account';
export type LimiterMetric =
  | 'remaining'
  | 'remaining_percent'
  | 'used'
  | 'used_percent'
  | 'tokens'
  | 'requests';
export type LimiterComparison = 'lt' | 'lte' | 'gte';
export type LimiterReset = 'fixed' | 'rolling' | 'daily' | 'weekly' | 'monthly' | 'total';
/** 条件连接：与前一条件的连接方式（首条恒 'or'，写入侧归一） */
export type LimiterJoin = 'or' | 'and';

/** 本地计数窗口（仅 tokens/requests 携带；余额类 null） */
export interface LimiterWindow {
  reset: LimiterReset;
  /** fixed/rolling 窗口长度（秒）；自然窗口不携带 */
  seconds?: number;
  /** fixed 窗口显式起止（unix 秒；0=未指定）——v1 编辑器不暴露，保留透传 */
  startsAt?: number;
  endsAt?: number;
  /** 自然窗口时区偏移（分钟；缺省=本地时区）——v1 编辑器不暴露，保留透传 */
  utcOffsetMinutes?: number | null;
}

/** 单条条件：一个来源 × 一组模型 × 一个指标 × 一个阈值（+ 可选窗口） */
export interface LimiterCondition {
  join: LimiterJoin;
  /** 额度来源 id：'account'/'' = 账号主余额；其余 = 钱包行 id */
  sourceId: string;
  /** 本条件涉及的模型（空 = 不限模型维度；≤128） */
  models: string[];
  metric: LimiterMetric;
  comparison: LimiterComparison;
  /** 阈值（非负有限；百分比 0~100；本地类须为正整数） */
  threshold: number;
  window: LimiterWindow | null;
}

/** 条件组：动作 + 目标 + 条件列表（1~32 条） */
export interface LimiterGroup {
  enabled: boolean;
  action: LimiterAction;
  target: LimiterTarget;
  conditions: LimiterCondition[];
}
