/**
 * 对话调试器的「按消息下标存的时间戳 / 响应耗时」重映射（纯函数，可单测）。
 *
 * 修的原缺陷：`ChatDebugger.tsx` 用 `Map<number, number>` 以**数组下标**为键
 * 存每条消息的时间戳与响应耗时，删除某条消息时只 `delete(idx)`：
 *
 *   [m0][m1][m2]   删除 m1 → [m0][m2]
 *    keys 0,1,2                keys 0,2   ← m2 的下标变成 1，但它记的耗时还挂在 2 上
 *
 * 结果是删除后「响应耗时」整体错位一格（后一条显示前一条的耗时，最后一条
 * 直接丢标记）。同一个坑在「编辑重发 / 重新生成」里不出现——那两条是
 * `slice(0, k)` 截尾，前面的下标不变。
 *
 * 这里把「删除后重排」收敛成单点：调用方删消息时同步调 `removeMetaAt`。
 */
export type IndexKeyedMeta = Map<number, number>;

/**
 * 删除下标 `idx` 的元数据，并把其后所有条目左移一位。
 *
 * 返回**新 Map**（不修改入参，避免 React 复用旧引用导致不重渲染）。
 */
export function removeMetaAt(meta: IndexKeyedMeta, idx: number): IndexKeyedMeta {
  const next: IndexKeyedMeta = new Map();
  for (const [k, v] of meta) {
    if (k === idx) continue;
    next.set(k > idx ? k - 1 : k, v);
  }
  return next;
}

/**
 * 截断到前 `len` 条消息：丢弃下标 >= len 的元数据，前面的保持不变。
 *
 * `slice(0, len)` 场景（编辑重发 / 重新生成）用它清理尾部残留，避免下一次
 * 追加消息时撞上上一轮遗留的旧条目（`Map.set` 覆盖虽无害，但会让「重新生成
 * 后耗时」继承上一轮的测量值，看起来像新请求的耗时）。
 */
export function truncateMeta(meta: IndexKeyedMeta, len: number): IndexKeyedMeta {
  const next: IndexKeyedMeta = new Map();
  for (const [k, v] of meta) {
    if (k < len) next.set(k, v);
  }
  return next;
}

/** 整体清空（清空对话 / 宿主切换会话）——返回空 Map，语义显式。 */
export function clearMeta(): IndexKeyedMeta {
  return new Map();
}
