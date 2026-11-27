/**
 * 预设库的「命中率」与「虚拟滚动」两件纯逻辑件（无 React）。
 *
 * ── 命中率为什么值得算出来 ─────────────────────────────────
 * 后端每条预设带 `model_match`（exact / vendor / universal / mismatch / unknown），
 * 但那是**逐条展示**用的：管理员要在 53 条列表里数「我这个模型到底套不套得上」。
 * 真正决定「会不会前插」的是 `preset_for_model`（写路径），后端已把它单独透出到
 * `data.auto_preset`（`{model, preset_id, match, applied}`）——这里只用这两份现成
 * 数据算一句话，**不重判、不猜**：写路径结论优先，逐条匹配只用于「列出候选」。
 *
 * ── 虚拟滚动为什么必须自己写 ───────────────────────────────
 * 依赖里没有虚拟列表（react-window / react-virtual 都没装，也不该为一个页面加）。
 * 页面里的 `.purge-preset-list` 是固定行高的列表（标题 + 元信息两行，含徽标时仍
 * 单行），因此「窗口化」只需三件事：固定行高、按滚动位置算可见区间、上下各垫
 * 一个占位 div。行高不固定时不能这么做——那会错位，所以这里把行高写成常量并与
 * CSS 里的 `min-height` 对齐（判据在测试里读 CSS 校验）。
 */

/** 预设列表行高（px）：与 `purge-presets.css` 的 `.purge-preset-item { min-height }` 一致 */
export const PRESET_ROW_HEIGHT = 62;

/** 可视区域外多渲染几行（滚动时不闪白） */
export const PRESET_OVERSCAN = 4;

export interface VirtualWindow {
  /** 第一条要渲染的下标 */
  start: number;
  /** 最后一条之后的下标（不含） */
  end: number;
  /** 上方占位高度（px） */
  padTop: number;
  /** 下方占位高度（px） */
  padBottom: number;
}

/**
 * 算可见窗口（纯函数）。
 *
 * 边界刻意写清：条数不足一屏时返回全量（不做任何占位，让布局与「没有虚拟化」时
 * 逐像素一致——这保证了 53 条以下的小列表行为零变化）；滚动位置超出总高时回夹，
 * 否则 `start` 会越过尾行、列表看起来凭空空了。
 */
export function virtualWindow(
  count: number,
  scrollTop: number,
  viewportHeight: number,
  rowHeight: number = PRESET_ROW_HEIGHT,
  overscan: number = PRESET_OVERSCAN,
): VirtualWindow {
  if (count <= 0 || rowHeight <= 0) {
    return { start: 0, end: 0, padTop: 0, padBottom: 0 };
  }
  const totalHeight = count * rowHeight;
  const clampedTop = Math.min(Math.max(0, scrollTop), Math.max(0, totalHeight - rowHeight));
  const perScreen = Math.max(1, Math.ceil(Math.max(0, viewportHeight) / rowHeight));
  const rawStart = Math.floor(clampedTop / rowHeight) - overscan;
  const rawEnd = Math.floor(clampedTop / rowHeight) + perScreen + overscan;
  const start = Math.max(0, Math.min(rawStart, count));
  const end = Math.max(start, Math.min(rawEnd, count));
  return {
    start,
    end,
    padTop: start * rowHeight,
    padBottom: (count - end) * rowHeight,
  };
}

/** 一条预设的匹配档（后端 `PresetModelMatch` 的 snake_case 形态 + 未传模型的 null）。 */
export type PresetMatchKind = 'exact' | 'vendor' | 'universal' | 'mismatch' | 'unknown';

/** 写路径命中档（后端 `PresetMatch` —— 与展示口径的 `PresetModelMatch` 不是同一个枚举） */
export type WritePathMatchKind = 'exact' | 'family' | 'universal' | 'pinned';

