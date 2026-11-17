/**
 * 特殊路由面板（压缩 / 标题 / 视觉兼容）回归。
 *
 * 钉住四件事：
 * 1. 规则表**增删**（添加规则 → 应用 → 行出现；删除 → 行消失）；
 * 2. **revision 409 冲突**：保存被拒时显示「修订号冲突」，且**草稿不丢**
 *    （未覆盖他人修改，也不清空用户正在编的内容）；
 * 3. 端点未就绪（404）时明确降级，不谎报成功；
 * 4. 标题 / 视觉两块能按后端形状渲染既有规则（形状守卫不崩）。
 *
 * mock 形状按后端 serde 落地的 camelCase 结构构造（route_special / vision）。
 */
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, cleanup, waitFor, fireEvent } from '@testing-library/react';
import type { ReactElement } from 'react';

const apiMock = vi.hoisted(() => ({
  getCompactRoutes: vi.fn(),
  saveCompactRoutes: vi.fn(),
  getTitleRoutes: vi.fn(),
  saveTitleRoutes: vi.fn(),
  getVisionCompatibility: vi.fn(),
  saveVisionCompatibility: vi.fn(),
}));
vi.mock('../../../api', () => ({ api: apiMock }));

import CompactRoutes from './CompactRoutes';
import TitleRoutes from './TitleRoutes';
import VisionCompat from './VisionCompat';
import {
  foldVisionBlocks,
  normalizeVisionConfig,
  type VisionSettingsConfig,
} from '../../../types/routing-workspace';

const COMPACT_VIEW = {
  success: true,
  data: {
    config: { enabled: true, rules: [] },
    error: null,
    revision: 'rev-1',
    clients: [{ id: 'zcode', label: 'ZCode' }],
    models: [{ id: 'm1' }, { id: 'm2' }],
    maxRules: 8,
  },
};

function addCompactRule(container: HTMLElement): void {
  fireEvent.click(screen.getByText('添加规则'));
  const source = container.querySelector('select[aria-label="原模型"]') as HTMLElement;
  const target = container.querySelector('select[aria-label="压缩模型"]') as HTMLElement;
  expect(source).toBeTruthy();
  fireEvent.change(source, { target: { value: 'm1' } });
  fireEvent.change(target, { target: { value: 'm2' } });
  fireEvent.click(screen.getByText('应用'));
}

