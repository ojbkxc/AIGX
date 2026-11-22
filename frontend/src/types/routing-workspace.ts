/**
 * 路由工作台 + 特殊路由（压缩 / 标题 / 视觉兼容）前端类型。
 *
 * 与 AIGX 后端同构的锚点（契约可能随 be-special / be-vision / be-routing
 * 落地微调 —— 所有字段都按「可选 + 防御性守卫」读取，缺字段时降级而不是崩）：
 * - 压缩路由：`src/route_special/compact.rs`   `CompactRoutes { enabled, rules[{ client, source, target, enabled }] }`
 *   （serde camelCase + deny_unknown_fields；规则级 enabled 缺省 true）
 * - 标题路由：`src/route_special/title.rs`     `TitleRoutes { enabled, rules[{ enabled, clients[], model, fallback }] }`
 * - 视觉兼容：`src/image_compat/vision.rs`     `VisionSettings { enabled, rules[], unknownCapability, timeoutSeconds }`
 *   （规则形状对齐 Agent2API `config/vision_compat.rs`，assist / reroute 两组字段随规则存）
 * - 工作台：GET /api/routing/{workspace,flow,trace,trace/request}（be-routing）
 *
 * 所有 GET 响应按 `{ success, data }` 包装读取；保存请求体：
 *   `{ config, revision }` —— revision 取最近一次 GET 的值，PUT 冲突返回 409。
 */
import type { RoutingPolicyItem, RoutingQueueEntryItem, RoutingQueueItem } from './index';
import type { RoutingProfile, Layer, ProfileProvenance, RoutingQueueWithLayers } from './routing-profile';

/* ─────────────── 特殊路由：压缩 / 标题 ─────────────── */

/** 压缩路由规则（`route_special::compact::CompactRoute`） */
export interface CompactRouteRule {
  client: string;
  source: string;
  target: string;
  enabled: boolean;
}

export interface CompactRoutesConfig {
  enabled: boolean;
  rules: CompactRouteRule[];
}

/** 标题路由规则（`route_special::title::TitleRule`；按列表顺序第一条命中生效） */
export type TitleFallback = 'original' | 'error';

export interface TitleRouteRule {
  enabled: boolean;
  clients: string[];
  model: string;
  fallback: TitleFallback;
}

export interface TitleRoutesConfig {
  enabled: boolean;
  rules: TitleRouteRule[];
}

/** 客户端目录项（压缩只有 id/label；标题多一个「后端有识别判据」标志） */
export interface SpecialRouteClient {
  id: string;
  label: string;
  supported?: boolean;
}

/**
 * 模型下拉的 wire 形状：后端可能下发纯字符串或带展示名的对象。
 * 读取一律经 [`modelChoices`] 归一，不假定某一种。
 */
export type SpecialModelWire =
  | string
  | {
      id?: string;
      model?: string;
      name?: string;
      provider?: string;
      providerLabel?: string;
      kind?: string;
      capabilities?: { supportsImages?: boolean | null };
    };

export interface SpecialModelChoice {
  id: string;
  label: string;
  provider?: string;
  supportsImages?: boolean | null;
}

/** GET /api/models/compact-routes 的 data 形状 */
export interface CompactRoutesView {
  config?: Partial<CompactRoutesConfig> | null;
  /** 后端解析已存配置失败时的错误原文（此时 config 为 null，面板须如实告知并禁止覆盖保存） */
  error?: string | null;
  revision?: string;
  clients?: SpecialRouteClient[];
  models?: SpecialModelWire[];
  maxRules?: number;
}

/** GET /api/models/title-routes 的 data 形状 */
export interface TitleRoutesView {
  config?: Partial<TitleRoutesConfig> | null;
  /** 同 compact：已存配置解析失败时的错误原文 */
  error?: string | null;
  revision?: string;
  clients?: SpecialRouteClient[];
  models?: SpecialModelWire[];
  fallbackModes?: string[];
  maxRules?: number;
}

/* ─────────────── 特殊路由：视觉兼容 ─────────────── */

export type VisionMode = 'assist' | 'reroute';
export type VisionSource = 'shared' | 'custom';
export type VisionFallback = 'stop' | 'designated';
export type VisionUnknownCapability = 'try_original' | 'compat';

