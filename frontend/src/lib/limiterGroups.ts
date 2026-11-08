/**
 * 限制器条件组纯逻辑（#29）——扁平规则预填 / 校验 / 摘要，无 React 依赖。
 *
 * 校验规则镜像后端 `src/account_session/limiter.rs` validate（:163-190）：
 * 每组 1~32 条、阈值非负有限、本地类（tokens/requests）正整数、百分比 0~100、
 * 模型 ≤128/条件；组数 ≤64。错误文案与后端一致（400 透传前前端先拦）。
 * 求值语义（limiter.rs 模块注释）：与优先于或——相邻 AND 链成组，组内全命中
 * 才算该分支命中；任一 OR 分支命中即命中。
 */
import type { AccountLimiterRule } from '../types';
import type {
  LimiterCondition,
  LimiterGroup,
  LimiterMetric,
  LimiterWindow,
} from '../types/limiter';

export const MAX_GROUPS = 64;
export const MAX_CONDITIONS = 32;
export const MAX_CONDITION_MODELS = 128;

/** 本地消耗类指标（读计数器，window 必填；threshold 须正整数） */
export function isLocalMetric(m: LimiterMetric): boolean {
  return m === 'tokens' || m === 'requests';
}

/** 百分比类指标（阈值 0~100） */
export function isPercentMetric(m: LimiterMetric): boolean {
  return m === 'remaining_percent' || m === 'used_percent';
}

/** 默认条件（新行起点：主余额 / 不限模型 / 余额下限 ≤0） */
export function defaultCondition(): LimiterCondition {
  return {
    join: 'or',
    sourceId: '',
    models: [],
    metric: 'remaining',
    comparison: 'lte',
    threshold: 0,
    window: null,
  };
}

/** 默认组（新组起点：skip / models 目标 / 单条件） */
export function defaultGroup(): LimiterGroup {
  return { enabled: true, action: 'skip', target: 'models', conditions: [defaultCondition()] };
}

/**
 * 组级校验：返回第一条错误的中文文案（与后端 400 文案一致），null = 通过。
 * 逐条件校验用 validateCondition 取行级文案；本函数复核组级约束。
 */
export function validateCondition(c: LimiterCondition): string | null {
  if (!Number.isFinite(c.threshold) || c.threshold < 0) {
    return '限制阈值必须是非负有限数字';
  }
  if (isLocalMetric(c.metric) && (c.threshold <= 0 || !Number.isInteger(c.threshold))) {
    return '本地消耗上限必须是大于 0 的安全整数';
  }
  if (isPercentMetric(c.metric) && c.threshold > 100) {
    return '百分比阈值必须在 0~100 之间';
  }
  if (c.models.length > MAX_CONDITION_MODELS) {
    return `每条条件最多 ${MAX_CONDITION_MODELS} 个模型`;
  }
  if (isLocalMetric(c.metric)) {
    if (!c.window) return '本地消耗类条件必须配置窗口';
    if ((c.window.reset === 'fixed' || c.window.reset === 'rolling')
      && (!Number.isFinite(c.window.seconds) || (c.window.seconds ?? 0) <= 0)) {
      return '固定/滚动窗口必须配置大于 0 的秒数';
    }
  }
  return null;
}

/** 整包校验：组数 + 每组条件数 + 逐条件；返回首条错误文案或 null */
export function validateGroups(groups: LimiterGroup[]): string | null {
  if (groups.length > MAX_GROUPS) return `条件组最多 ${MAX_GROUPS} 个`;
  for (const g of groups) {
    if (g.conditions.length === 0 || g.conditions.length > MAX_CONDITIONS) {
      return '每个条件组必须有 1~32 条条件';
    }
    for (const c of g.conditions) {
      const err = validateCondition(c);
      if (err) return err;
    }
  }
  return null;
}

/**
 * 扁平规则 → 条件组预填（一键升级用；每条扁平规则 = 单条件单组）。
 * 语义映射（扁平规则 types/index.ts:1301-1314）：
 * - balance（余额下限，触发当 remaining ≤ threshold）→ metric=remaining,
 *   comparison=lte, window=null
 * - token（周期用量上限，触发当 used ≥ threshold）→ metric=tokens,
 *   comparison=gte, window：reset=daily → {reset:daily}；reset=fixed →
 *   {reset:fixed, seconds:period}；其余 → {reset:total}
 */
export function flatToGroups(rules: AccountLimiterRule[]): LimiterGroup[] {
  return rules.map((rule) => {
    const condition: LimiterCondition =
      rule.type === 'token'
        ? {
            join: 'or',
            sourceId: '',
            models: [],
            metric: 'tokens',
            comparison: 'gte',
            threshold: rule.threshold,
            window: flatWindow(rule),
          }
        : {
            join: 'or',
            sourceId: '',
            models: [],
            metric: 'remaining',
            comparison: 'lte',
            threshold: rule.threshold,
            window: null,
          };
    return {
      enabled: rule.enabled !== false,
      action: rule.action,
      target: 'account' as const,
      conditions: [condition],
    };
  });
}

function flatWindow(rule: AccountLimiterRule): LimiterWindow {
  if (rule.reset === 'daily') return { reset: 'daily', seconds: 0 };
  if (rule.reset === 'fixed') return { reset: 'fixed', seconds: rule.period ?? 86_400 };
  return { reset: 'total', seconds: 0 };
}

/** 组摘要（列表一行）：动作 + 目标 + 条件数（如「skip · 整号 · 2 条件」） */
export function describeGroup(g: LimiterGroup): string {
  const action = g.action === 'disable' ? '停用账号' : '跳过本轮';
  const target = g.target === 'account' ? '整号' : '限定模型';
  const local = g.conditions.filter((c) => isLocalMetric(c.metric)).length;
  const parts = [`${action} · ${target} · ${g.conditions.length} 条件`];
  if (local > 0) parts.push(`${local} 条本地计数`);
  return parts.join(' · ');
}