describe('特殊路由面板', () => {
  beforeEach(() => {
    Object.values(apiMock).forEach((f) => f.mockReset());
    apiMock.getCompactRoutes.mockResolvedValue(COMPACT_VIEW);
    apiMock.saveCompactRoutes.mockResolvedValue(COMPACT_VIEW);
    apiMock.getTitleRoutes.mockResolvedValue(COMPACT_VIEW);
    apiMock.saveTitleRoutes.mockResolvedValue(COMPACT_VIEW);
    apiMock.getVisionCompatibility.mockResolvedValue({
      success: true,
      data: { config: { enabled: true, rules: [] }, revision: 'v1', models: [] },
    });
    apiMock.saveVisionCompatibility.mockResolvedValue({
      success: true,
      data: { config: { enabled: true, rules: [] }, revision: 'v1', models: [] },
    });
  });
  afterEach(() => cleanup());

  it('压缩面板：规则表增删 + 未就绪降级', async () => {
    const { container } = render(<CompactRoutes active /> as ReactElement);
    await waitFor(() => expect(container.textContent).toContain('还没有规则'), { timeout: 3000 });

    addCompactRule(container);
    await waitFor(() => expect(container.textContent).toContain('m1'));
    expect(container.textContent).toContain('m2');

    fireEvent.click(screen.getByText('删除'));
    await waitFor(() => expect(container.textContent).toContain('还没有规则'));
    expect(container.textContent).not.toContain('m2 的');
  });

  it('压缩面板：保存 409 → 冲突提示且草稿不丢', async () => {
    apiMock.saveCompactRoutes.mockRejectedValue(
      Object.assign(new Error('revision mismatch'), { status: 409 }),
    );
    const { container } = render(<CompactRoutes active /> as ReactElement);
    await waitFor(() => expect(container.textContent).toContain('还没有规则'), { timeout: 3000 });

    addCompactRule(container);
    await waitFor(() => expect(container.textContent).toContain('m1'));

    fireEvent.click(screen.getByText('保存'));
    await waitFor(() => expect(container.textContent).toContain('修订号冲突'), { timeout: 3000 });
    // 草稿仍在（未被清空，也没被服务端版本覆盖）
    expect(container.textContent).toContain('m1');
    // 保存请求带上了最近一次 GET 的 revision（后端据此做 CAS）
    expect(apiMock.saveCompactRoutes).toHaveBeenCalledWith(
      expect.objectContaining({ rules: expect.any(Array) }),
      'rev-1',
    );
  });

  it('压缩面板：端点 404 → 明确告知未就绪', async () => {
    apiMock.getCompactRoutes.mockRejectedValue(
      Object.assign(new Error('nf'), { status: 404 }),
    );
    const { container } = render(<CompactRoutes active /> as ReactElement);
    await waitFor(() => expect(container.textContent).toContain('端点尚未就绪'), { timeout: 3000 });
  });

  it('压缩面板：激活后只拉一次（回调身份变化不触发重拉，防请求环）', async () => {
    const { container } = render(<CompactRoutes active /> as ReactElement);
    await waitFor(() => expect(container.textContent).toContain('还没有规则'), { timeout: 3000 });
    expect(apiMock.getCompactRoutes).toHaveBeenCalledTimes(1);
  });

  it('标题面板：按后端形状渲染既有规则', async () => {
    apiMock.getTitleRoutes.mockResolvedValue({
      success: true,
      data: {
        config: {
          enabled: true,
          rules: [{ enabled: true, clients: ['codex'], model: 'cheap-model', fallback: 'original' }],
        },
        revision: 't1',
        clients: [
          { id: 'codex', label: 'Codex', supported: true },
          { id: 'zcode', label: 'ZCode', supported: false },
        ],
        models: [{ id: 'cheap-model' }],
        maxRules: 32,
      },
    });
    const { container } = render(<TitleRoutes active /> as ReactElement);
    await waitFor(() => expect(container.textContent).toContain('cheap-model'), { timeout: 3000 });
    // 规则行标题是插值文案（无 i18n 实例时保留原键），客户端识别在行详情文案里
    expect(container.textContent).toContain('{{clients}} 的标题请求');
    expect(container.textContent).toContain('失败时退回原模型');
    expect(apiMock.getTitleRoutes).toHaveBeenCalled();
  });

  it('视觉面板：按后端形状渲染既有规则（assist）', async () => {
    apiMock.getVisionCompatibility.mockResolvedValue({
      success: true,
      data: {
        config: {
          enabled: true,
          rules: [
            {
              enabled: true,
              models: [{ provider: 'openai', model: 'gpt-4o' }],
              mode: 'assist',
              source: 'shared',
              target: null,
              reuseResults: true,
              preferCurrentRoute: true,
              fallback: 'stop',
              targets: [],
            },
          ],
        },
        revision: 'v1',
        models: [
          {
            provider: 'openai',
            providerLabel: 'OpenAI',
            model: 'gpt-4o',
            name: 'GPT-4o',
            capabilities: { supportsImages: false },
          },
        ],
        maxTargets: 8,
      },
    });
    const { container } = render(<VisionCompat active /> as ReactElement);
    await waitFor(() => expect(container.textContent).toContain('辅助识图'), { timeout: 3000 });
    expect(container.textContent).toContain('{{models}} 看不了图时');
    expect(container.textContent).toContain('先读图再交回');
    expect(apiMock.getVisionCompatibility).toHaveBeenCalled();
  });

  it('视觉面板：契约收尾 —— preferCurrentRoute 恒 false（后端拒 true / GET 恒回 false）', () => {
    // 旧配置里是 true、规则缺 fallback：归一后必须是 false + fallback=designated，
    // 否则后端 PUT 会 400（容器级 serde default 会把缺省值补成 true）。
    const normalized = normalizeVisionConfig({
      enabled: true,
      rules: [
        {
          enabled: true,
          models: [{ provider: 'openai', model: 'gpt-4o' }],
          mode: 'reroute',
          source: 'custom',
          target: null,
          reuseResults: false,
          preferCurrentRoute: true,
          fallback: 'stop',
          targets: [{ provider: 'openai', model: 'gpt-4o', reasoning: '' }],
        } as never,
      ],
    });
    expect(normalized.rules[0].preferCurrentRoute).toBe(false);
    expect(normalized.rules[0].fallback).toBe('designated');

    // 保存前折块：reroute 块必须与规则一致（prefer=false / designated / targets 非空），
    // 且旧版顶层字段 mode/overrides 被清掉（否则后端走旧全局分支忽略规则）。
    const folded = foldVisionBlocks({
      ...normalized,
      mode: 'reroute',
      overrides: [{ provider: 'x', model: 'y', mode: 'reroute' }],
    } as VisionSettingsConfig);
    expect(folded.reroute?.preferCurrentRoute).toBe(false);
    expect(folded.reroute?.fallback).toBe('designated');
    expect(folded.reroute?.targets?.length).toBe(1);
    expect(folded.mode).toBeUndefined();
    expect(folded.overrides).toBeUndefined();
  });
});