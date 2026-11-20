/**
 * 兑换倍率（1 元 = ? 额度）的全局口径。
 *
 * 后端在「系统设置 → 计费 → 易支付配置」里以 `epay.price` 维护，并通过
 * `GET /api/public/quota-rate` 公开；前端挂载前拉一次写入这里。所有
 * 「额度 ↔ 元」展示（fmtCost / fmtQuota）都读这一份，避免页面各自写死 200
 * 导致「后台改成 100 后余额显示还按 200 折算」的错误。
 *
 * 默认 200：接口不可用 / 未配置时保持历史口径。
 */
export const DEFAULT_QUOTA_PER_YUAN = 200;

let quotaPerYuan = DEFAULT_QUOTA_PER_YUAN;

/** 当前生效的兑换倍率（1 元 = ? 额度）。 */
export function getQuotaPerYuan(): number {
  return quotaPerYuan;
}

/** 更新兑换倍率；非法值（<=0/NaN/∞）忽略，保持上一次有效值。 */
export function setQuotaPerYuan(value: unknown): void {
  const n = Number(value);
  if (Number.isFinite(n) && n > 0) quotaPerYuan = n;
}
