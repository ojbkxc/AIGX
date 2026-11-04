/**
 * AIGX · 页面筛选条件的跨次启动记忆（日志 / 请求 / 模型 / 渠道四页共用）。
 *
 * 每个页面一个自己的 localStorage 键，值是「维度 → 取值」的扁平 JSON 对象
 * （如 `{state:'on', sort:'priority'}`）。空串是合法值——下拉类筛选用空串表示
 * 「全部」。各页自己决定哪些维度值得记（描述「此刻在盯什么」的临时开关不记）。
 *
 * 为什么收成一份工具：各页行为完全一致（启动恢复、变更落盘、存坏回落默认），
 * 逐页各写一份迟早漂成四种行为。这里只管存取，UI 的回填（分段按钮的 active、
 * 下拉的 value）仍归各页——控件形态不同，收上来只会多出一堆配置项。
 *
 * 对齐 Agent2API desktop-tauri/ui/filter-memory.js（load / save 同一口径）。
 */

/**
 * 读一份筛选条件。按 defaults 的键收窄：非字符串的值（存坏 / 手改坏）一律
 * 落回该键的默认值——记忆是锦上添花，不能让首屏拿到半份数据或抛错。
 * 不给 defaults 时返回存盘原值（不存在 / 存坏返回 null）。
 */
export function getFilter<T extends object>(key: string, defaults: T): T;
export function getFilter<T>(key: string): T | null;
export function getFilter<T extends object>(key: string, defaults?: T): T | null {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return defaults !== undefined ? { ...defaults } : null;
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return defaults !== undefined ? { ...defaults } : null;
    }
    if (defaults === undefined) return parsed as T;
    const out: Record<string, unknown> = {};
    for (const [name, value] of Object.entries(defaults)) out[name] = value;
    for (const name of Object.keys(defaults)) {
      const stored = (parsed as Record<string, unknown>)[name];
      if (typeof stored === 'string') out[name] = stored;
    }
    return out as T;
  } catch {
    // 解析失败按全默认算
    return defaults !== undefined ? { ...defaults } : null;
  }
}

/** 合并写回：patch 里的键覆盖存量，其余维度保留（各页通常一次只改一个维度） */
export function setFilter(key: string, patch: object): void {
  try {
    let current: Record<string, unknown> = {};
    try {
      const parsed: unknown = JSON.parse(localStorage.getItem(key) || '{}');
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        current = parsed as Record<string, unknown>;
      }
    } catch {
      // 存量存坏：忽略，以 patch 为准
    }
    localStorage.setItem(key, JSON.stringify({ ...current, ...patch }));
  } catch {
    // 存储不可用只影响下次打开，本次会话照常
  }
}

/** 清空一份筛选条件的记忆（遗忘该页的筛选状态） */
export function clearFilter(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    // 存储不可用时静默
  }
}
