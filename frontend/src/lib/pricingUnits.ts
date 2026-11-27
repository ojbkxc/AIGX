/**
 * 定价单位换算 —— 界面口径「元 / 百万 Token」与后端口径「每 1K token」之间的纯函数。
 *
 * 后端契约（**一个字节都不能改**）：`pricing::ModelPrice` 的 `input_price` /
 * `output_price` / `cache_price` 是**每 1K token** 的价，KV 键 `price:xxx` 存的也是它，
 * 计费公式 `cost = (input_tokens * input_price + output_tokens * output_price) / 1000`
 * 直接依赖它（`src/pricing/mod.rs:7`、`src/pricing/mod.rs:70` 的注释）。
 *
 * 因此换算**只在前端**：界面值 = 后端值 × 1000，写回时再 ÷ 1000。
 * 本模块不做任何 IO，供成本价编辑器、模型管理定价弹窗与用户价格页共用，
 * 避免三个地方各写一份换算、写歪其中一份。
 */

/** 每百万 Token = 每 1K Token × 1000。 */
export const PER_MILLION_SCALE = 1000;

/** 换算的精度档位：12 位小数。价格最小到「每百万 0.0000001 元」仍可分辨。 */
const ROUND_SCALE = 1e12;

/**
 * 把浮点结果压回可打印的十进制：`0.30000000000000004 → 0.3`、
 * `0.0025000000000000005 → 0.0025`。
 *
 * 非有限值原样返回（NaN / Infinity 由调用方判空），不做「把 NaN 变 0」这类
 * 静默改写——价格算错要比显示空更值得看见。
 */
function roundDecimals(value: number): number {
  if (!Number.isFinite(value)) return value;
  return Math.round(value * ROUND_SCALE) / ROUND_SCALE;
}

/**
 * 存储值（每 1K token）→ 界面值（每百万 Token）。
 *
 * 例：`0.0003 → 0.3`、`0.0000025 → 0.0025`、`0.01 → 10`。
 */
export function toPerMillion(storageValue: number): number {
  if (!Number.isFinite(storageValue)) return storageValue;
  return roundDecimals(storageValue * PER_MILLION_SCALE);
}

/**
 * 界面值（每百万 Token）→ 存储值（每 1K token）。
 *
 * 例：`0.3 → 0.0003`、`0.0025 → 0.0000025`、`10 → 0.01`。
 * 这是写回后端的唯一出口，写回的仍是「每 1K」，后端计费公式一字不动。
 */
export function fromPerMillion(uiValue: number): number {
  if (!Number.isFinite(uiValue)) return uiValue;
  return roundDecimals(uiValue / PER_MILLION_SCALE);
}

/**
 * 展示用格式：去掉浮点尾巴，**只在有小数位时才裁零**。
 *
 * 对齐 `src/purge/fallback_plan.rs::trim_num` 的口径：`1000` 不能被裁成 `1`
 * （否则「每百万 1000 元」会显示成「每百万 1 元」），而 `1.50` 应显示 `1.5`。
 */
export function formatPriceNumber(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return '—';
  if (Math.abs(value) >= 1e21) return String(value); // toFixed 对超大值会抛 RangeError
  const rounded = roundDecimals(value);
  const text = rounded.toFixed(12);
  const dot = text.indexOf('.');
  const frac = text.slice(dot + 1).replace(/0+$/, '');
  return frac ? `${text.slice(0, dot)}.${frac}` : text.slice(0, dot);
}

/**
 * 后端存储值 → 界面展示串（每百万 Token）。
 *
 * `0.0000025 → "0.0025"`（不带浮点尾巴），`0 → "0"`，`undefined → "—"`。
 */
export function formatPerMillion(storageValue: number | null | undefined): string {
  if (storageValue == null) return '—';
  return formatPriceNumber(toPerMillion(storageValue));
}

/**
 * 界面输入串 → 后端存储值。空串按 0（与既有编辑器「留空即 0」的口径一致）。
 */
export function parsePerMillionInput(raw: string): number {
  const text = raw.trim();
  if (text === '') return 0;
  const n = Number(text);
  return Number.isFinite(n) ? fromPerMillion(n) : NaN;
}

/**
 * 官方价折扣：`折 = 我们价 / 官方价 × 10`（中文惯例：0.1 → 1 折、0.25 → 2.5 折）。
 *
 * 无官方价、官方价非正数、或我们价缺失时返回 `null` —— 界面据此显示「未登记」，
 * 绝不把「没填」显示成「1 折」这种凭空折扣。
 */
export function discountRatio(
  ourPrice: number | null | undefined,
  officialPrice: number | null | undefined
): number | null {
  if (ourPrice == null || officialPrice == null) return null;
  const our = Number(ourPrice);
  const official = Number(officialPrice);
  if (!Number.isFinite(our) || !Number.isFinite(official) || official <= 0) return null;
  if (our < 0) return null;
  return our / official;
}

/**
 * 折扣比率 → 中文「折」表述，保留一位小数。
 *
 * `0.1 → "1 折"`、`0.25 → "2.5 折"`、`0.05 → "0.5 折"`。
 * 比率 ≥ 1（不低于官方价）由调用方判，这里只负责表述。
 */
export function formatDiscount(ratio: number): string {
  const zhe = Math.round(ratio * 10 * 10) / 10;
  return `${formatPriceNumber(zhe)} 折`;
}