export interface VisionModelRef {
  provider: string;
  model: string;
}

export interface VisionTarget {
  provider: string;
  model: string;
  reasoning: string;
}

/**
 * 一条视觉兼容规则。`mode` 决定用哪一组字段：
 * - `assist`  用 `source` / `target` / `reuseResults`
 * - `reroute` 用 `preferCurrentRoute` / `fallback` / `targets`
 * 另一组字段照样随规则保存（切回该方式时之前填的还在）——与后端同构。
 */
export interface VisionRule {
  enabled: boolean;
  models: VisionModelRef[];
  mode: VisionMode;
  source: VisionSource;
  target: VisionTarget | null;
  reuseResults: boolean;
  preferCurrentRoute: boolean;
  fallback: VisionFallback;
  targets: VisionTarget[];
}

/**
 * 视觉兼容整块配置。`assist` / `reroute` 是后端为旧界面保留的投影；
 * `mode` / `overrides` 是上游旧形状 —— 本页原样带着走、不直接编辑。
 */
export interface VisionSettingsConfig {
  enabled: boolean;
  rules: VisionRule[];
  assist?: {
    models?: VisionModelRef[];
    source?: VisionSource;
    target?: VisionTarget | null;
    reuseResults?: boolean;
  };
  reroute?: {
    models?: VisionModelRef[];
    preferCurrentRoute?: boolean;
    fallback?: VisionFallback;
    targets?: VisionTarget[];
  };
  unknownCapability?: VisionUnknownCapability;
  timeoutSeconds?: number;
  mode?: VisionMode | null;
  overrides?: unknown[];
}

/** 视觉模型候选（带提供商与图片能力，规则里按 provider+model 引用） */
export interface VisionModelChoice {
  provider: string;
  providerLabel?: string;
  model: string;
  name?: string;
  supportsImages?: boolean | null;
}

/** GET /api/vision-compatibility 的 data 形状 */
export interface VisionCompatView {
  config?: Partial<VisionSettingsConfig> | null;
  revision?: string;
  models?: VisionModelChoice[];
  sharedTarget?: VisionTarget | null;
  maxTargets?: number;
  maxModels?: number;
}

/* ─────────────── 归一函数（wire → 渲染模型） ─────────────── */

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null;
}

/** 模型 wire → `{id,label}`；无法识别返回 null（调用方过滤）。 */
function toModelChoice(raw: SpecialModelWire | unknown): SpecialModelChoice | null {
  if (typeof raw === 'string') {
    const id = raw.trim();
    return id ? { id, label: id } : null;
  }
  if (!isRecord(raw)) return null;
  const id = String(raw.id ?? raw.model ?? '').trim();
  if (!id) return null;
  const label = String(raw.name ?? raw.id ?? raw.model ?? id);
  const provider = raw.provider != null ? String(raw.provider) : undefined;
  const providerLabel = raw.providerLabel != null ? String(raw.providerLabel) : undefined;
  const caps = isRecord(raw.capabilities) ? raw.capabilities : null;
  const supportsImages =
    caps && typeof caps.supportsImages === 'boolean' ? caps.supportsImages : undefined;
  return {
    id,
    label: providerLabel || provider ? `${providerLabel || provider} · ${label}` : label,
    provider,
    supportsImages,
  };
}

/** 模型列表归一（去空、按 id 去重、保序）。 */
export function modelChoices(raw: SpecialModelWire[] | undefined): SpecialModelChoice[] {
  if (!Array.isArray(raw)) return [];
  const seen = new Set<string>();
  const out: SpecialModelChoice[] = [];
  for (const item of raw) {
    const choice = toModelChoice(item);
    if (!choice || seen.has(choice.id)) continue;
    seen.add(choice.id);
    out.push(choice);
  }
  return out;
}

/** 压缩配置归一：缺 rules / 规则字段时给安全默认（服务端默认 enabled=true）。 */
export function normalizeCompactConfig(raw: Partial<CompactRoutesConfig> | null | undefined): CompactRoutesConfig {
  const rules = Array.isArray(raw?.rules) ? raw.rules : [];
  return {
    enabled: raw?.enabled !== false,
    rules: rules.map((rule) => ({
      client: String(rule?.client ?? ''),
      source: String(rule?.source ?? ''),
      target: String(rule?.target ?? ''),
      enabled: rule?.enabled !== false,
    })),
  };
}

