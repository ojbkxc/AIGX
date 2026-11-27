/**
 * 破甲控制台的**分组元数据**（纯数据 + 纯函数，不含 JSX）。
 *
 * 为什么要这张表：破甲页有 10 个开关 + 两张词表 + 模板 / 预设 / 技能四类配置，
 * 平铺在 5.6k 行里，用户进来看到的是「几十个字段」，找不到「我该先配什么」。
 * 分组本身不改变数据面行为，只改变「先看哪一组、哪一组当前在生效」。
 *
 * 三条纪律（改这张表前先读）：
 * 1. **不收窄能力**：分组只决定「折叠 / 展开」，任何字段都必须仍然可达——
 *    折叠起来也算可达，但**不能**有字段既不在组里、也没有独立入口（漏一个就等于删了它）。
 * 2. **默认展开**：默认只收起 `advanced`。其余组默认展开，因为「进来就能改最常改的」
 *    比「先点两下才看到」更重要；折叠收益最大的组（高级）恰恰是最少改的那一组。
 * 3. **总开关关闭时**只强制收起「治理 / 容错 / 技能 / 高级」四组：这四组关掉后
 *    100% 不生效（用户以为配好了）。`preset` 组刻意**不**跟随强制收起——
 *    它是修配置的入口（「一句话搞定」的核心），而且预设库的浏览 / 应用在总开关
 *    关闭时依然是有效的准备工作（应用后打开总开关即可生效）。
 */
import type { PurgeSettings } from '../types';

/** 分组 id（同时也是 localStorage 里的折叠标记）。 */
export type PurgeGroupId =
  | 'stats'
  | 'summary'
  | 'base'
  | 'preset'
  | 'governance'
  | 'resilience'
  | 'skills'
  | 'advanced'
  | 'preview';

export interface PurgeGroupMeta {
  id: PurgeGroupId;
  /** i18n key（= 中文原文） */
  label: string;
  /** 一句话：这一组是干什么的（折叠时也显示，避免「不知道这组里有什么」） */
  hint: string;
  /** 页内导航锚点（与 `data-purge-fix` 同名；null = 不参与页内导航） */
  anchor: string | null;
  /** 总开关关闭时是否强制收起（见文件头第 3 条） */
  forceCollapseWhenOff: boolean;
  /** 默认是否展开 */
  defaultOpen: boolean;
}

/**
 * 组顺序 = 页面从上到下的顺序（页内导航与「全部展开」都按这个顺序读）。
 *
 * 排序理由（使用频率 + 依赖关系）：
 * - `stats` / `summary`：只读事实，进页面第一眼要看的是「现在在发生什么」；
 * - `base`：总开关。它是所有其它开关的前提，必须第一屏就在；
 * - `preset`：**优先级最高的可配项**——「一句话搞定」的入口（按模型套预设
 *   + 通用兜底）。用户问「怎么最快生效」，答案在这一组；
 * - `governance`：请求治理的数据面（两张词表 + 结构化模板）；
 * - `resilience`：容错（异源递进 + 重试）。放在治理之后、技能之前：
 *   想调它的人一定是先看过统计里的重试率才来的；
 * - `skills`：技能注入（目录 / 注入上限 / 技能包导入 / 新建技能 / 扫描快照）；
 * - `advanced`：其余低频项（拒绝话术 / 失败关键词 / 结构化后缀 / 只读参数
 *   / 配置导入导出）；
 * - `preview`：dry-run 试跑。
 *
 * ── 为什么默认**全部展开** ─────────────────────────────────
 * 用户原话「不要为了看起来清爽把功能藏到找不到的地方」是硬约束，而默认收起
 * 恰恰就是把功能藏起来：新用户不知道哪个组里有他要的字段，只能挨个点开。
 * 所以默认状态与改版前保持等价（所有字段一眼可见），折叠是**用户自己的选择**——
 * 收起后按组落盘（`PURGE_GROUPS_KEY`），下次进来仍是他收的样子。
 *
 * 真正降低的是「找不到」而不是「有多少」：组头（9 个）+ 每组「N 项已启用」+
 * 页内导航直接指向组 + 吸底保存条 + 体检面板的「怎么修」。默认高度因此与
 * 改版前同级（略增，来自组头），而**用户收起到 4 个关键组后**高度砍掉一半
 * ——两个数都在交付说明里，不含「默认就变矮」的假话。
 */
