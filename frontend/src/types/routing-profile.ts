/**
 * 路由多规则体系（任务 #40）数据契约 —— 与后端 `src/channel/routing_policy.rs`
 * 逐字段对齐（契约冻结稿 §1 / §5）。
 *
 * `GET /api/routing` 在既有字段之外**只新增** `enabled` 与 `profiles`，
 * 旧字段语义不变（旧前端不受影响）。
 *
 * ── 三态是本设计的核心，别在界面上退化成两态 ──────────────────────
 * 后端 `RoutingProfile` 的每个字段都是 `#[serde(default)] Option<_>`：
 *
 *   `null`（或键缺失）  = **继承下层**（本层不表态）
 *   有值                = **显式设为该值**（本层说了算）
 *   有值且「空」        = **显式清空**（本层说了算：就是空）
 *
 * 缺了「显式清空」，模型级就无法「不用 global 的参与名单」；缺了「继承」，
 * 改 global 时模型级不会跟着变（静默的规则漂移，配置界面看不出）。
 * 前端统一发 `null` 表示继承 —— 与缺键等价，但意图显式。
 *
 * ⚠ `strategy` / `sticky` 是枚举，**没有「空」语义**，只有继承 / 覆盖两态。
 */
import type { RouteStrategy, StickyMode, RoutingPolicyItem, RoutingQueueEntryItem } from './index';

/** 提供商作用域：all = 自动并入新出现的提供商；listed = 只用 providers 列出的 */
export type ProviderScope = 'all' | 'listed';

/** 排序方向 */
export type SortDir = 'asc' | 'desc';

/** 排序依据（AIGX 口径，对应账号的 last_balance / created_at / expires_at / limiter reset） */
export type SortOn = 'balance' | 'added' | 'expires' | 'quota_reset';

/** 账号排序规则：serde 内部标签形式 `{"on":"balance","dir":"asc"}` */
export interface AccountSortRule {
  on: SortOn;
  dir: SortDir;
}

/** 参与名单（provider / 账号标识）。各名单上限见 `ROUTING_CAPS` */
export interface RoutingScope {
  provider_scope: ProviderScope;
  providers: string[];
  disabled_providers: string[];
  /** 账号 id 优先级顺序（先出现的先试） */
  account_order: string[];
  excluded_accounts: string[];
}

/** 能力需求（由请求体推出；账号侧按 tool / vision / video 标签做包含判定） */
export interface RequestNeeds {
  tools: boolean;
  images: boolean;
  video: boolean;
}

/** 单层档案。每个字段 `null` = 继承下层。 */
export interface RoutingProfile {
  scope: RoutingScope | null;
  strategy: RouteStrategy | null;
  sticky: StickyMode | null;
  needs: RequestNeeds | null;
  /** 上限 2 条；`[]` = 显式清空 */
  sort_rules: AccountSortRule[] | null;
  /** 该层指定的默认路由组 id；`''` = 显式清空 */
  default_group: string | null;
}

/** 路由组成员 */
export interface RoutingMember {
  provider: string;
  model: string;
  included: boolean;
  accounts: string[];
}

/**
 * 路由组。**非三态**（后端字段不是 Option）：组一旦被选中，
 * 其 `strategy` / `sticky` 总会覆盖上层——组内没配时即为枚举默认值
 * （weighted / auto）。界面上必须把这条讲明白，否则用户会以为
 * 「组没配策略 = 继承 global」。
 */
export interface RoutingGroup {
  id: string;
  name: string;
  /** 该组服务的模型（A2A 的组是 per-model 的） */
  model: string;
  strategy: RouteStrategy;
  sticky: StickyMode;
  members: RoutingMember[];
}

/**
 * 三层容器。`models` 的键是**请求名/别名**，不是上游真名
 * （契约 §2.3：解析发生在候选渠道选择之前，此刻不存在唯一上游名）。
 * 后端查找口径 = 请求名精确命中优先 → 全局映射名兜底。
 */
export interface RoutingProfiles {
  global: RoutingProfile;
  models: Record<string, RoutingProfile>;
  groups: RoutingGroup[];
}

/** 字段值来自哪一层（后端 `Layer` 枚举的 snake_case 形态） */
export type Layer = 'global' | 'model' | 'group';

/**
 * 逐字段来源。后端 `resolve_profile` 的产物（`ProfileProvenance`）。
 * 缺 provenance 时界面只能显示结果值，「我改了 global 为什么没生效」
 * 这类问题就无法自查 —— 故它是验收前提，不是锦上添花。
 */
