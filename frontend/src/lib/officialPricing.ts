/**
 * 官方参考价 —— 用户侧价格页的「官方 ¥3.00 · 我们 1 折」对照数据源。
 *
 * **后端没有这个字段。** 证据（排查记录，勿重探）：
 * - `src/pricing/mod.rs::ModelPrice` 只有 `input_price` / `output_price` /
 *   `cache_price` / `price_type` 四个价格字段，**无 official / list_price / msrp /
 *   reference_price**（`grep -rn 'official|list_price|msrp|reference_price' src/`
 *   仅命中通道 X-Channel、品牌等无关字面量）。
 * - 随包的 `src/model/modelsdev.json`（models.dev 目录快照）只有
 *   `canonical / reasoning / toolCall / images / video / context / output / levels`，
 *   **没有 cost / price 键** → 无法从 models.dev 元数据取官方价。
 * - `src/pricing/price_sync.rs::RemoteModelPrice`（远程定价仓库条目）同样无官方价字段。
 *
 * 所以在官方字段落地前，官方价做成**管理员在本机填写**的参考表，存 localStorage：
 * 填了才显示折扣，没填显示「未登记」，绝不凭空造数。
 *
 * 已知的一批（线上实测基准，与后端每 1K 值换算后一致）：
 * - `deepseek-v4.1-flash` / `DeepSeek-V4.1-Flash`：官方输入 ¥3.00 / 百万，我们 ¥0.30 / 百万 → 1 折。
 * 输出与缓存读官方价未给，留空（不臆造）。
 */

/** 一份官方参考价（单位：元 / 百万 Token）。`null` = 该项未登记，不参与折扣。 */
export interface OfficialPrice {
  input: number | null;
  output: number | null;
  cache: number | null;
}

/** 内置默认表：只登记**有实测依据**的条目，其余留空等管理员补。 */
export const OFFICIAL_PRICE_SEED: Record<string, OfficialPrice> = {
  'deepseek-v4.1-flash': { input: 3.0, output: null, cache: null },
};

/** localStorage 键：管理员在本机维护的官方参考价覆盖表。 */
export const OFFICIAL_PRICE_STORAGE_KEY = 'aigx-official-prices';

/** 模型名归一化：小写 + 去空白，让 `DeepSeek-V4.1-Flash` 与 `deepseek-v4.1-flash` 命中同一条。 */
export function normalizeModelName(name: string): string {
  return name.trim().toLowerCase();
}

/** 把任意输入规整成合法条目：缺的键补 null，非有限数当 null（不在界面上写垃圾值）。 */
function sanitizeEntry(raw: unknown): OfficialPrice | null {
  if (!raw || typeof raw !== 'object') return null;
  const obj = raw as Record<string, unknown>;
  const num = (v: unknown): number | null => {
    // 显式 null / undefined / 空串 = 未登记，必须保持 null：
    // `Number(null) === 0` 会把「没填」悄悄变成「官方价 0 元」，
    // 进而让折扣计算把官方价 0 当成非法值以外的 0… 更糟的是界面上
    // 「未登记」被显示成「¥0.00」。这条判断不能靠 Number() 兜。
    if (v == null || (typeof v === 'string' && v.trim() === '')) return null;
    const n = Number(v);
    return Number.isFinite(n) && n >= 0 ? n : null;
  };
  const entry: OfficialPrice = {
    input: num(obj.input),
    output: num(obj.output),
    cache: num(obj.cache),
  };
  return entry;
}

/** 读当前生效的官方参考价表（内置种子 + 本机覆盖，覆盖优先）。 */
export function getOfficialPrices(): Record<string, OfficialPrice> {
  const merged: Record<string, OfficialPrice> = {};
  for (const [key, value] of Object.entries(OFFICIAL_PRICE_SEED)) {
    merged[normalizeModelName(key)] = value;
  }
  try {
    const raw = localStorage.getItem(OFFICIAL_PRICE_STORAGE_KEY);
    if (raw) {
      const parsed: unknown = JSON.parse(raw);
      if (parsed && typeof parsed === 'object') {
        for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
          const entry = sanitizeEntry(value);
          if (entry) merged[normalizeModelName(key)] = entry;
        }
      }
    }
  } catch {
    // 存盘坏：回落种子，不抛
  }
  return merged;
}

/**
 * 读单个模型的官方价；未登记返回 null。
 *
 * 全 null 的覆盖条目（`clearOfficialPrice` 写的）等同「未登记」：不能让种子把它
 * 顶回来，也不能让界面拿到一个「三项都是 null」的对象去算折扣。
 */
export function getOfficialPrice(model: string): OfficialPrice | null {
  const table = getOfficialPrices();
  const entry = table[normalizeModelName(model)];
  if (!entry) return null;
  if (entry.input == null && entry.output == null && entry.cache == null) return null;
  return entry;
}

/**
 * 保存某个模型的官方价（管理员在价格页填）。只写本机 localStorage：
 * 后端字段是另一条线，本模块不碰 `src/**`。
 *
 * **只写本机覆盖层，不写内置种子**：若把合并结果整体落盘，种子条目会被固化成本机
 * 覆盖，将来升级种子（新增官方价 / 纠正错价）就永远盖不动。读取侧本来就会合并，
 * 所以覆盖层只需装「管理员真的填过的那几条」。
 */
export function saveOfficialPrice(model: string, entry: OfficialPrice): void {
  const key = normalizeModelName(model);
  const overrides = getLocalOverrides();
  overrides[key] = {
    input: entry.input ?? null,
    output: entry.output ?? null,
    cache: entry.cache ?? null,
  };
  try {
    localStorage.setItem(OFFICIAL_PRICE_STORAGE_KEY, JSON.stringify(overrides));
  } catch {
    // 存储不可用：丢弃本次填写，界面按「未登记」显示
  }
}

/**
 * 清掉某个模型的官方价（回到「未登记」状态，界面上不再显示折扣）。
 *
 * 语义是「连种子一起不认」：管理员明确清除某模型时，内置种子也不该把它顶回来。
 * 所以这里先写一条全 null 的显式覆盖（覆盖优先于种子），再让 `getOfficialPrices`
 * 把全 null 条目当成「未登记」渲染——见 `getOfficialPrice` 的归一化处理。
 */
export function clearOfficialPrice(model: string): void {
  const key = normalizeModelName(model);
  const overrides = getLocalOverrides();
  overrides[key] = { input: null, output: null, cache: null };
  try {
    localStorage.setItem(OFFICIAL_PRICE_STORAGE_KEY, JSON.stringify(overrides));
  } catch {
    // 同上
  }
}

/**
 * 本机覆盖表里的条目（不含内置种子）——价格页据此区分「内置默认」与「管理员已改」。
 */
export function getLocalOverrides(): Record<string, OfficialPrice> {
  try {
    const raw = localStorage.getItem(OFFICIAL_PRICE_STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object') return {};
    const out: Record<string, OfficialPrice> = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      const entry = sanitizeEntry(value);
      if (entry) out[normalizeModelName(key)] = entry;
    }
    return out;
  } catch {
    return {};
  }
}