export const PURGE_GROUPS: readonly PurgeGroupMeta[] = [
  {
    id: 'stats',
    label: '统计',
    hint: '命中与失败计数（只读）',
    anchor: 'stats',
    forceCollapseWhenOff: false,
    defaultOpen: true,
  },
  {
    id: 'summary',
    label: '当前生效项',
    hint: '按数据面语义汇总：现在到底哪些治理真的在跑',
    anchor: 'active-summary',
    forceCollapseWhenOff: false,
    defaultOpen: true,
  },
  {
    id: 'base',
    label: '基础',
    hint: '总开关与作用域',
    anchor: 'switches',
    forceCollapseWhenOff: false,
    defaultOpen: true,
  },
  {
    id: 'preset',
    label: '预设库',
    hint: '按模型自动套用 + 通用兜底（一句话搞定）',
    anchor: 'presets',
    forceCollapseWhenOff: false,
    defaultOpen: true,
  },
  {
    id: 'governance',
    label: '请求治理',
    hint: '归一化 / 死区词改写 / 同义词 / 结构化提示',
    anchor: 'dead-zone-table',
    forceCollapseWhenOff: true,
    defaultOpen: true,
  },
  {
    id: 'resilience',
    label: '容错',
    hint: '异源递进与重试',
    anchor: 'fallback-models',
    forceCollapseWhenOff: true,
    defaultOpen: true,
  },
  {
    id: 'skills',
    label: '技能注入',
    hint: '技能目录 / 关键词命中 / 技能包导入',
    anchor: 'skill-dir',
    forceCollapseWhenOff: true,
    defaultOpen: true,
  },
  {
    id: 'advanced',
    label: '高级',
    hint: '拒绝话术 / 失败关键词 / 结构化后缀 / 只读参数',
    anchor: 'advanced',
    forceCollapseWhenOff: true,
    defaultOpen: true,
  },
  {
    id: 'preview',
    label: '改写预览',
    hint: 'dry-run 试跑：不发送请求、不写统计',
    anchor: 'preview',
    forceCollapseWhenOff: false,
    defaultOpen: true,
  },
];

/** 折叠状态的 localStorage 键（存的是**收起**的组，这样新增组默认展开）。 */
export const PURGE_GROUPS_KEY = 'aigx.purge.groups.collapsed';

/** 总开关关闭时强制收起的组（`preset` 刻意不在其中，见文件头）。 */
export const FORCED_COLLAPSE_WHEN_OFF: readonly PurgeGroupId[] = PURGE_GROUPS
  .filter((group) => group.forceCollapseWhenOff)
  .map((group) => group.id);

/**
 * 锚点 → 组（`focusFixTarget` 跳转前必须先展开目标组，否则跳到折叠区等于跳到空处）。
 *
 * **每个 `data-purge-fix` 值都必须在这里登记**：漏一条的后果不是报错，而是
 * 「点『怎么修』跳过去了，但那一组还是收起的」——看起来像按钮点了没用。
 * 判据由 `purge-sections.test.ts` 的「锚点全覆盖」一条钉住（它直接扫页面源码里
 * 的 `data-purge-fix="..."` 字面量）。
 */
const ANCHOR_GROUP: Record<string, PurgeGroupId> = {
  stats: 'stats',
  'active-summary': 'summary',
  switches: 'base',
  presets: 'preset',
  'auto-preset-id': 'preset',
  'dead-zone-table': 'governance',
  'synonym-table': 'governance',
  templates: 'governance',
  'fallback-models': 'resilience',
  'fallback-attempts': 'resilience',
  'skill-dir': 'skills',
  'skill-snapshot': 'skills',
  'skill-import': 'skills',
  preview: 'preview',
  advanced: 'advanced',
};

export function groupForAnchor(anchor: string): PurgeGroupId | null {
  return ANCHOR_GROUP[anchor] ?? null;
}

/** 默认折叠集合（= 未持久化时的那一份）。 */
export function defaultCollapsed(): PurgeGroupId[] {
  return PURGE_GROUPS.filter((group) => !group.defaultOpen).map((group) => group.id);
}

/**
 * 读回落盘的分组状态。
 *
 * 容错口径与 `readPresetFilter` 一致：坏 JSON / 非字符串元素 / 未知组 id 一律忽略，
 * 未知 id 不报错也不保留（组改名后旧键只会越积越多，且折叠状态会「复活」到新组上）。
 */
export function readCollapsedGroups(get: (key: string) => string | null): PurgeGroupId[] {
  const raw = get(PURGE_GROUPS_KEY);
  if (!raw) return defaultCollapsed();
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return defaultCollapsed();
  }
  if (!Array.isArray(parsed)) return defaultCollapsed();
  const known = new Set<string>(PURGE_GROUPS.map((group) => group.id));
  const ids = parsed.filter(
    (item): item is PurgeGroupId => typeof item === 'string' && known.has(item),
  );
  return [...new Set(ids)];
}

export function writeCollapsedGroups(
  ids: readonly PurgeGroupId[],
  set: (key: string, value: string) => void,
  remove: (key: string) => void,
): void {
  try {
    const uniq = [...new Set(ids)];
    if (uniq.length === 0) {
      remove(PURGE_GROUPS_KEY);
      return;
    }
    set(PURGE_GROUPS_KEY, JSON.stringify(uniq));
  } catch {
    // localStorage 满 / 禁用：静默。折叠状态是便利，不能反过来打断配配置
  }
}

/** 某组是否展开（受总开关强制收起影响）。 */
export function isGroupOpen(
  id: PurgeGroupId,
  collapsed: readonly PurgeGroupId[],
  enabled: boolean,
): boolean {
  if (!enabled && FORCED_COLLAPSE_WHEN_OFF.includes(id)) return false;
  return !collapsed.includes(id);
}