/** 标题配置归一。 */
export function normalizeTitleConfig(raw: Partial<TitleRoutesConfig> | null | undefined): TitleRoutesConfig {
  const rules = Array.isArray(raw?.rules) ? raw.rules : [];
  return {
    enabled: raw?.enabled !== false,
    rules: rules.map((rule) => ({
      enabled: rule?.enabled !== false,
      clients: Array.isArray(rule?.clients) ? rule.clients.map(String) : [],
      model: String(rule?.model ?? ''),
      fallback: rule?.fallback === 'error' ? 'error' : 'original',
    })),
  };
}

/** 视觉目标归一：保证三个字段齐全（后端 `reasoning` 缺省为空串）。 */
function normalizeVisionTarget(raw: VisionTarget | null | undefined): VisionTarget | null {
  if (!raw || !String(raw.provider ?? '').trim() || !String(raw.model ?? '').trim()) return null;
  return {
    provider: String(raw.provider),
    model: String(raw.model),
    reasoning: String(raw.reasoning ?? ''),
  };
}

/** 视觉规则归一（旧记录缺字段时按后端 `Default` 同口径补齐）。 */
function normalizeVisionRule(raw: Partial<VisionRule> | null | undefined): VisionRule {
  return {
    enabled: raw?.enabled !== false,
    models: (Array.isArray(raw?.models) ? raw.models : [])
      .filter((m): m is VisionModelRef => Boolean(m && String(m.provider ?? '').trim() && String(m.model ?? '').trim()))
      .map((m) => ({ provider: String(m.provider), model: String(m.model) })),
    mode: raw?.mode === 'reroute' ? 'reroute' : 'assist',
    source: raw?.source === 'custom' ? 'custom' : 'shared',
    target: normalizeVisionTarget(raw?.target ?? null),
    reuseResults: raw?.reuseResults !== false,
    /** 结构性缺口：AIGX 视觉裁决在选路前，`prefer_current_route` 不适用。
     *  后端 GET 恒回 false、PUT 拒绝 true；后端默认值现也已是 false，
     *  这里归一为 false 仅为显式化（不依赖前端补默认）。 */
    preferCurrentRoute: false,
    /** 后端 `validate` 只要求 reroute 规则的 `targets` 非空，`fallback` 不参与
     *  裁决；发 designated 仅为兼容老形状。assist 规则归一为 stop 即可。 */
    fallback: raw?.mode === 'reroute' ? 'designated' : 'stop',
    targets: (Array.isArray(raw?.targets) ? raw.targets : [])
      .map((t) => normalizeVisionTarget(t))
      .filter((t): t is VisionTarget => t !== null),
  };
}

/** 视觉配置归一。 */
export function normalizeVisionConfig(
  raw: Partial<VisionSettingsConfig> | null | undefined,
): VisionSettingsConfig {
  const rules = Array.isArray(raw?.rules) ? raw.rules : [];
  return {
    enabled: raw?.enabled !== false,
    rules: rules.map((rule) => normalizeVisionRule(rule)),
    ...(raw?.assist ? { assist: raw.assist } : {}),
    ...(raw?.reroute ? { reroute: raw.reroute } : {}),
    unknownCapability: raw?.unknownCapability === 'compat' ? 'compat' : 'try_original',
    timeoutSeconds: Number.isFinite(raw?.timeoutSeconds) ? Number(raw?.timeoutSeconds) : 30,
    ...(raw?.mode ? { mode: raw.mode } : {}),
    ...(Array.isArray(raw?.overrides) ? { overrides: raw.overrides } : {}),
  };
}

/**
 * 保存前把规则列表折回 `assist` / `reroute` 两块投影（与后端
 * `VisionSettings::fold_blocks` 同口径）。
 *
 * 为什么必须做：PUT 校验 (`vision::validate`) 在 `rules` 非空时仍会检查
 * `reroute` 块 —— 要求 `preferCurrentRoute=false` 且 `targets` 非空
 * （`fallback` 不参与裁决，这里发 designated 仅为兼容老形状）。若把 GET 时的
 * 旧投影原样带回，用户新增/修改规则后会因「块与规则不一致」被 400。
 * 这里按后端规则重算，纯机械投影、不做裁决。
 *
 * 同时：有规则时清掉旧版顶层字段（`mode` / `overrides`），否则
 * `is_legacy()` 为真会让后端走旧全局分支而忽略规则列表。
 */
