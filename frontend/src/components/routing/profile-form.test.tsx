/**
 * 档案字段面回归（对齐 Agent2API v3.5.1 routing-config-panel 的字段顺序与分组）。
 *
 * 只钉「改前不存在」的结构位：
 * 1. 策略 / 粘滞并排成 `.rt-field-grid` 两列（A2A 的 routing-config-rules 密度）；
 * 2. 参与方式改成 A2A 的「全部 / 仅加入的参与」档位文案；
 * 3. 参与名单进入覆盖态时渲染**提供商卡片**（`.rt-config-provider`），
 *    且卡片上的参与开关真的改到 `scope.disabled_providers`；
 * 4. 模型筛选只接受 models + onModelChange 成对出现，缺一个就不渲染（不许半死控件）；
 * 5. 保护摘要按 labels 渲染，空数组即「关闭」。
 *
 * 三态（继承/覆盖/清空）是 AIGX 独有项，这里只当输入态使用，不对齐、不削弱。
 */
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, within } from '@testing-library/react';
import ProfileForm from './ProfileForm';
import { emptyProfile, type RoutingProfile } from '../../types/routing-profile';

const profile = (over: Partial<RoutingProfile> = {}): RoutingProfile => ({ ...emptyProfile(), ...over });

const renderForm = (
  value: RoutingProfile,
  extra: Partial<React.ComponentProps<typeof ProfileForm>> = {},
  onChange = vi.fn(),
): { container: HTMLElement; unmount: () => void; onChange: typeof onChange } => {
  const { container, unmount } = render(
    <ProfileForm
      value={value}
      onChange={onChange}
      inherited={emptyProfile()}
      inheritSource="内置默认"
      groupIds={['g1']}
      {...extra}
    />,
  );
  return { container, unmount, onChange };
};

describe('档案字段面（对齐 A2A routing-config-panel）', () => {
  it('策略 / 粘滞并排两列，六个字段的态控件都在', () => {
    const { container } = renderForm(profile());
    expect(container.querySelectorAll('.rt-field-grid > .rt-field').length).toBe(2);
    expect(container.querySelectorAll('.rt-seg').length).toBeGreaterThanOrEqual(6);
  });

  it('参与方式：A2A 档位文案（全部 / 仅加入的参与）', () => {
    // 覆盖态才有编辑控件：空名单是「清空」态的说明位，这里给一个非空名单
    const { container } = renderForm(profile({ scope: { provider_scope: 'all', providers: ['openai'], disabled_providers: [], account_order: [], excluded_accounts: [] } }));
    const options = Array.from(container.querySelectorAll('select')).flatMap((s) => Array.from(s.options)).map((o) => o.textContent);
    expect(options).toContain('全部提供商（自动并入新出现的）');
    expect(options).toContain('仅加入的参与');
  });

  it('提供商卡片：参与开关改到 disabled_providers（不是本地影子状态）', () => {
    const onChange = vi.fn();
    const { container } = renderForm(
      profile({ scope: { provider_scope: 'listed', providers: ['openai', 'anthropic'], disabled_providers: [], account_order: [], excluded_accounts: [] } }),
      {},
      onChange,
    );
    const cards = container.querySelectorAll('.rt-config-provider');
    expect(cards.length).toBe(2);
    expect(cards[0].querySelector('.rt-config-rank')?.textContent).toBe('01');
    fireEvent.click(within(cards[0] as HTMLElement).getAllByRole('checkbox')[0]);
    const next = onChange.mock.calls[0][0] as RoutingProfile;
    expect(next.scope?.disabled_providers).toContain('openai');
    expect(next.scope?.providers).toEqual(['anthropic']);
  });

  it('模型筛选 / 保护摘要：筛选常开（无目录时给空态说明），保护摘要 labels 空即「关闭」', () => {
    const { container, unmount } = renderForm(profile());
    // A3：模型筛选不再依赖页面传 props —— 缺账号目录时渲染空态说明而不是消失
    expect(container.querySelector('.rt-model-filter')).toBeTruthy();
    // A1：protection 未传时自读 /api/routing，读取期间不渲染摘要（不编造开关态）
    expect(container.querySelector('.rt-protection-summary')).toBeNull();
    unmount();

    const full = render(
      <ProfileForm
        value={profile()}
        onChange={vi.fn()}
        inherited={emptyProfile()}
        inheritSource="内置默认"
        groupIds={[]}
        models={['gpt-4o', 'claude']}
        modelFilter=""
        onModelChange={vi.fn()}
        protection={{ labels: [] }}
      />,
    );
    const box = full.container;
    expect(box.querySelector('.rt-model-filter')).toBeTruthy();
    const summary = box.querySelector('.rt-protection-summary');
    expect(summary?.textContent).toContain('全局保护设置');
    expect(summary?.textContent).toContain('关闭');
    // 筛选下拉第一项是「全部」（空值 = 不过滤）
    const filter = box.querySelector('.rt-model-filter select') as HTMLSelectElement;
    expect(filter.options[0].textContent).toBe('全部');
    expect(Array.from(filter.options).map((o) => o.value)).toContain('gpt-4o');
  });
});