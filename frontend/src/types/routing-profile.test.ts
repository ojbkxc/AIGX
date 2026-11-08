/**
 * 三态语义回归（#40 契约 §1/§2.1）。
 *
 * 为什么这些纯函数值得单测：整个分层覆盖设计的正确性全压在「继承 / 覆盖 /
 * 清空」三态上，配错了是**静默**的（没有报错、界面看不出），而它只在
 * 纯字段层面成立 —— 不依赖网络与渲染，是本页最该钉死的一层。
 */
import { describe, expect, it } from 'vitest';
import {
  EMPTY_NEEDS,
  EMPTY_SCOPE,
  PROFILE_FIELDS,
  emptyProfile,
  fieldState,
  isProfileEmpty,
  itemsToLines,
  linesToItems,
  mergeProfiles,
  profileOverrideCount,
  setFieldState,
  type RoutingProfile,
} from './routing-profile';

const withScope = (p: Partial<RoutingProfile>): RoutingProfile => ({ ...emptyProfile(), ...p });

describe('三态推导 fieldState', () => {
  it('null / undefined = 继承；有值 = 覆盖', () => {
    const p = emptyProfile();
    for (const f of PROFILE_FIELDS) expect(fieldState(p, f)).toBe('inherit');
  });

  it('枚举字段没有「空」语义：只有 inherit / override', () => {
    expect(fieldState(withScope({ strategy: 'ranked' }), 'strategy')).toBe('override');
    expect(fieldState(withScope({ sticky: 'off' }), 'sticky')).toBe('override');
  });

  it('「有值但为空」= 显式清空（不是覆盖）', () => {
    // 空参与名单 / 空需求 / 空排序数组 / 空组 id 都算清空
    expect(fieldState(withScope({ scope: { ...EMPTY_SCOPE } }), 'scope')).toBe('clear');
    expect(fieldState(withScope({ needs: { ...EMPTY_NEEDS } }), 'needs')).toBe('clear');
    expect(fieldState(withScope({ sort_rules: [] }), 'sort_rules')).toBe('clear');
    expect(fieldState(withScope({ default_group: '   ' }), 'default_group')).toBe('clear');
  });

  it('非空值 = 覆盖', () => {
    expect(
      fieldState(withScope({ scope: { ...EMPTY_SCOPE, provider_scope: 'listed' } }), 'scope'),
    ).toBe('override');
    expect(
      fieldState(withScope({ scope: { ...EMPTY_SCOPE, providers: ['openai'] } }), 'scope'),
    ).toBe('override');
    expect(fieldState(withScope({ needs: { ...EMPTY_NEEDS, tools: true } }), 'needs')).toBe('override');
    expect(fieldState(withScope({ sort_rules: [{ on: 'added', dir: 'asc' }] }), 'sort_rules')).toBe('override');
    expect(fieldState(withScope({ default_group: 'g1' }), 'default_group')).toBe('override');
  });
});

describe('三态切换 setFieldState', () => {
  const lower = withScope({ strategy: 'ranked', sticky: 'session', default_group: 'g1' });

  it('inherit 写回 null（= 不表态，不是写默认值）', () => {
    const p = setFieldState(withScope({ strategy: 'round_robin' }), 'strategy', 'inherit', lower);
    expect(p.strategy).toBeNull();
    expect(fieldState(p, 'strategy')).toBe('inherit');
  });

  it('clear 写入该字段的「空」值，且不等于继承', () => {
    const p = setFieldState(emptyProfile(), 'scope', 'clear', lower);
    expect(p.scope).toEqual(EMPTY_SCOPE);
    expect(fieldState(p, 'scope')).toBe('clear');
    // 空与继承是**两种**状态 —— 后端 provenance 会区分（global vs 无层）
    expect(fieldState(emptyProfile(), 'scope')).toBe('inherit');
  });

  it('override 从继承出发时沿用下层值作为编辑起点（不制造空值）', () => {
    const p = setFieldState(emptyProfile(), 'strategy', 'override', lower);
    expect(p.strategy).toBe('ranked');
    const q = setFieldState(emptyProfile(), 'default_group', 'override', lower);
    expect(q.default_group).toBe('g1');
  });

  it('枚举字段被要求 clear 时退回继承，不产生非法值', () => {
    const p = setFieldState(withScope({ strategy: 'ranked' }), 'strategy', 'clear', lower);
    expect(p.strategy).toBeNull();
  });
});

describe('档案合并 mergeProfiles（§2.2 逐字段三态覆盖的直译）', () => {
  it('最右侧「不是继承」的层生效，其余字段继续继承', () => {
    const global = withScope({
      strategy: 'weighted',
      sticky: 'auto',
      scope: { ...EMPTY_SCOPE, providers: ['openai'] },
    });
    // 模型级只覆盖 strategy —— 其余必须继续继承 global，不得被清空
    const model = withScope({ strategy: 'ranked' });
    const eff = mergeProfiles(global, model);
    expect(eff.strategy).toBe('ranked');
    expect(eff.sticky).toBe('auto');
    expect(eff.scope).toEqual(global.scope);
  });

  it('显式清空**会**覆盖下层（这正是三态存在的理由）', () => {
    const global = withScope({ scope: { ...EMPTY_SCOPE, providers: ['openai'] } });
    const model = withScope({ scope: { ...EMPTY_SCOPE } });
    expect(mergeProfiles(global, model).scope).toEqual(EMPTY_SCOPE);
  });

  it('全继承 = 下层原样', () => {
    const global = withScope({ strategy: 'ranked', needs: { ...EMPTY_NEEDS, images: true } });
    expect(mergeProfiles(global, emptyProfile())).toEqual(global);
  });
});

describe('层密度与名单文本互转', () => {
  it('isProfileEmpty / profileOverrideCount', () => {
    expect(isProfileEmpty(emptyProfile())).toBe(true);
    expect(profileOverrideCount(emptyProfile())).toBe(0);
    const p = withScope({ strategy: 'ranked', sort_rules: [] });
    expect(profileOverrideCount(p)).toBe(2); // 覆盖 1 项 + 清空 1 项都算「本层表过态」
    expect(isProfileEmpty(p)).toBe(false);
  });

  it('linesToItems：去空行 / 去重 / 保序；与 itemsToLines 互逆', () => {
    expect(linesToItems('b\n\na\n b \n')).toEqual(['b', 'a']);
    expect(itemsToLines(['b', 'a'])).toBe('b\na');
    expect(linesToItems(itemsToLines(['x', 'y']))).toEqual(['x', 'y']);
  });
});