export interface ProfileProvenance {
  scope: Layer | null;
  strategy: Layer | null;
  sticky: Layer | null;
  needs: Layer | null;
  sort_rules: Layer | null;
  default_group: Layer | null;
}

/** 档位上限（后端 `routing_policy::caps`；超限后端**拒绝写入**，不静默截断） */
export const ROUTING_CAPS = {
  MAX_MODELS: 500,
  MAX_PROVIDERS: 256,
  MAX_ACCOUNTS: 10_000,
  MAX_GROUPS: 500,
  MAX_MEMBERS: 256,
  MAX_SORT_RULES: 2,
} as const;

// ── #40 新增的 wire 字段（不写进 types/index.ts：那个文件被 #13 并行编辑，
//    契约 §5 明说旧字段语义不变、只新增，故在独立文件里扩展）────────────

/** `GET /api/routing` 的 #40 形态：旧字段 + `enabled` / `profiles` */
export type RoutingPolicyWithProfiles = RoutingPolicyItem & {
  /** 多规则总开关（默认 false = 档案全部短路，行为与旧版逐字节一致） */
  enabled?: boolean;
  profiles?: RoutingProfiles;
};

/** `GET /api/routing/queue` 的 #40 形态：新增命中层 / 裁剪计数 */
export type RoutingQueueWithLayers = {
  /** 档案总开关（旧前端不读即忽略） */
  profilesEnabled?: boolean;
  /** 本次判定**实际命中**的层（已去重） */
  appliedLayers?: Layer[];
  /** 被档案裁掉的原因计数 */
  skipped?: SkipSummary;
};

/** 被裁掉的原因计数（后端 `SkipSummary`；原因码沿用既有日志枚举） */
export interface SkipSummary {
  /** 渠道提供商不在档案 `providers` 名单内 */
  provider_not_allowed?: number;
  /** 渠道提供商落在档案 `disabled_providers` 内 */
  provider_disabled?: number;
}

/**
 * `POST /api/routing/preview` 的可见投影。
 * 与真实转发调**同一个** `plan_route`，故界面上不再自己算候选（本仓 #31 的教训）。
 */
export interface RoutingPreviewItem {
  model: string;
  strategy: RouteStrategy;
  sticky: StickyMode;
  /** 生效组 id（优先级链第 3 环，或请求显式传入） */
  groupId?: string | null;
  /** 本次判定实际采用的层 */
  appliedLayers?: Layer[];
  /** 逐字段来源（本页 provenance 展示的权威来源） */
  provenance?: ProfileProvenance;
  /** 合并后的生效档案 */
  profile?: RoutingProfile;
  pickedId?: string | null;
  ordered?: RoutingQueueEntryItem[];
  blocked?: RoutingQueueEntryItem[];
  skipped?: SkipSummary;
}

/** 预览请求体：model + 能力需求 + 可选显式组 */
export interface RoutingPreviewRequest {
  model: string;
  needs: RequestNeeds;
  group_id?: string | null;
}

/** 档案字段名（provenance 表的行序与键） */
export const PROFILE_FIELDS = [
  'scope',
  'strategy',
  'sticky',
  'needs',
  'sort_rules',
  'default_group',
] as const;
export type ProfileField = (typeof PROFILE_FIELDS)[number];

// ── 三态推导 ────────────────────────────────────────────────────────

/** 界面三态：继承下层 / 显式覆盖 / 显式清空 */
export type FieldState = 'inherit' | 'override' | 'clear';

/** 空参与名单（= 不限制，即「清空」态的值） */
export const EMPTY_SCOPE: RoutingScope = {
  provider_scope: 'all',
  providers: [],
  disabled_providers: [],
  account_order: [],
  excluded_accounts: [],
};

/** 空能力需求（= 不要求任何能力，即「清空」态的值） */
export const EMPTY_NEEDS: RequestNeeds = { tools: false, images: false, video: false };

/** 空档案（全部继承；新建模型级档案与路由组时的初值） */
export function emptyProfile(): RoutingProfile {
  return {
    scope: null,
    strategy: null,
    sticky: null,
    needs: null,
    sort_rules: null,
    default_group: null,
  };
}

function isEmptyScope(v: RoutingScope): boolean {
  return (
    v.provider_scope === 'all' &&
    v.providers.length === 0 &&
    v.disabled_providers.length === 0 &&
    v.account_order.length === 0 &&
    v.excluded_accounts.length === 0
  );
}

function isEmptyNeeds(v: RequestNeeds): boolean {
  return !v.tools && !v.images && !v.video;
}

