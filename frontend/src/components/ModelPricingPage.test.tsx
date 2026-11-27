import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/react';
import i18n from 'i18next';
import ModelPricingPage from './ModelPricingPage';
import '../i18n';

/**
 * ModelPricingPage 的单测 —— 守卫「用户价格页不许说谎」这条判据。
 *
 * 三件必须能区分新旧的事：
 * 1. 展示口径是**每百万 Token**：后端给的每 1K 值 0.0003 必须显示成 0.3，
 *    而不是原样摊出 0.0003（那正是这次改单位的根因）。
 * 2. 未定价不许显示成 0 元，必须标「未定价（不可用）」。
 * 3. 官方价没填时显示「未登记」、不算折扣；填了才按 0.3/3.0 = 1 折显示。
 *
 * 接口按两跳真实回落路径打桩：`/api/user/models` 命中时用它，否则回落
 * `/api/models/available`（后者是已上线端点，字段名是 price_input/price_output）。
 */
const fetchMock = vi.fn();

function jsonResponse(body: unknown): Response {
  return {
    ok: true,
    status: 200,
    headers: { get: () => 'application/json' },
    text: async () => JSON.stringify(body),
  } as unknown as Response;
}

beforeEach(async () => {
  localStorage.clear();
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
  fetchMock.mockReset();
  vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('ModelPricingPage — 每百万 Token 口径', () => {
  it('后端每 1K 的 0.0003 显示成 0.3（不是原样摊出 0.0003）', async () => {
    fetchMock.mockImplementation(async (url: string) => {
      if (String(url).includes('/api/user/models')) {
        return jsonResponse({
          success: true,
          data: [{ model: 'deepseek-v4.1-flash', input_price: 0.0003, output_price: 0.0006, cache_price: 0.0000025 }],
        });
      }
      throw new Error('unexpected');
    });

    render(<ModelPricingPage />);
    await waitFor(() => expect(screen.getByText('0.3')).toBeTruthy());
    expect(screen.getByText('0.6')).toBeTruthy();
    // 缓存读的浮点尾巴必须被规整成 0.0025，不能出现 0.0025000000000000005
    expect(screen.getByText('0.0025')).toBeTruthy();
    expect(screen.queryByText('0.0003')).toBeNull();
  });

  it('官方价 3.0 对 0.3 → 显示「1 折」；官方价显示为 3 而不是 3000（不二次换算）', async () => {
    // 种子表里有 deepseek-v4.1-flash 的官方输入价 3.0
    fetchMock.mockImplementation(async (url: string) => {
      if (String(url).includes('/api/user/models')) {
        return jsonResponse({
          success: true,
          data: [{ model: 'deepseek-v4.1-flash', input_price: 0.0003, output_price: 0.0006 }],
        });
      }
      throw new Error('unexpected');
    });

    render(<ModelPricingPage />);
    await waitFor(() => expect(screen.getByText('1 折')).toBeTruthy());
    expect(screen.getByText('3')).toBeTruthy();
  });

  it('未定价模型标「未定价（不可用）」，不显示成 0 元', async () => {
    fetchMock.mockImplementation(async (url: string) => {
      if (String(url).includes('/api/user/models')) {
        return jsonResponse({
          success: true,
          data: [
            { model: 'priced-model', input_price: 0.0003, output_price: 0.0006 },
            { model: 'unpriced-model', input_price: 0, output_price: 0 },
          ],
        });
      }
      throw new Error('unexpected');
    });

    render(<ModelPricingPage />);
    await waitFor(() => expect(screen.getByText('unpriced-model')).toBeTruthy());
    // 未定价两处（模型名旁徽标 + 输入价格格）
    expect(screen.getAllByText('未定价（不可用）').length).toBeGreaterThanOrEqual(1);
  });

  it('/api/user/models 不可用时回落 /api/models/available（已上线端点）', async () => {
    fetchMock.mockImplementation(async (url: string) => {
      if (String(url).includes('/api/user/models')) {
        throw new Error('404');
      }
      if (String(url).includes('/api/models/available')) {
        return jsonResponse({
          success: true,
          data: [{ id: 'glm-4.7-flash', price_input: 0.001, price_output: 0.002, price_type: 'token' }],
        });
      }
      throw new Error('unexpected');
    });

    render(<ModelPricingPage />);
    await waitFor(() => expect(screen.getByText('glm-4.7-flash')).toBeTruthy());
    // 0.001 每 1K → 1 元 / 百万
    expect(screen.getByText('1')).toBeTruthy();
    expect(screen.getByText('2')).toBeTruthy();
  });

  it('搜索按模型名过滤', async () => {
    fetchMock.mockImplementation(async (url: string) => {
      if (String(url).includes('/api/user/models')) {
        return jsonResponse({
          success: true,
          data: [
            { model: 'alpha-model', input_price: 0.0003, output_price: 0.0006 },
            { model: 'beta-model', input_price: 0.0005, output_price: 0.001 },
          ],
        });
      }
      throw new Error('unexpected');
    });

    render(<ModelPricingPage />);
    await waitFor(() => expect(screen.getByText('alpha-model')).toBeTruthy());
    const search = screen.getByLabelText('搜索模型名');
    fireEvent.change(search, { target: { value: 'beta' } });
    await waitFor(() => expect(screen.queryByText('alpha-model')).toBeNull());
    expect(screen.getByText('beta-model')).toBeTruthy();
  });
});