export function foldVisionBlocks(config: VisionSettingsConfig): VisionSettingsConfig {
  const rules = config.rules;
  if (!rules.length) return config;
  const unionModels = (list: VisionModelRef[]): VisionModelRef[] => {
    const seen = new Set<string>();
    const out: VisionModelRef[] = [];
    for (const m of list) {      const key = `${m.provider.toLowerCase()}\u0000${m.model.toLowerCase()}`;
      if (!seen.has(key)) {
        seen.add(key);
        out.push({ provider: m.provider, model: m.model });
      }
    }
    return out;
  };
  const assists = rules.filter((r) => r.mode === 'assist');
  const reroutes = rules.filter((r) => r.mode === 'reroute');
  const firstA = assists[0];
  const firstR = reroutes[0];
  return {
    enabled: config.enabled,
    rules,
    assist: {
      models: unionModels(assists.flatMap((r) => r.models)),
      source: firstA?.source ?? 'shared',
      target: firstA?.target ?? null,
      reuseResults: firstA?.reuseResults ?? true,
    },
    reroute: {
      models: unionModels(reroutes.flatMap((r) => r.models)),
      preferCurrentRoute: false,
      fallback: firstR?.fallback ?? 'stop',
      targets: firstR?.targets ?? [],
    },
    unknownCapability: config.unknownCapability ?? 'try_original',
    timeoutSeconds: config.timeoutSeconds ?? 30,
  };
}

/** 视觉模型候选归一（兼容 `{provider, model, name}` 与 `models` 直出的对象）。 */
export function visionModelChoices(raw: unknown): VisionModelChoice[] {
  if (!Array.isArray(raw)) return [];
  const seen = new Set<string>();
  const out: VisionModelChoice[] = [];
  for (const item of raw) {
    if (!isRecord(item)) continue;
    const provider = String(item.provider ?? '').trim();
    const model = String(item.model ?? item.id ?? '').trim();
    if (!provider || !model) continue;
    const key = `${provider.toLowerCase()}\u0000${model.toLowerCase()}`;
    if (seen.has(key)) continue;
    seen.add(key);
    const caps = isRecord(item.capabilities) ? item.capabilities : null;
    out.push({
      provider,
      providerLabel: item.providerLabel != null ? String(item.providerLabel) : undefined,
      model,
      name: item.name != null ? String(item.name) : undefined,
      supportsImages:
        caps && typeof caps.supportsImages === 'boolean' ? caps.supportsImages : undefined,
    });
  }
  return out;
}

/* ─────────────── 路由工作台：workspace / flow / trace ─────────────── */

/**
 * GET /api/routing/workspace?model= 的 data 形状。
 * 对齐 Agent2API `routing-types.ts` 的 `RoutingWorkspace` 子集：
 * 本页只用「模型覆盖对比」需要的字段，其余原样忽略。
 */
export interface RoutingWorkspaceView {
  model?: string | null;
  customized?: boolean;
  configuredModels?: string[];
  models?: string[];
  profile?: RoutingProfile | null;
  inheritedProfile?: RoutingProfile | null;
  /** 逐字段来源（后端顶层下发；缺省 = 来源未知，界面不猜） */
  provenance?: ProfileProvenance | null;
  /** 本次判定实际命中的层 */
  appliedLayers?: Layer[];
  /** 多规则总开关（后端 `profilesEnabled`） */
  profilesEnabled?: boolean;
  queue?: Partial<RoutingQueueItem & RoutingQueueWithLayers> & {
    pickedId?: string | null;
    lastRequestModel?: string;
    ordered?: RoutingQueueEntryItem[];
    blocked?: RoutingQueueEntryItem[];
  };
  settings?: Partial<RoutingPolicyItem> & Record<string, unknown>;
  globalSettings?: Partial<RoutingPolicyItem>;
  revision?: string | null;
  /** 该端点是否支持修订号（AIGX 工作台恒 false：写入口是 PUT /api/routing*） */
  revisionSupported?: boolean;
  updatedAt?: number;
  stickyCount?: number;
  modelLastUsed?: Record<string, number>;
}

