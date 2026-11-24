/**
 * 成员分区（对齐 Agent2API v3.5.1 `routing-account-sections.ts`）。
 *
 * A2A 按「该提供商有没有账号 / 账号是否全禁用 / 是否参与」把成员分成
 * 参与区 + 三个折叠区（未参与本路由 / 账号全部禁用 / 还没有账号）。
 * AIGX 侧没有账号目录可查（`RoutingMember.accounts` 只是账号 id 列表，
 * 没有启用态），故 `routingAccountSections` 保留完整判据（传 accounts
 * 即按 A2A 原样分），`accounts` 缺失时按「是否参与」退化 —— 只分
 * 参与 / 未参与两区，绝不猜「账号全部禁用」。
 */

export type RoutingCollapsedKey = 'excluded' | 'disabled' | 'empty';
export type RoutingSectionKey = 'active' | RoutingCollapsedKey;

/** 折叠区顺序与文案（A2A 同名同序；文案在渲染期取 t，避免模块级冻结语言） */
export const routingCollapsedSectionKeys: Array<{ key: RoutingCollapsedKey; label: string }> = [
  { key: 'excluded', label: '未参与本路由' },
  { key: 'disabled', label: '账号全部禁用' },
  { key: 'empty', label: '还没有账号' },
];

export interface RoutingAccountLike {
  id: string;
  provider?: string;
  enabled?: boolean;
}

/**
 * 分组只改变展示位置，保留每组内部的原始顺序，不改写路由优先级。
 *
 * `accounts` 为空数组 = 没有账号目录数据：退化成 active/excluded 两区，
 * disabled/empty 恒空（界面因此不会显示「账号全部禁用」这种没有依据的区）。
 */
export function routingAccountSections<T>(
  items: T[],
  accounts: RoutingAccountLike[],
  provider: (item: T) => string,
  included: (item: T) => boolean,
): Record<RoutingSectionKey, T[]> {
  const counts = new Map<string, { total: number; enabled: number }>();
  for (const account of accounts) {
    const id = account.provider ?? '';
    const count = counts.get(id) || { total: 0, enabled: 0 };
    count.total += 1;
    if (account.enabled !== false) count.enabled += 1;
    counts.set(id, count);
  }
  const sections: Record<RoutingSectionKey, T[]> = { active: [], excluded: [], disabled: [], empty: [] };
  for (const item of items) {
    const count = accounts.length ? counts.get(provider(item)) : undefined;
    const section: RoutingSectionKey = count && !count.total
      ? 'empty'
      : count && !count.enabled
        ? 'disabled'
        : included(item)
          ? 'active'
          : 'excluded';
    sections[section].push(item);
  }
  return sections;
}