/** 枚举类字段（无「空」语义）：只有继承 / 覆盖 */
export function enumFieldState(v: string | null | undefined): FieldState {
  return v == null ? 'inherit' : 'override';
}

/** 读取某字段的三态（含「有值但为空」→ clear 的判定） */
export function fieldState(p: RoutingProfile, field: ProfileField): FieldState {
  switch (field) {
    case 'strategy':
    case 'sticky':
      return enumFieldState(p[field]);
    case 'scope': {
      const v = p.scope;
      if (v == null) return 'inherit';
      return isEmptyScope(v) ? 'clear' : 'override';
    }
    case 'needs': {
      const v = p.needs;
      if (v == null) return 'inherit';
      return isEmptyNeeds(v) ? 'clear' : 'override';
    }
    case 'sort_rules': {
      const v = p.sort_rules;
      if (v == null) return 'inherit';
      return v.length === 0 ? 'clear' : 'override';
    }
    case 'default_group': {
      const v = p.default_group;
      if (v == null) return 'inherit';
      return v.trim() === '' ? 'clear' : 'override';
    }
  }
}

/**
 * 按三态更新单层档案的某字段：
 * - `inherit` → `null`
 * - `clear`   → 该字段的「空」值（scope 空名单 / needs 全 false / `[]` / `''`）
 * - `override`→ 保留 `fallback`（调用方给的下层推算值或合理初值）
 */
export function setFieldState(
  p: RoutingProfile,
  field: ProfileField,
  state: FieldState,
  fallback: RoutingProfile,
): RoutingProfile {
  if (state === 'inherit') return { ...p, [field]: null };
  if (state === 'clear') {
    switch (field) {
      case 'scope':
        return { ...p, scope: { ...EMPTY_SCOPE } };
      case 'needs':
        return { ...p, needs: { ...EMPTY_NEEDS } };
      case 'sort_rules':
        return { ...p, sort_rules: [] };
      case 'default_group':
        return { ...p, default_group: '' };
      default:
        // strategy / sticky 无「空」语义：界面上不给清空档，
        // 真走到这里就退回继承（不制造非法值）
        return { ...p, [field]: null };
    }
  }
  // override：切到覆盖态时给一个可编辑的起点（优先沿用下层推算值）
  switch (field) {
    case 'scope':
      return { ...p, scope: p.scope ?? fallback.scope ?? { ...EMPTY_SCOPE } };
    case 'needs':
      return { ...p, needs: p.needs ?? fallback.needs ?? { ...EMPTY_NEEDS } };
    case 'sort_rules':
      return { ...p, sort_rules: p.sort_rules?.length ? p.sort_rules : [{ on: 'added', dir: 'asc' }] };
    case 'default_group':
      return { ...p, default_group: p.default_group || fallback.default_group || '' };
    default:
      return { ...p, [field]: p[field] ?? fallback[field] };
  }
}

/** 该档案是否为空档案（全部继承） */
export function isProfileEmpty(p: RoutingProfile): boolean {
  return PROFILE_FIELDS.every((f) => fieldState(p, f) === 'inherit');
}

/** 档案里「本层表过态」的字段个数（层列表上做密度提示用） */
export function profileOverrideCount(p: RoutingProfile): number {
  return PROFILE_FIELDS.filter((f) => fieldState(p, f) !== 'inherit').length;
}

/**
 * 单层档案 → 生效值（编辑器内的**推算**，仅用于「继承会得到什么」的提示）。
 *
 * 这是 §2.2 合并规则的纯字段直译（自左向右取最右侧非 null），
 * 不含任何选路逻辑 —— 候选裁剪与判定永远以后端 `plan_route` 为唯一口径，
 * 界面不自己算候选（本仓 #31 的教训）。
 */
export function mergeProfiles(lower: RoutingProfile, upper: RoutingProfile): RoutingProfile {
  return {
    scope: upper.scope ?? lower.scope,
    strategy: upper.strategy ?? lower.strategy,
    sticky: upper.sticky ?? lower.sticky,
    needs: upper.needs ?? lower.needs,
    sort_rules: upper.sort_rules ?? lower.sort_rules,
    default_group: upper.default_group ?? lower.default_group,
  };
}

/** 行内把名单文本（一行一个）切成数组：去空行、保序、去重 */
export function linesToItems(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of text.split('\n')) {
    const v = raw.trim();
    if (!v || seen.has(v)) continue;
    seen.add(v);
    out.push(v);
  }
  return out;
}

/** 数组 → 行内文本（与 linesToItems 互逆） */
export function itemsToLines(items: string[] | undefined): string {
  return (items ?? []).join('\n');
}
