/**
 * 破甲 · 异源候选推荐的纯函数与面板判据。
 *
 * 钉住两件最容易错的事：
 * 1. `fallbackDiff` 的「点下去会丢掉哪些模型」必须是**前后一致**的
 *    （当前已填的模型若仍在推荐里就不该算「移除」）；
 * 2. 主模型未知 / 池子为空时**不能**给出一个「点了等于清空」的按钮。
 */
import { describe, expect, it, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import type { TFunction } from 'i18next';
import type { PurgeFallbackSuggestion } from '../types';
import { FallbackSuggestPanel, fallbackDiff } from './purge-fallback-suggest';

/**
 * 测试用 `t`：把插值打出来（真实 i18n 要 `initReactI18next` 全套，这里只判
 * 「文案里有没有那句话」，不需要语言包）。同理**不做真实翻译**——断言的都是
 * 中文源串本身，与 `zh.json` 同形。
 */
const t = ((key: string, params?: Record<string, string | number>): string =>
  params
    ? Object.entries(params).reduce(
        (acc, [k, v]) => acc.split(`{{${k}}}`).join(String(v)),
        key,
      )
    : key) as unknown as TFunction;

function suggestion(over: Partial<PurgeFallbackSuggestion> = {}): PurgeFallbackSuggestion {
  return {
    main_model: 'claude-sonnet-4-6',
    main_vendor: 'anthropic',
    main_known: true,
    candidates: [
      {
        model: 'kimi-k3',
        provider: 'openai_compatible',
        vendor: 'moonshot',
        price: 'in 4 / out 12 每 1M',
        channels: 2,
        accounts: 0,
        free: false,
        preview: false,
        context_length: 128_000,
        reason: '异源厂商 anthropic → moonshot · 2 条启用渠道 · 已定价',
      },
      {
        model: 'glm-5.3',
        provider: 'zai',
        vendor: 'zhipu',
        price: '免费',
        channels: 1,
        accounts: 0,
        free: true,
        preview: false,
        context_length: 200_000,
        reason: '异源厂商 anthropic → zhipu · 1 条启用渠道 · 免费档',
      },
    ],
    models: ['kimi-k3', 'glm-5.3'],
    excluded: [
      {
        model: 'claude-haiku-4',
        vendor: 'anthropic',
        code: 'same_vendor',
        reason: '与主模型同厂商（anthropic）',
      },
      {
        model: 'unpriced-x',
        vendor: 'moonshot',
        code: 'unpriced',
        reason: '未定价（写进列表也不会生效）（moonshot）',
      },
    ],
    pools: {
      models_total: 196,
      enabled: 180,
      enabled_priced: 150,
      eligible: 120,
      cross_vendor: 90,
    },
    limit: 3,
    ...over,
  };
}

describe('fallbackDiff', () => {
  it('当前已填的模型仍在推荐里 → 算「保留」而不是「移除」', () => {
    const diff = fallbackDiff(['kimi-k3', 'old-model'], ['kimi-k3', 'glm-5.3']);
    expect(diff.kept).toEqual(['kimi-k3']);
    expect(diff.added).toEqual(['glm-5.3']);
    expect(diff.removed).toEqual(['old-model']);
  });

  it('大小写不敏感：已填 GLM-5.3 与推荐的 glm-5.3 视为同一个', () => {
    const diff = fallbackDiff(['GLM-5.3'], ['glm-5.3']);
    expect(diff.kept).toEqual(['glm-5.3']);
    expect(diff.added).toEqual([]);
    expect(diff.removed).toEqual([]);
  });

  it('当前为空时全部算「新增」，不产生「移除」', () => {
    const diff = fallbackDiff([], ['kimi-k3']);
    expect(diff.added).toEqual(['kimi-k3']);
    expect(diff.removed).toEqual([]);
    expect(diff.kept).toEqual([]);
  });

  it('空串/空白条目不计入「已填」（normalize 会丢掉它们，界面不该先报冲突）', () => {
    const diff = fallbackDiff(['', '  '], ['kimi-k3']);
    expect(diff.kept).toEqual([]);
    expect(diff.removed).toEqual([]);
    expect(diff.added).toEqual(['kimi-k3']);
  });
});

describe('FallbackSuggestPanel', () => {
  it('旧后端（无推荐）时给出「手填也能用」，不假装有推荐', () => {
    const view = render(
      <FallbackSuggestPanel t={t} suggestion={undefined} current={[]} onApply={() => {}} />,
    );
    expect(view.getByTestId('purge-fallback-suggest-missing')).toBeTruthy();
    expect(view.queryByTestId('purge-fallback-apply')).toBeNull();
  });

  it('点「一键填入」回传推荐清单（且不改动当前值）', () => {
    const onApply = vi.fn();
    const current = ['old-model'];
    const view = render(
      <FallbackSuggestPanel
        t={t}
        suggestion={suggestion()}
        current={current}
        mainModel="claude-sonnet-4-6"
        onApply={onApply}
      />,
    );
    fireEvent.click(view.getByTestId('purge-fallback-apply'));
    expect(onApply).toHaveBeenCalledWith(['kimi-k3', 'glm-5.3']);
    // 组件是纯展示：调用方的数组不该被就地改写
    expect(current).toEqual(['old-model']);
  });

  it('已有手填内容且与推荐不同 → 按钮文案明说「替换」，并列出会移除谁', () => {
    const view = render(
      <FallbackSuggestPanel
        t={t}
        suggestion={suggestion()}
        current={['my-own-model']}
        onApply={() => {}}
      />,
    );
    expect(view.getByTestId('purge-fallback-apply').textContent).toContain('替换为推荐异源');
    expect(view.getByTestId('purge-fallback-diff').textContent).toContain('my-own-model');
  });

  it('排除清单默认折叠；点开后按原因分组显示（而不是「就是不给你」）', () => {
    const view = render(
      <FallbackSuggestPanel t={t} suggestion={suggestion()} current={[]} onApply={() => {}} />,
    );
    expect(view.queryByText('claude-haiku-4')).toBeNull();
    fireEvent.click(view.getByTestId('purge-fallback-toggle-excluded'));
    expect(view.getByText('claude-haiku-4')).toBeTruthy();
    expect(view.getByText('unpriced-x')).toBeTruthy();
    // 分组标签用人话（后端 code 只做机器码）
    expect(view.getByText('同厂商')).toBeTruthy();
    expect(view.getByText('未定价')).toBeTruthy();
  });

  it('池子为空时按钮禁用（点了只会清空，不该可点）', () => {
    const view = render(
      <FallbackSuggestPanel
        t={t}
        suggestion={suggestion({ candidates: [], models: [], excluded: [] })}
        current={['keep-me']}
        onApply={() => {}}
      />,
    );
    expect(view.getByTestId('purge-fallback-suggest-empty')).toBeTruthy();
    const button = view.getByTestId('purge-fallback-apply') as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });

  it('主模型不在启用清单 → 明说「未做同厂商排除」', () => {
    const view = render(
      <FallbackSuggestPanel
        t={t}
        suggestion={suggestion({ main_known: false })}
        current={[]}
        onApply={() => {}}
      />,
    );
    expect(view.getByTestId('purge-fallback-main-unknown')).toBeTruthy();
  });

  it('「重算」按钮只在给了回调时出现（旧后端/无权限时不留死按钮）', () => {
    const first = render(
      <FallbackSuggestPanel t={t} suggestion={suggestion()} current={[]} onApply={() => {}} />,
    );
    expect(first.queryByTestId('purge-fallback-refresh')).toBeNull();

    const onRefresh = vi.fn();
    const second = render(
      <FallbackSuggestPanel
        t={t}
        suggestion={suggestion()}
        current={[]}
        onApply={() => {}}
        onRefresh={onRefresh}
      />,
    );
    fireEvent.click(second.getByTestId('purge-fallback-refresh'));
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });
});