export type RoutingFlowScope = 'window' | 'running';

export interface RoutingFlowQuery {
  minutes: number;
  scope: RoutingFlowScope;
  /** 只看某个请求模型的路径（A2A 同名参数；UI 暂未使用） */
  model?: string;
}

export interface RoutingFlowPath {
  id?: string;
  client?: string;
  clientLabel?: string;
  model?: string;
  provider?: string;
  providerLabel?: string;
  accountId?: string;
  accountName?: string;
  requests?: number;
  running?: number;
  successful?: number;
  failed?: number;
  sessions?: number;
  stickyHits?: number;
  switched?: number;
  lastSeen?: number;
}

export interface RoutingFlowSnapshot {
  available?: boolean;
  paths?: RoutingFlowPath[];
  pathCount?: number;
  truncated?: boolean;
  requests?: number;
  running?: number;
  sessions?: number;
  unknownRequests?: number;
  now?: number;
  since?: number;
  /** 端点 `detail=requests` 时同形返回的匹配数（个别后端把 matched 放在顶层） */
  matched?: number;
  /** 有在途请求无法归因到具体路径（缺 provider/channel 字段）——只计入顶层 running */
  runningUnattributed?: boolean;
  /** 后端明确告知本次请求里哪些过滤维度不可兑现（如 account） */
  unsupportedFilters?: string[];
  /** `detail=sessions` 时为 false：RequestLog 无会话标识 */
  supported?: boolean;
}

export interface RoutingTraceQuery {
  offset?: number;
  limit?: number;
  /** 请求 / 模型 / 账号模糊搜索（A2A 同名参数） */
  search?: string;
  model?: string;
  /** HTTP 状态过滤（A2A 同名参数；UI 暂未使用） */
  status?: string;
}

/** 跟踪列表行 / 详情共用的扁平形状（对齐回放快照的字段名）。 */
export interface RoutingTraceRow {
  id: string;
  ts?: number;
  model?: string;
  clientModel?: string;
  upstreamModel?: string;
  provider?: string;
  providerLabel?: string;
  accountId?: string;
  accountName?: string;
  client?: string;
  clientLabel?: string;
  status?: number;
  error?: string | null;
  durationMs?: number;
  attempts?: number;
  phase?: string;
  attemptDetails?: Array<{
    index?: number;
    channel_id?: string;
    channel_name?: string;
    provider?: string;
    model?: string;
    stickyHit?: boolean;
    startedAt?: number;
    finishedAt?: number | null;
    status?: number | null;
    error?: string | null;
    notice?: string | null;
  }>;
  route?: {
    ordered?: RoutingQueueEntryItem[];
    blocked?: RoutingQueueEntryItem[];
    strategy?: string;
    sticky?: string;
    stickyHit?: boolean;
    picked?: string;
    /** 特殊请求路由改写快照（后端 #7 记录后下发；缺省 = 该请求未被改写）。
     *  形状对齐上游 routing-types.ts 的 route.compactRoute/titleRoute/visionCompatibility。 */
    compactRoute?: {
      source?: string;
      target?: string;
      client?: string;
    } | null;
    titleRoute?: {
      source?: string;
      target?: string;
      client?: string;
    } | null;
    visionCompatibility?: {
      mode?: string;
      calls?: unknown[];
      target?: { model?: string } | null;
    } | null;
  } | null;
}

export interface RoutingTraceList {
  entries?: RoutingTraceRow[];
  matched?: number;
  available?: boolean;
}

/** GET /api/routing/replays 的查询参数（A2A 同名端点）。 */
export interface RoutingReplaysQuery {
  model?: string;
  offset?: number;
  limit?: number;
  /** error / sticky */
  status?: string;
}

/** GET /api/routing/replays 的 data（与 trace 列表同形的回放行）。 */
export interface RoutingReplaysList {
  available?: boolean;
  entries?: RoutingTraceRow[];
  matched?: number;
  offset?: number;
  limit?: number;
}

/** 时间窗选项（5/10/30/60 分钟，与后端 flow/trace 的 minutes 参数同口径） */
export const FLOW_WINDOWS = [5, 10, 30, 60] as const;
