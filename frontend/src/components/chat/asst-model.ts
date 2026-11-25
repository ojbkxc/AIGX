/**
 * 模型 / 思考等级的选择口径（对齐 A2A v3.5.1 `assistant-models.ts` +
 * `assistant-model-state.ts` + `models-reasoning.ts` 的候选与降级）。
 *
 * ── 思考等级：候选从哪来，为什么要三级回落 ───────────────────
 * A2A 的 `levels()` 是三级：① 模型自己声明的档位（`model.reasoningLevels`）
 * → ② 后端下发的通用候选表（`model_rules::REASONING_LEVELS`）→ ③ 本地兜底表。
 * 前两级都在说「这个模型/这个网关认哪些档」，第三级只在「模型没声明 + 老网关
 * / 首次加载失败」时兜底。
 *
 * AIGX 的现状：
 *   ① 有数据源 —— `ModelMetaOverride.reasoning_levels`（`/api/models/meta`）与
 *      `ManagedModel.cap_sources.reasoning_levels`（`/api/models/manage`）都下发了
 *      模型声明的档位；
 *   ② 没有网关级候选表端点（`reasoning_effort_options` 那类东西不存在）；
 *   ③ 本地兜底 = Agent 后端真正接受的四个值（`src/agent/api.rs` 的 match：
 *      `off` / `low` / `medium` / `high`，其它值**静默忽略**）。
 *
 * 所以这里刻意**不**照抄 A2A 的八档兜底表（`minimal` / `xhigh` / `max`）：
 * 那不是「更全」，而是「发出去会被后端丢掉、界面却显示已选」。回落链改为
 * ①模型声明 → ②AIGX 四档，并且**只保留后端认得的档位**（超出的一律并到最近的
 * 合法档，并在 `dropped` 里如实返回，界面可提示「该模型有档位本网关不支持」）。
 *
 * ── 模型列表为空时的降级 ────────────────────────────────────
 * A2A：`modelsError` → 「读取模型清单失败」；`!ready` → 「正在读取可用模型…」；
 * 当前模型不在列表 → 「模型当前不在可用列表中，请重新选择」（但**不清空**已选
 * 的值，否则用户一刷新就把选择丢了）。
 */

/** AIGX Agent 后端真正接受的档位（`src/agent/api.rs` 的 match 值域） */
export const AGENT_EFFORT_LEVELS = ['off', 'low', 'medium', 'high'] as const;

/** 与 `components/chat/EffortPicker` 的 `Effort` 结构兼容（不 import：那个文件属另一位成员） */
export type EffortLevel = (typeof AGENT_EFFORT_LEVELS)[number];

/** 候选来源（界面据此决定要不要显示「兜底」提示） */
export type EffortSource = 'model' | 'gateway' | 'fallback';

export interface EffortResolution {
  /** 渲染用的档位（顺序即后端顺序，恒在 `AGENT_EFFORT_LEVELS` 值域内） */
  options: EffortLevel[];
  source: EffortSource;
  /** 模型声明了、但 AIGX 后端不认的档位（如实返回，便于提示） */
  dropped: string[];
}

const isEffortLevel = (value: unknown): value is EffortLevel =>
  typeof value === 'string' && (AGENT_EFFORT_LEVELS as readonly string[]).includes(value);

/** 任意档位串 → 最近的合法档（未知值不猜「高」：落到 `off` 更安全） */
export function normalizeEffort(raw: unknown): EffortLevel {
  if (typeof raw !== 'string') return 'off';
  const value = raw.trim().toLowerCase();
  if (isEffortLevel(value)) return value;
  if (value === 'none' || value === '') return 'off';
  if (value === 'minimal') return 'low';
  if (value === 'xhigh' || value === 'max') return 'high';
  return 'off';
}

/**
 * 解析候选档位。
 *
 * @param declared  模型自己声明的档位（`reasoning_levels`；空数组 = 明确「不支持」）
 * @param gateway   后端通用候选表（AIGX 暂无数据源，留位）
 */