/**
 * 「N 项已启用」：按**数据面真实语义**数，不是数字段个数。
 *
 * 为什么按语义数：`dead_zone_rewrite` 开着但词表全部停用，实际是 0 生效；反过来
 * 词表有 3 条启用行但开关关着，也是 0。照着字段有没有填来数，会让折叠头显示
 * 「8 项已启用」而实际一条都不跑——这正是这次改版要消灭的错觉。
 */
export function groupItemCount(
  id: PurgeGroupId,
  settings: PurgeSettings,
  extra: { skills?: number } = {},
): number {
  const activeRules = (rules: PurgeSettings['rewrite_rules'] | undefined): number =>
    (rules ?? []).filter((rule) => rule.enabled !== false && rule.from.trim() !== '').length;

  switch (id) {
    case 'base':
      return settings.enabled ? 1 : 0;
    case 'preset': {
      let n = 0;
      if (settings.auto_preset) n += 1;
      if (settings.universal_fallback) n += 1;
      if ((settings.auto_preset_id ?? '').trim()) n += 1;
      return settings.enabled ? n : 0;
    }
    case 'governance': {
      if (!settings.enabled) return 0;
      let n = 0;
      if (settings.system_normalize) n += 1;
      if (settings.dead_zone_rewrite) n += activeRules(settings.rewrite_rules) > 0 ? 1 : 0;
      if (settings.synonym_enabled) n += activeRules(settings.synonym_pairs) > 0 ? 1 : 0;
      if (settings.structured_prompt) n += 1;
      if ((settings.normalize_strip_prefixes ?? []).length > 0) n += 1;
      return n;
    }
    case 'resilience': {
      if (!settings.enabled) return 0;
      let n = 0;
      if (settings.cross_source_fallback) {
        n += (settings.fallback_models ?? []).length > 0 ? 1 : 0;
      }
      // 重试上限是「有效值」就算：它永远有值，只要总开关开着就参与
      n += 1;
      return n;
    }
    case 'skills': {
      if (!settings.enabled) return 0;
      let n = 0;
      if (settings.skill_inject) n += 1;
      if ((settings.skill_dir ?? '').trim()) n += 1;
      if ((extra.skills ?? 0) > 0) n += 1;
      return n;
    }
    case 'advanced': {
      if (!settings.enabled) return 0;
      let n = 0;
      if (settings.reduce_reasoning) n += 1;
      if ((settings.refusal_patterns ?? []).length > 0) n += 1;
      if ((settings.failure_filter_keywords ?? []).length > 0) n += 1;
      if ((settings.failure_empty_keywords ?? []).length > 0) n += 1;
      if ((settings.structured_suffix ?? '').trim()) n += 1;
      return n;
    }
    default:
      return 0;
  }
}

/**
 * 一行「当前状态」摘要（基础组顶部；总开关旁边必须能一眼看出「现在到底在不在治理」）。
 *
 * 只报**数据面会发生的三件事**：请求是否被改写、失败会不会重试、预设会不会前插。
 * 不报「有 N 个开关开着」——那是配置自述，不是行为。
 *
 * 返回**键**而不是拼好的中文句子：本页是双语界面，拼死的句子在英文界面里会
 * 突然冒出一段中文（`t()` 只翻译 key，不翻译调用方拼进来的参数）。
 */
export interface StatusChip {
  key: string;
  params?: Record<string, string | number>;
}

export interface BaseStatus {
  tone: 'on' | 'off' | 'partial';
  chips: StatusChip[];
}

export function baseStatus(settings: PurgeSettings): BaseStatus {
  if (!settings.enabled) {
    return { tone: 'off', chips: [{ key: '请求原样透传：不改写、不重试、不套预设' }] };
  }
  const rewrite: StatusChip[] = [];
  if (settings.system_normalize) rewrite.push({ key: '归一化' });
  if (settings.dead_zone_rewrite && (settings.rewrite_rules ?? []).some((r) => r.enabled !== false)) {
    rewrite.push({ key: '死区词' });
  }
  if (settings.synonym_enabled && (settings.synonym_pairs ?? []).some((r) => r.enabled !== false)) {
    rewrite.push({ key: '同义词' });
  }
  if (settings.structured_prompt) rewrite.push({ key: '结构化' });

  const retry: StatusChip =
    settings.retry_limit > 1
      ? { key: '重试 ≤ {{n}} 次', params: { n: settings.retry_limit } }
      : { key: '不重试' };
  const preset: StatusChip = settings.auto_preset
    ? settings.auto_preset_id?.trim()
      ? { key: '预设固定「{{id}}」', params: { id: settings.auto_preset_id.trim() } }
      : { key: '预设按模型匹配' }
    : { key: '不套预设' };

  return {
    tone: rewrite.length > 0 ? 'on' : 'partial',
    // 逐项各一枚 chip（不把几个 key 拼成一个中文句子：英文界面下那条会整段是中文）
    chips: [rewrite.length > 0 ? { key: '改写中' } : { key: '无改写' }, ...rewrite, retry, preset],
  };
}