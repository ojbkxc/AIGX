/**
 * 模型排序口径（对齐 Agent2API v3.5.1 `routing-model-order.ts`，逐行同义）。
 *
 * 排序键：最近使用时间倒序 → 模型 id 字典序（大小写不敏感、去首尾空白）。
 * 只在**展示**层用（picker / 新建路由弹窗）：同一批候选每次渲染顺序稳定，
 * 后台刷新不会挪走用户正准备点击的那一行。
 */
export function sortRoutingModels<T>(
  models: readonly T[],
  modelId: (model: T) => string,
  lastUsed?: Record<string, number>,
): T[] {
  const usage = new Map<string, number>();
  for (const [model, at] of Object.entries(lastUsed || {})) {
    if (!Number.isFinite(at) || at <= 0) continue;
    const key = model.trim().toLowerCase();
    usage.set(key, Math.max(usage.get(key) || 0, at));
  }
  return models
    .map((model) => {
      const key = modelId(model).trim().toLowerCase();
      return { model, key, at: usage.get(key) || 0 };
    })
    .sort(
      (left, right) =>
        right.at - left.at || (left.key < right.key ? -1 : left.key > right.key ? 1 : 0),
    )
    .map((item) => item.model);
}