// OAuth 配置面板的**渲染层**测试。
//
// 纯函数测（api/endpoints/oauth.test.ts）钉不住三件事，只在真实 DOM 上才能验：
// 1. 长/短两套回调地址是否真的都被渲染出来，以及「当前实际会用」标记是否跟着
//    draft.redirect_uri 走（标记错位 = 用户拿着错的串去平台登记）；
// 2. secret 留空时保存请求里**不能出现** client_secret（出现空串就会清空后端密钥）；
// 3. 三家 provider 卡片都在，且未配置的显示未启用。
import { describe, expect, it, beforeEach, vi } from 'vitest';
import { render, fireEvent, cleanup, screen, waitFor } from '@testing-library/react';
import OAuthConfigPanel from './OAuthConfigPanel';
import i18n from '../i18n';
import { api } from '../api';
import type { ApiResponse } from '../types';
import type { OauthConfig } from '../types/oauth';

vi.mock('../api', () => ({
  api: {
    getOauthConfig: vi.fn(),
    updateOauthConfig: vi.fn(),
  },
}));

function serverConfig(patch?: Partial<OauthConfig>): OauthConfig {
  return {
    github: {
      client_id: 'Ov23lioj',
      client_secret: '...',
      redirect_uri: 'https://api.example.com/api/auth/github/callback',
      ready: true,
    },
    google: { client_id: '', client_secret: '', redirect_uri: '', ready: false },
    linuxdo: {
      client_id: 'WxSdu3yJ',
      client_secret: '',
      redirect_uri: 'https://api.example.com/oauth/linuxdo',
      ready: false,
    },
    ...patch,
  };
}

function ok(data: OauthConfig): ApiResponse<OauthConfig> {
  return { success: true, data } as ApiResponse<OauthConfig>;
}

async function renderPanel(cfg: OauthConfig = serverConfig()): Promise<void> {
  vi.mocked(api.getOauthConfig).mockResolvedValue(ok(cfg));
  vi.mocked(api.updateOauthConfig).mockResolvedValue(ok(cfg));
  render(<OAuthConfigPanel />);
  await waitFor(() => {
    expect(screen.getByText('GitHub')).toBeTruthy();
  });
}

/**
 * 语言固定为中文：本文件断言中文文案，而 i18n 实例在 vitest 进程里是共享的，
 * 别的测试文件可能已把它切成别的语言（jsdom 不做文件级隔离）。
 * 这里只负责「自己看到的语言」，不替全量套件设语言。
 */
beforeEach(async () => {
  cleanup();
  vi.clearAllMocks();
  localStorage.clear();
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
});

describe('OAuthConfigPanel —— 三家 provider 的入口与状态', () => {
  it('三张卡片都在（GitHub / Google / LinuxDO）', async () => {
    await renderPanel();
    expect(document.querySelector('[data-provider="github"]')).toBeTruthy();
    expect(document.querySelector('[data-provider="google"]')).toBeTruthy();
    expect(document.querySelector('[data-provider="linuxdo"]')).toBeTruthy();
  });

  it('未配置的 provider 显示未启用，已配置的显示已启用', async () => {
    await renderPanel();
    const g = document.querySelector('[data-provider="github"] .oauth-status-badge');
    const go = document.querySelector('[data-provider="google"] .oauth-status-badge');
    expect(g?.getAttribute('data-ready')).toBe('true');
    expect(go?.getAttribute('data-ready')).toBe('false');
  });

  it('秘钥输入框提示「留空表示不修改」（掩码回读的唯一正确交互）', async () => {
    await renderPanel();
    const input = document.querySelector(
      '[data-provider="github"] input[type="password"]',
    ) as HTMLInputElement | null;
    expect(input).toBeTruthy();
    expect(input?.placeholder).toContain('留空表示不修改');
    // 已配置的 secret 不回显到输入框（只能看到掩码语义，看不到原值/`...`）
    expect(input?.value).toBe('');
  });
});

describe('OAuthConfigPanel —— 两套回调路径的呈现', () => {
  it('长/短两条候选地址都列出，且标记跟在已配置的 redirect_uri 上', async () => {
    await renderPanel();
    const card = document.querySelector('[data-provider="github"]') as HTMLElement;
    const rows = Array.from(card.querySelectorAll('.oauth-callback-row'));
    expect(rows).toHaveLength(2);
    const active = rows.filter((r) => r.getAttribute('data-active') === 'true');
    expect(active).toHaveLength(1);
    // 配置的是长回调 → 激活的是长回调那行
    expect(active[0].textContent).toContain('/api/auth/github/callback');
    expect(active[0].textContent).toContain('当前实际会用');
  });

  it('点「填入这里」把短回调写进 redirect_uri，标记随之切换', async () => {
    await renderPanel();
    const card = document.querySelector('[data-provider="github"]') as HTMLElement;
    const fillButtons = Array.from(
      card.querySelectorAll('.oauth-callback-row button'),
    ).filter((b) => b.textContent?.includes('填入这里'));
    expect(fillButtons).toHaveLength(1);
    fireEvent.click(fillButtons[0]);

    await waitFor(() => {
      const active = Array.from(card.querySelectorAll('.oauth-callback-row')).filter(
        (r) => r.getAttribute('data-active') === 'true',
      );
      expect(active).toHaveLength(1);
      expect(active[0].textContent).toContain('/oauth/github');
    });
    const input = card.querySelector('input[id="oauth-github-redirect-uri"]') as HTMLInputElement;
    expect(input.value).toMatch(/\/oauth\/github$/);
  });

  it('linuxdo 的配置是短回调 → 激活的是短回调那行', async () => {
    await renderPanel();
    const card = document.querySelector('[data-provider="linuxdo"]') as HTMLElement;
    const active = Array.from(card.querySelectorAll('.oauth-callback-row')).filter(
      (r) => r.getAttribute('data-active') === 'true',
    );
    expect(active).toHaveLength(1);
    expect(active[0].textContent).toContain('/oauth/linuxdo');
  });
});

describe('OAuthConfigPanel —— 保存载荷', () => {
  it('只改 client_id：请求体里没有 client_secret（留空 = 不修改）', async () => {
    await renderPanel();
    const idInput = document.querySelector(
      '[data-provider="github"] input[id="oauth-github-client-id"]',
    ) as HTMLInputElement;
    fireEvent.change(idInput, { target: { value: 'Ov23NEW' } });

    const saveBtn = screen.getByRole('button', { name: '保存更改' });
    fireEvent.click(saveBtn);

    await waitFor(() => {
      expect(api.updateOauthConfig).toHaveBeenCalledTimes(1);
    });
    const body = vi.mocked(api.updateOauthConfig).mock.calls[0][0];
    expect(body).toEqual({ github: { client_id: 'Ov23NEW' } });
    expect(Object.prototype.hasOwnProperty.call(body.github, 'client_secret')).toBe(false);
  });

  it('没有任何改动时保存按钮禁用（不发空 PUT）', async () => {
    await renderPanel();
    const saveBtn = screen.getByRole('button', { name: '保存更改' }) as HTMLButtonElement;
    expect(saveBtn.disabled).toBe(true);
    expect(api.updateOauthConfig).not.toHaveBeenCalled();
  });
});
