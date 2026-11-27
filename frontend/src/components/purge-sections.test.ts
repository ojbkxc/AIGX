// @vitest-environment node
/**
 * 破甲控制台分组层的**结构判据**（纯逻辑，无 React 渲染）。
 *
 * 这一组测试存在的理由：分组是个「看起来对了、实际漏一个」的最容易出错的地方。
 * 具体三种失败形态，都在这里钉住：
 * 1. **锚点漏登记**：`data-purge-fix="X"` 在页面里存在、`ANCHOR_GROUP` 里没有 →
 *    点「怎么修」跳过去，但那一组还收着，按钮看起来是坏的（实测踩过）。
 * 2. **字段漏分组**：某个配置字段既不在任何组里、又没有独立入口 → 等于删了它。
 *    用户原话「不要为了看起来清爽把功能藏到找不到的地方」。
 * 3. **总开关关闭后仍可编辑**：老版本关掉总开关照样能改，用户以为配好了。
 *    现在四组强制收起 + 灰化，判据是 `isGroupOpen`。
 *
 * 读源码做判据是本仓既有的做法（`PurgeConsole.test.ts` 也读 `src/purge/mod.rs`）。
 */
import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import type { PurgeSettings } from '../types';
import {
  FORCED_COLLAPSE_WHEN_OFF,
  PURGE_GROUPS,
  PURGE_GROUPS_KEY,
  baseStatus,
  defaultCollapsed,
  groupForAnchor,
  groupItemCount,
  isGroupOpen,
  readCollapsedGroups,
  writeCollapsedGroups,
} from './purge-sections';

const root = path.resolve(__dirname, '..');
const pageSource = fs.readFileSync(path.join(root, 'pages', 'PurgeConsole.tsx'), 'utf8');
const uiSource = fs.readFileSync(path.join(root, 'components', 'purge-groups-ui.tsx'), 'utf8');

/** 页面里出现的所有 `data-purge-fix="..."` 字面量（含 `focusFixTarget('x')` 的调用实参） */
function anchorsInSource(source: string): string[] {
  const out = new Set<string>();
  for (const m of source.matchAll(/data-purge-fix="([a-z0-9-]+)"/g)) out.add(m[1]);
  for (const m of source.matchAll(/focusFixTarget\('([a-z0-9-]+)'\)/g)) out.add(m[1]);
  // 动态拼接的模板串（`data-testid={`purge-configured-off-${...}`}` 之类）不算锚点
  return [...out].sort();
}

const settings = (patch: Partial<PurgeSettings> = {}): PurgeSettings =>
  ({
    enabled: true,
    system_normalize: false,
    dead_zone_rewrite: false,
    synonym_enabled: false,
    reduce_reasoning: false,
    cross_source_fallback: false,
    skill_inject: false,
    structured_prompt: false,
    reasoning_floor: 'low',
    retry_limit: 3,
    fallback_trigger_attempt: 2,
    fallback_models: [],
    rewrite_rules: [],
    synonym_pairs: [],
    normalize_strip_prefixes: [],
    skill_dir: '',
    skill_inject_limit: 2000,
    structured_suffix: '',
    structured_templates: [],
    active_template: '',
    refusal_patterns: [],
    failure_filter_keywords: [],
    failure_empty_keywords: [],
    auto_preset: false,
    auto_preset_id: '',
    universal_fallback: false,
    upstream_label: '',
    ...patch,
  }) as PurgeSettings;

describe('ANCHOR_GROUP 覆盖页面里所有锚点', () => {
  it('每个 data-purge-fix（含 focusFixTarget 实参）都能解析出组', () => {
    const anchors = anchorsInSource(pageSource);
    // 正向对照下限：扫不到就不可能是「全覆盖」
    expect(anchors.length).toBeGreaterThanOrEqual(12);
    const missing = anchors.filter((anchor) => groupForAnchor(anchor) === null);
    expect(missing, `未登记到 ANCHOR_GROUP 的锚点：${missing.join(', ')}`).toEqual([]);
  });

  it('锚点必须落在活跃组里（不是只存在于 DOM 的幽灵组）', () => {
    const ids = new Set(PURGE_GROUPS.map((group) => group.id));
    for (const anchor of anchorsInSource(pageSource)) {
      const group = groupForAnchor(anchor);
      if (group) expect(ids.has(group)).toBe(true);
    }
  });
});