/**
 * 命中率摘要：**只回答一个问题**——「当前生效模型会不会被套上预设」。
 *
 * `writePath` 是后端 `data.auto_preset`（`preset_for_model` 的结论，与数据面同链）：
 * - 传了 `writePath` 就以它为准（这是唯一能说「会 / 不会」的判据）；
 * - 没传（旧后端）时退回逐条统计：`exact + vendor > 0` 记为「至少能套上一类」，
 *   **不说「命中率 %」**——逐条徽标不是写路径，把 12/53 说成「命中率 23%」是假话。
 *
 * `applied === false` 时还要区分「没命中」与「命中了但没开 / 总开关关着」：
 * 后端只在 `enabled && auto_preset && body 非空` 时给 true，所以 false 可能是
 * 配置问题而不是覆盖问题——文案必须分开，否则管理员会去改模型名。
 */
export interface PresetCoverage {
  /** 有写路径结论（能说「会 / 不会套」） */
  decisive: boolean;
  /** 写路径会不会前插正文 */
  applied: boolean;
  /** 写路径命中档（null = 旧后端或无结论） */
  match: WritePathMatchKind | null;
  /** 命中的预设 id（null = 没命中） */
  presetId: string | null;
  /** 逐条统计（用于列表排序提示与「有 N 条候选」） */
  counts: Record<PresetMatchKind | 'total', number>;
}

export function presetCoverage(
  items: ReadonlyArray<{ model_match?: PresetMatchKind | null }>,
  writePath?: {
    match?: WritePathMatchKind | null;
    preset_id?: string | null;
    applied?: boolean;
  } | null,
): PresetCoverage {
  const counts: Record<PresetMatchKind | 'total', number> = {
    exact: 0,
    vendor: 0,
    universal: 0,
    mismatch: 0,
    unknown: 0,
    total: items.length,
  };
  for (const item of items) {
    const kind = item.model_match;
    if (kind === 'exact' || kind === 'vendor' || kind === 'universal' || kind === 'mismatch' || kind === 'unknown') {
      counts[kind] += 1;
    }
  }
  if (writePath) {
    return {
      decisive: true,
      applied: writePath.applied === true,
      match: writePath.match ?? null,
      presetId: writePath.preset_id ?? null,
      counts,
    };
  }
  return { decisive: false, applied: false, match: null, presetId: null, counts };
}

/**
 * 命中率的一句话文案（i18n key + 参数；调用方 `t()`）。
 *
 * 四种状态各自一句，**不合并**：
 * - 能套：报命中的是哪一档 + 预设 id（「为什么是它」）；
 * - 没开自动套用 / 总开关关着：报配置原因；
 * - 命中了但正文空：报内容原因（后端 `applied=false` 且 `preset_id` 有值）；
 * - 完全不命中：报「无预设可套」并给候选数量。
 */
export interface CoverageLine {
  key: string;
  params?: Record<string, string | number>;
  tone: 'on' | 'off' | 'warn';
}

export function coverageLine(
  coverage: PresetCoverage,
  model: string,
  enabled: boolean,
  autoPreset: boolean,
): CoverageLine {
  const modelLabel = model.trim() || '—';
  if (!enabled) {
    return { key: '总开关关闭：任何模型都不会被套预设', tone: 'off' };
  }
  if (!autoPreset) {
    return { key: '「按模型自动套用」是关的：不会套任何预设', tone: 'off' };
  }
  if (!coverage.decisive) {
    // 旧后端：只能给「有 N 条可能是候选」，不能说会不会套
    const candidates = coverage.counts.exact + coverage.counts.vendor;
    return candidates > 0
      ? {
          key: '当前模型至少命中 {{n}} 条候选预设（逐条徽标口径，未含写路径结论）',
          params: { n: candidates },
          tone: 'warn',
        }
      : { key: '当前模型没有命中任何候选预设（逐条徽标口径）', tone: 'warn' };
  }
  if (coverage.applied) {
    return {
      key: '当前模型会套预设：{{model}} → {{id}}',
      params: { model: modelLabel, id: coverage.presetId ?? '—' },
      tone: 'on',
    };
  }
  if (coverage.presetId) {
    // 命中了但没套：后端只在总开关/自动套用/正文非空三者同时成立时 applied=true，
    // 走到这里说明正文为空（其余两个分支已在上面返回）
    return { key: '命中了预设「{{id}}」但正文为空，不会前插', params: { id: coverage.presetId }, tone: 'warn' };
  }
  return { key: '当前模型 {{model}} 没有可套的预设', params: { model: modelLabel }, tone: 'warn' };
}