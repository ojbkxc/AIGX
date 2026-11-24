/**
 * 路由卡片面回归（对齐 Agent2API v3.5.1 的组面板 / 成员卡 / 模型选择器）。
 *
 * 钉住的是「A2A 有、旧版 AIGX 没有」的那几个结构位 —— 只断言旧组件
 * **不可能**出现的东西，这样这组用例在改前必红、改后必绿：
 * 1. 组卡 = `.rt-config-card` + 眼线「跨模型组合」+ 对外模型 ID（`code`）；
 * 2. 成员卡 = `.rt-config-provider` + 两位序号（01/02）+ 参与开关 +
 *    「路由成员操作」更多菜单（提高/降低优先级、参与账号）；
 * 3. 搜索命中时成员卡常开、搜索不改变数据；
 * 4. 「添加模型」弹窗在模型目录缺失时给的是 A2A 的「没有可添加的模型」+ 数据源说明，
 *    而不是一个空白的假列表；
 * 5. 参与开关改的是草稿（onChange 收到的 included 已经是新值），不是本地影子状态。
 */
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, within } from '@testing-library/react';
import type { RoutingGroup } from '../../types/routing-profile';
import GroupEditor from './GroupEditor';

const group = (over: Partial<RoutingGroup> = {}): RoutingGroup => ({
  id: 'g1',
  name: '快速组',
  model: 'fast-chat',
  strategy: null,
  sticky: null,
  members: [
    { provider: 'openai', model: 'gpt-4o', included: true, accounts: ['acc-1', 'acc-2'] },
    { provider: 'anthropic', model: 'claude', included: true, accounts: [] },
    { provider: 'deepseek', model: 'deepseek-chat', included: false, accounts: [] },
  ],
  ...over,
});

const renderEditor = (groups: RoutingGroup[], onChange = vi.fn()): { container: HTMLElement; onChange: typeof onChange } => {
  const { container } = render(<GroupEditor groups={groups} onChange={onChange} />);
  return { container, onChange };
};

describe('路由组卡片（对齐 A2A routing-group-panel）', () => {
  it('组卡骨架：眼线 / 组名 / 对外模型 ID / 两列规则', () => {
    const { container } = renderEditor([group()]);
    expect(container.querySelectorAll('.rt-config-card').length).toBe(1);
    expect(container.textContent).toContain('跨模型组合');
    expect(screen.getByRole('heading', { level: 2 }).textContent).toBe('快速组');
    const alias = container.querySelector('.rt-config-alias code');
    expect(alias?.textContent).toBe('fast-chat');
    // 策略 / 粘滞都在，且「继承上层」档给到了（三态不许退化成两态）
    const selects = container.querySelectorAll<HTMLSelectElement>('.rt-config-rules select');
    expect(selects.length).toBe(2);
    expect(Array.from(selects[0].options).map((o) => o.textContent)).toContain('继承上层（不覆盖）');
  });

  it('成员卡：两位序号 + 提供商 · 模型副标题 + 参与开关 + 更多菜单', () => {
    const { container } = renderEditor([group()]);
    const cards = container.querySelectorAll('.rt-config-provider');
    // 参与区 2 张 + 未参与区 1 张
    expect(cards.length).toBe(3);
    const first = cards[0];
    expect(first.querySelector('.rt-config-rank')?.textContent).toBe('01');
    expect(first.querySelector('.rt-provider-name b')?.textContent).toBe('gpt-4o');
    expect(first.querySelector('.rt-provider-name small')?.textContent).toBe('openai · gpt-4o');
    // aria-label 是带插值的 i18n key（`{{model}} 参与本路由`），测试环境没有
    // i18next 实例时不会解析成具体模型名 —— 按下标取卡片内第一个复选框。
    expect(within(first as HTMLElement).getAllByRole('checkbox').length).toBeGreaterThanOrEqual(1);
    const menu = first.querySelector('.rt-actions-menu');
    expect(menu?.textContent).toContain('提高优先级');
    expect(menu?.textContent).toContain('降低优先级');
    expect(menu?.textContent).toContain('参与账号');
    // 未参与的成员卡是虚线态（is-off）
    expect(cards[2].className).toContain('is-off');
  });

  it('参与开关改的是草稿：onChange 收到 included=false 的新成员表', () => {
    const onChange = vi.fn();
    const { container } = renderEditor([group()], onChange);
    const card = container.querySelectorAll('.rt-config-provider')[0] as HTMLElement;
    fireEvent.click(within(card).getAllByRole('checkbox')[0]);
    expect(onChange).toHaveBeenCalledTimes(1);
    const next = onChange.mock.calls[0][0] as RoutingGroup[];
    expect(next[0].members[0].included).toBe(false);
  });

  it('提高优先级：把第二个成员换到第一位（只重排参与中的成员）', () => {
    const onChange = vi.fn();
    const { container } = renderEditor([group()], onChange);
    const cards = container.querySelectorAll('.rt-config-provider');
    const second = cards[1] as HTMLElement;
    fireEvent.click(within(second).getByRole('menuitem', { name: /提高优先级/ }));
    const next = onChange.mock.calls[0][0] as RoutingGroup[];
    expect(next[0].members.map((m) => m.model)).toEqual(['claude', 'gpt-4o', 'deepseek-chat']);
    expect(next[0].members[2].included).toBe(false);
  });

  it('搜索只筛显示：命中时成员卡常开，未命中的种类进「没有匹配的候选」', () => {
    const { container } = renderEditor([group()]);
    const search = screen.getByPlaceholderText('搜索提供商或模型');
    fireEvent.change(search, { target: { value: 'claude' } });
    const cards = container.querySelectorAll('.rt-config-provider');
    expect(cards.length).toBe(1);
    expect(cards[0].getAttribute('data-route-expanded')).toBe('');
    fireEvent.change(search, { target: { value: '不存在' } });
    expect(container.textContent).toContain('没有匹配的候选');
  });

  it('添加模型弹窗：目录缺失时给空态 + 数据源说明，添加按钮为 0 且禁用', () => {
    const { container } = renderEditor([group()]);
    fireEvent.click(within(container as HTMLElement).getAllByRole('menuitem', { name: /添加模型/ })[0]);
    const dialog = screen.getByRole('dialog', { name: '添加参与模型' });
    expect(within(dialog).getByText('没有可添加的模型')).toBeTruthy();
    expect(within(dialog).getByText(/模型目录未接入/)).toBeTruthy();
    const add = within(dialog).getByRole('button', { name: /添加 · 0/ });
    expect((add as HTMLButtonElement).disabled).toBe(true);
  });

  it('全部移出：二次确认后才改草稿（不是点了就清空）', () => {
    const onChange = vi.fn();
    const { container } = renderEditor([group()], onChange);
    fireEvent.click(within(container as HTMLElement).getByRole('menuitem', { name: '全部移出' }));
    expect(onChange).not.toHaveBeenCalled();
    const dialog = screen.getByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: /全部移出/ }));
    const next = onChange.mock.calls[0][0] as RoutingGroup[];
    expect(next[0].members.every((m) => !m.included)).toBe(true);
  });
});