describe('分组元数据完整性', () => {
  it('组 id 唯一，且总开关关闭时强制收起的组都在表里', () => {
    const ids = PURGE_GROUPS.map((g) => g.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of FORCED_COLLAPSE_WHEN_OFF) expect(ids).toContain(id);
  });

  it('预设组不跟随总开关强制收起（它是修配置的入口，不是治理行为）', () => {
    // 关掉总开关后仍要能改「按模型自动套用」——那是「先配好、最后开总开关」的
    // 常见流程，强制收起会让它变成「先开总开关才能配预设」。
    expect(FORCED_COLLAPSE_WHEN_OFF).not.toContain('preset');
    expect(isGroupOpen('preset', [], false)).toBe(true);
  });

  it('四个治理组在总开关关闭时强制收起', () => {
    for (const id of ['governance', 'resilience', 'skills', 'advanced'] as const) {
      expect(isGroupOpen(id, [], false)).toBe(false);
      // 重新打开总开关 → 恢复展开（强制收起是派生状态，不该写进持久化）
      expect(isGroupOpen(id, [], true)).toBe(true);
    }
  });

  it('默认收起集合只来自 defaultOpen=false 的组', () => {
    const expected = PURGE_GROUPS.filter((g) => !g.defaultOpen).map((g) => g.id);
    expect(defaultCollapsed().sort()).toEqual(expected.sort());
  });
});

describe('readCollapsedGroups / writeCollapsedGroups', () => {
  const makeStore = (initial: Record<string, string> = {}) => {
    const store = new Map(Object.entries(initial));
    return {
      store,
      get: (k: string) => store.get(k) ?? null,
      set: (k: string, v: string) => void store.set(k, v),
      remove: (k: string) => void store.delete(k),
    };
  };

  it('无记录 → 默认集合', () => {
    const s = makeStore();
    expect(readCollapsedGroups(s.get).sort()).toEqual(defaultCollapsed().sort());
  });

  it('坏 JSON / 非数组 → 默认集合（不拿脏值去渲染）', () => {
    expect(readCollapsedGroups(makeStore({ [PURGE_GROUPS_KEY]: '{oops' }).get)).toEqual(
      defaultCollapsed(),
    );
    expect(readCollapsedGroups(makeStore({ [PURGE_GROUPS_KEY]: '{"a":1}' }).get)).toEqual(
      defaultCollapsed(),
    );
  });

  it('未知组 id 与非字符串元素被丢弃（组改名后旧键不会复活到新组上）', () => {
    const raw = JSON.stringify(['governance', 'no-such-group', 42, null, 'skills']);
    expect(readCollapsedGroups(makeStore({ [PURGE_GROUPS_KEY]: raw }).get).sort()).toEqual([
      'governance',
      'skills',
    ]);
  });

  it('空集合 → 移除键（不留 `[]` 垃圾）', () => {
    const s = makeStore();
    writeCollapsedGroups([], s.set, s.remove);
    expect(s.store.has(PURGE_GROUPS_KEY)).toBe(false);
    writeCollapsedGroups(['base', 'base'], s.set, s.remove);
    expect(JSON.parse(s.store.get(PURGE_GROUPS_KEY)!)).toEqual(['base']);
  });

  it('写失败静默（localStorage 满/禁用不能打断配配置）', () => {
    const boom = () => {
      throw new Error('quota');
    };
    expect(() => writeCollapsedGroups(['base'], boom, boom)).not.toThrow();
  });
});

describe('groupItemCount 按数据面语义数', () => {
  it('总开关关闭时所有治理组都是 0（不是数有没有填）', () => {
    const off = settings({
      enabled: false,
      system_normalize: true,
      dead_zone_rewrite: true,
      rewrite_rules: [{ from: 'a', to: 'b' }],
      cross_source_fallback: true,
      fallback_models: ['m'],
      skill_inject: true,
      auto_preset: true,
      universal_fallback: true,
      structured_prompt: true,
      reduce_reasoning: true,
    });
    for (const id of ['governance', 'resilience', 'skills', 'advanced', 'preset'] as const) {
      expect(groupItemCount(id, off), `${id} 在关闭态应为 0`).toBe(0);
    }
    expect(groupItemCount('base', off)).toBe(0);
  });

  it('开关开着但词表全部停用 → 不算一项（这是「看起来配了」的典型）', () => {
    const s = settings({
      dead_zone_rewrite: true,
      rewrite_rules: [
        { from: 'a', to: 'b', enabled: false },
        { from: 'c', to: 'd', enabled: false },
      ],
    });
    expect(groupItemCount('governance', s)).toBe(0);
    const allOn = settings({ dead_zone_rewrite: true, rewrite_rules: [{ from: 'a', to: 'b' }] });
    expect(groupItemCount('governance', allOn)).toBe(1);
  });

  it('空原文行不算启用（后端 normalize 会静默丢弃）', () => {
    const s = settings({ synonym_enabled: true, synonym_pairs: [{ from: '  ', to: 'x' }] });
    expect(groupItemCount('governance', s)).toBe(0);
  });

  it('预设组：自动套用 + 通用兜底 + 固定 id 各算一项', () => {
    expect(groupItemCount('preset', settings({ auto_preset: true }))).toBe(1);
    expect(
      groupItemCount('preset', settings({ auto_preset: true, universal_fallback: true })),
    ).toBe(2);
    expect(
      groupItemCount(
        'preset',
        settings({ auto_preset: true, universal_fallback: true, auto_preset_id: 'x' }),
      ),
    ).toBe(3);
  });

  it('容错组：开着但候选为空只算重试上限一项', () => {
    expect(
      groupItemCount('resilience', settings({ cross_source_fallback: true, fallback_models: [] })),
    ).toBe(1);
    expect(
      groupItemCount(
        'resilience',
        settings({ cross_source_fallback: true, fallback_models: ['a', 'b'] }),
      ),
    ).toBe(2);
  });

  it('高级组：只数真的填了内容的项', () => {
    expect(groupItemCount('advanced', settings())).toBe(0);
    expect(
      groupItemCount(
        'advanced',
        settings({
          reduce_reasoning: true,
          refusal_patterns: ['p'],
          structured_suffix: 'x',
        }),
      ),
    ).toBe(3);
    // 只有空白后缀 = 没填（后端 trim 后为空）
    expect(groupItemCount('advanced', settings({ structured_suffix: '   ' }))).toBe(0);
  });
});