export function resolveEffortOptions(input: {
  declared?: readonly unknown[];
  gateway?: readonly unknown[];
} = {}): EffortResolution {
  const declared = Array.isArray(input.declared) ? input.declared : [];
  if (declared.length) {
    const options: EffortLevel[] = [];
    const dropped: string[] = [];
    for (const raw of declared) {
      if (isEffortLevel(raw)) {
        if (!options.includes(raw)) options.push(raw);
        continue;
      }
      if (typeof raw === 'string' && raw.trim()) dropped.push(raw.trim());
    }
    if (options.length) return { options: orderLevels(options), source: 'model', dropped };
    // 声明了但全是本网关不认的档：不静默回落到四档，否则界面会显示「已选 low」
    // 而模型根本不接受该参数 —— 让调用方看到空候选并禁用选择器。
    return { options: [], source: 'model', dropped };
  }
  const gateway = Array.isArray(input.gateway) ? input.gateway : [];
  const fromGateway = gateway.filter(isEffortLevel);
  if (fromGateway.length) {
    return { options: orderLevels(fromGateway), source: 'gateway', dropped: [] };
  }
  return { options: [...AGENT_EFFORT_LEVELS], source: 'fallback', dropped: [] };
}

/** 按后端顺序排（避免「声明的顺序」决定界面顺序，同一网关不同模型长得不一样） */
function orderLevels(levels: readonly EffortLevel[]): EffortLevel[] {
  return AGENT_EFFORT_LEVELS.filter((level) => levels.includes(level));
}

/** 当前档位在候选里是否合法；不合法时给最近的合法值（用于切换模型后的收敛） */
export function reconcileEffort(current: unknown, options: readonly EffortLevel[]): EffortLevel {
  if (!options.length) return 'off';
  if (isEffortLevel(current) && options.includes(current)) return current;
  return options.includes('off') ? 'off' : options[0];
}

/* ─── 模型列表 ─────────────────────────────────────────────── */

/** 只需要 id（AIGX `/api/models/available` 的条目形状见 `ModelInfo`） */
export interface ModelLike {
  id?: unknown;
}

/** 模型名清单（去空、去重、保持后端顺序） */
export function modelNames(models: readonly ModelLike[] | undefined | null): string[] {
  if (!Array.isArray(models)) return [];
  const out: string[] = [];
  for (const model of models) {
    const id = typeof model?.id === 'string' ? model.id.trim() : '';
    if (id && !out.includes(id)) out.push(id);
  }
  return out;
}

/** 当前选择是否还在可用列表里（列表为空 = 未知，不判「不可用」） */
export function modelAvailability(
  names: readonly string[] | undefined | null,
  current: string | undefined | null,
): { available: boolean; unknown: boolean } {
  const list = Array.isArray(names) ? names : [];
  const name = typeof current === 'string' ? current.trim() : '';
  if (!list.length) return { available: true, unknown: true };
  if (!name) return { available: false, unknown: false };
  return { available: list.includes(name), unknown: false };
}

/**
 * 模型的能力位（`supportsImages` 等）。
 *
 * AIGX 的两处数据源口径不同：`/api/models/manage` 的 `capability_flags.supportsImages`
 * 是**能力三态**（null 表示「不知道」）；`ModelMetaOverride.capabilities` 是
 * 字符串能力数组。这里两种都接，取第一个有明确值的。
 */
export function modelSupportsImages(model: {
  capability_flags?: { supportsImages?: boolean | null } | null;
  capabilities?: readonly string[] | null;
} | undefined | null): boolean | undefined {
  const flag = model?.capability_flags?.supportsImages;
  if (typeof flag === 'boolean') return flag;
  const kinds = Array.isArray(model?.capabilities) ? model?.capabilities : undefined;
  if (!kinds) return undefined;
  if (kinds.includes('vision') || kinds.includes('image')) return true;
  return undefined;
}
