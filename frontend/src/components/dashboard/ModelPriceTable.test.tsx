import { describe, expect, it, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { vi } from 'vitest';
import i18n from 'i18next';
import ModelPriceTable, { priceRowOf } from './ModelPriceTable';
import { api } from '../../api';
import '../../i18n';

/**
 * 用户价格表（业主原话「登陆后没看每个模型的价格」）。
 *
 * 三条判据，各自钉一种「看起来对、其实错」的形态：
 * 1. 单位换算：后端是「美元 / 1K token」，界面必须是「¥ / 百万 token」（×1000 × 汇率）。
 *    换算写反（除 1000）时表格仍然渲染、数字仍然「像个价格」，只有逐字对比才抓得住。
 * 2. 三态计价方式不可混：免费 ≠ 未定价 ≠ 按次。把 `free` 显示成 0、把 `count`
 *    的两列并排成输入/输出单价，都会让用户按 token 心算而对不上账单。
 * 3. 汇率不可用时不假装 1:1：必须退回 $ 口径，不能把美元数字挂上 ¥。
 */

beforeEach(async () => {
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
});

function renderWith(): { container: HTMLElement } {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <ModelPriceTable />
    </QueryClientProvider>,
  );
}

describe('priceRowOf — 单位与三态计价（纯函数）', () => {
  it('按 token：美元/1K → ¥/百万（×1000 × 汇率 7.2）', () => {
    // 0.0003 USD/1K → 0.3 USD/百万 → ×7.2 = 2.16 元/百万
    const p = priceRowOf({ id: 'glm-4', price_input: 0.0003, price_output: 0.0006 }, 7.2);
    expect(p.type).toBe('token');
    expect(p.currency).toBe('¥');
    expect(p.input).toBe('2.16');
    expect(p.output).toBe('4.32');
    // 缓存价未配置 → null（不是 0，0 表示「缓存读免费」）
    expect(p.cache).toBeNull();
  });

  it('缓存价配了就换算，配成 0 则显示 0（与 null 区分）', () => {
    const withZero = priceRowOf({ id: 'm', price_input: 0.001, price_cache: 0 }, 1);
    expect(withZero.cache).toBe('0');
    const withValue = priceRowOf({ id: 'm', price_input: 0.001, price_cache: 0.0002 }, 1);
    expect(withValue.cache).toBe('0.2');
  });

  it('免费：显示 0 且标 free，不是「未定价」', () => {
    const p = priceRowOf({ id: 'free-model', price_type: 'free', price_input: 0, price_output: 0 }, 1);
    expect(p.type).toBe('free');
    expect(p.free).toBe(true);
    expect(p.input).toBe('0');
  });

  it('按次：input_price 是每次价格，不做「每百万」换算，也不给输入/输出两列', () => {
    // 0.04 USD/次 × 7.2 = 0.288 元/次
    const p = priceRowOf({ id: 'sdxl', price_type: 'count', price_input: 0.04 }, 7.2);
    expect(p.type).toBe('count');
    expect(p.perCall).toBe('0.288');
    // 按次没有 token 单价：空串（表格据此改走 colSpan 单元格）
    expect(p.input).toBe('');
    expect(p.output).toBe('');
  });

  it('未知 price_type 按 token 处理（与后端 default_price_type 一致），不误判成免费', () => {
    const p = priceRowOf({ id: 'm', price_type: 'weird', price_input: 0.001 }, 1);
    expect(p.type).toBe('token');
    expect(p.free).toBe(false);
    expect(p.input).toBe('1');
  });

  it('汇率不可用（0/NaN/负数）时退回美元口径，不把 $ 数字挂上 ¥', () => {
    for (const fx of [0, Number.NaN, -3]) {
      const p = priceRowOf({ id: 'm', price_input: 0.0003 }, fx);
      expect(p.currency).toBe('$');
      // 仍按「每百万」换算（×1000），只是不做汇率折算
      expect(p.input).toBe('0.3');
    }
  });
});

describe('ModelPriceTable — 渲染与取数', () => {
  it('渲染清单、单位说明与「未定价不显示」的口径说明', async () => {
    vi.spyOn(api, 'listModels').mockResolvedValue({
      success: true,
      data: [
        { id: 'glm-4.7-flash', owned_by: 'zhipu', price_input: 0.0003, price_output: 0.0006, price_type: 'token' },
        { id: 'sdxl', owned_by: 'flux', price_input: 0.04, price_type: 'count' },
        { id: 'freebie', owned_by: 'local', price_input: 0, price_output: 0, price_type: 'free' },
      ],
      // 后端随清单下发的换算口径
      fx_usd_cny: 7.2,
    } as never);

    const { container } = renderWith();
    await waitFor(() => expect(container.textContent).toContain('glm-4.7-flash'));

    // 三态各出现一次，且各自形态正确
    expect(container.textContent).toContain('按 Token');
    expect(container.textContent).toContain('按次');
    expect(container.textContent).toContain('免费');
    // 汇率生效 → ¥ 口径
    expect(container.textContent).toContain('¥');
    // 未定价模型不在此表的口径说明必须在（否则用户会以为表是「全量模型」）
    expect(container.textContent).toContain('未定价');
  });

  it('搜索按模型名过滤，不改变排序分组', async () => {
    vi.spyOn(api, 'listModels').mockResolvedValue({
      success: true,
      data: [
        { id: 'bbb-model', owned_by: 'z-p', price_input: 0.001 },
        { id: 'aaa-model', owned_by: 'a-p', price_input: 0.001 },
      ],
      fx_usd_cny: 1,
    } as never);

    const { container } = renderWith();
    await waitFor(() => expect(container.textContent).toContain('bbb-model'));
    // 默认按供应商排序：a-p 的 aaa-model 在前
    const nameCells = (): string[] =>
      [...container.querySelectorAll('tbody tr td:first-child')].map((td) => td.textContent ?? '');
    expect(nameCells()).toEqual(['aaa-model', 'bbb-model']);

    fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'bbb' } });
    expect(nameCells()).toEqual(['bbb-model']);
  });

  it('清单为空时给「暂无可用」而不是空白表格', async () => {
    vi.spyOn(api, 'listModels').mockResolvedValue({ success: true, data: [], fx_usd_cny: 1 } as never);
    const { container } = renderWith();
    await waitFor(() => expect(container.textContent).toContain('暂无可用模型'));
    expect(container.querySelector('tbody')).toBeNull();
  });

  it('老后端不带 fx_usd_cny 时不崩、按 1.0 折算并仍用 ¥（与既有展示口径一致）', async () => {
    vi.spyOn(api, 'listModels').mockResolvedValue({
      success: true,
      data: [{ id: 'm', price_input: 0.0003 }],
    } as never);
    const { container } = renderWith();
    await waitFor(() => expect(container.textContent).toContain('m'));
    expect(container.textContent).toContain('¥0.3');
  });
});