describe('baseStatus：总开关旁的一行「现在到底在不在治理」', () => {
  it('关闭态只报一件事：原样透传', () => {
    const s = baseStatus(settings({ enabled: false, system_normalize: true }));
    expect(s.tone).toBe('off');
    expect(s.chips).toHaveLength(1);
    expect(s.chips[0].key).toContain('原样透传');
  });

  it('开着但没有任何改写项 → partial（配了不等于在治理）', () => {
    const s = baseStatus(settings({ retry_limit: 1 }));
    expect(s.tone).toBe('partial');
    expect(s.chips.some((c) => c.key === '无改写')).toBe(true);
    expect(s.chips.some((c) => c.key === '不重试')).toBe(true);
  });

  it('开着且有改写项 → on，且逐项各一枚 chip（不拼句子：英文界面会露出中文）', () => {
    const s = baseStatus(
      settings({
        system_normalize: true,
        dead_zone_rewrite: true,
        rewrite_rules: [{ from: 'a', to: 'b' }],
        retry_limit: 5,
        auto_preset: true,
      }),
    );
    expect(s.tone).toBe('on');
    const keys = s.chips.map((c) => c.key);
    expect(keys[0]).toBe('改写中');
    expect(keys).toContain('归一化');
    expect(keys).toContain('死区词');
    expect(keys).toContain('预设按模型匹配');
    // 每个 chip 的 key 都是独立 i18n key（不含拼出来的中文句子）
    for (const chip of s.chips) expect(chip.key).not.toMatch(/·/);
  });

  it('固定预设 id → 单独一枚带 id 的 chip', () => {
    const s = baseStatus(settings({ auto_preset: true, auto_preset_id: 'opus-v2' }));
    expect(s.chips.some((c) => c.key === '预设固定「{{id}}」' && c.params?.id === 'opus-v2')).toBe(
      true,
    );
  });
});

describe('渲染层：折叠必须真的收起（不是只改 aria）', () => {
  it('组体同时由 `hidden` 与 CSS 兜住', () => {
    // `hidden` 属性是语义/无障碍面；CSS 的 `[hidden]{display:none}` 才是视觉面 ——
    // 少了后者，作者样式里的 `display:flex` 会盖掉 UA 的 `display:none`，
    // 收起变成「aria-expanded=false 但照样占满屏」（实测踩过：scrollHeight 一格没变）。
    const css = fs.readFileSync(path.join(root, 'components', 'purge-groups-ui.css'), 'utf8');
    expect(css).toMatch(/\.purge-group-body\[hidden\]/);
    expect(uiSource).toMatch(/hidden=\{!open\}/);
    expect(uiSource).toMatch(/aria-expanded=\{open\}/);
  });

  it('跳转前展开必须改状态（只改 DOM 会被下一轮渲染写回）', () => {
    // 判据：页面用的 `requestOpenGroup` 在状态层注册了回调，
    // 而不是只 `body.hidden = false` 了事。
    expect(uiSource).toMatch(/registerOpenGroupHandler/);
    expect(uiSource).toMatch(/openGroupHandler\?\.\(id\)/);
    expect(pageSource).toMatch(/registerOpenGroupHandler\(/);
  });

  it('每组折叠头有「全部展开 / 全部收起」（折叠收益必须能一键撤回）', () => {
    expect(uiSource).toMatch(/data-testid="purge-groups-expand-all"/);
    expect(uiSource).toMatch(/data-testid="purge-groups-collapse-all"/);
  });

  it('组头不挂 data-purge-fix（否则锚点会先命中组头，跳到组而不是字段）', () => {
    expect(uiSource).not.toMatch(/data-purge-fix=/);
  });
});