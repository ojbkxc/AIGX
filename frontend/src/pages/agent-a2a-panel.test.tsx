/**
 * A2A v3.5.1 Agent 页对齐回归。
 *
 * 钉住三件事（都对着 A2A 的可见行为，不看内部实现）：
 * 1. **分区与筛选**：工具条搜索 + 四档分段（全部 / 接入本网关 / 未接入 / 未检测到）
 *    + 左栏「已检测到 / 未检测到」分组 + 右栏空态，与 A2A agent-page 一致。
 * 2. **筛选口径**：`visibleClients` / `counts` 对 via / direct / missing 三档的判定
 *    与 A2A agent-state.ts 相同（direct 要求 detected 且未接网关）。
 * 3. **模型列表区**：ModelsFields 在目录为空时按 A2A 空态渲染「没有可添加的模型」，
 *    目录有数据时按一行一项列出并可勾选。
 *
 * AIGX 后端暂无客户端检测数据源，页面保持空列表；本测不伪造后端数据，
 * 只覆盖结构、筛选语义与模型列表的表单行为。
 */
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';

vi.mock('../api', () => ({ api: {} }));

import { A2aAgentPanel } from './agent-a2a-panel';
import { A2aModelsFields, type A2aCatalogModel } from './agent-a2a-models';
import { counts, visibleClients, setFilter, clearFilters, getA2aStore } from './agent-a2a-state';
import type { A2aClient, A2aProfile } from './agent-a2a-types';

function makeClient(overrides: Partial<A2aClient> = {}): A2aClient {
  return {
    id: 'claude-code',
    name: 'Claude Code',
    vendor: 'Anthropic',
    hue: 210,
    letter: 'CC',
    icon: '',
    iconFull: false,
    configPath: '~/.claude/settings.json',
    resolvedPath: 'C:/Users/test/.claude/settings.json',
    pathOverride: '',
    format: 'json',
    configMode: 'providers',
    protocol: 'anthropic',
    protocols: ['anthropic'],
    installed: true,
    detected: true,
    detectionState: 'ready',
    evidence: [],
    version: '2.0.0',
    writable: true,
    supportsModels: true,
    supportsEffort: true,
    supportsDefault: true,
    supportsToggle: true,
    supportsGateway: true,
    supportsEnvKey: true,
    supportNote: '',
    docsUrl: '',
    needsRestart: false,
    configurations: [],
    profiles: [],
    live: [],
    readError: null,
    storeError: null,
    viaGateway: false,
    activeId: null,
    revision: 'r1',
    applied: {},
    ...overrides,
  };
}

function emptyProfile(overrides: Partial<A2aProfile> = {}): A2aProfile {
  return {
    id: 'p-1',
    name: 'Agent2API',
    kind: 'gateway',
    protocol: 'anthropic',
    baseUrl: 'https://gw.example.com',
    model: '',
    provider: '',
    effort: '',
    enabled: true,
    default: false,
    note: '',
    models: [],
    claudeModels: { opus: '', sonnet: '', haiku: '', subagent: '' },
    authMode: 'gateway',
    apiKey: '',
    envKey: '',
    gatewayKeyId: '',
    ...overrides,
  };
}

beforeEach(() => {
  clearFilters();
});

afterEach(() => {
  cleanup();
});

describe('A2A 面板结构', () => {
  it('渲染工具条、四档筛选与左右分栏', async () => {
    render(<A2aAgentPanel />);
    expect(screen.getByPlaceholderText('搜索客户端 / 配置文件…')).toBeTruthy();
    expect(screen.getByText('全部')).toBeTruthy();
    expect(screen.getByText('接入本网关')).toBeTruthy();
    expect(screen.getByText('未接入')).toBeTruthy();
    expect(screen.getByText('未检测到')).toBeTruthy();
    // 首帧是「正在检测…」，检测结束后按钮文案回到「重新检测」（对齐 A2A）
    await waitFor(() => {
      expect(screen.getAllByRole('button', { name: /重新检测/ }).length).toBeGreaterThan(0);
    });
    expect(screen.getByRole('complementary', { name: '客户端清单' })).toBeTruthy();
  });

  it('空列表时右栏给出 A2A 的「暂无客户端数据」空态', async () => {
    render(<A2aAgentPanel />);
    await waitFor(() => {
      expect(screen.getByText('暂无客户端数据')).toBeTruthy();
    });
  });

  // 面板改为挂载后异步拉 `/api/agents`，首帧是骨架（`initialLoading`），
  // 空态要等检测结束才渲染 —— 同步断言会撞在骨架上。
  it('搜索无命中时左栏出现「没有匹配的客户端」与「清除筛选」', async () => {
    render(<A2aAgentPanel />);
    fireEvent.change(screen.getByPlaceholderText('搜索客户端 / 配置文件…'), { target: { value: 'zzz' } });
    await waitFor(() => {
      expect(screen.getByText('没有匹配的客户端')).toBeTruthy();
    });
    expect(screen.getByRole('button', { name: '清除筛选' })).toBeTruthy();
  });
});

describe('A2A 筛选口径（对齐 agent-state.ts）', () => {
  it('via / direct / missing 三档的归属与计数', () => {
    const client = makeClient();
    const store = {
      ...getA2aStore(),
      clients: [makeClient({ id: 'a', viaGateway: true }), makeClient({ id: 'b' }), makeClient({ id: 'c', detected: false })],
    };
    const stats = counts(store);
    expect(stats.all).toBe(3);
    expect(stats.via).toBe(1);
    // direct = detected 且未接入（b），missing = 未检测到（c）
    expect(stats.direct).toBe(1);
    expect(stats.missing).toBe(1);
    expect(visibleClients({ ...store, filter: 'via' }).map((item) => item.id)).toEqual(['a']);
    expect(visibleClients({ ...store, filter: 'direct' }).map((item) => item.id)).toEqual(['b']);
    expect(visibleClients({ ...store, filter: 'missing' }).map((item) => item.id)).toEqual(['c']);
    expect(client.viaGateway).toBe(false);
  });

  it('搜索同时匹配名称 / 厂商 / 配置路径', () => {
    const store = {
      ...getA2aStore(),
      clients: [makeClient({ id: 'x', name: 'Claude Code', vendor: 'Anthropic' })],
    };
    expect(visibleClients({ ...store, query: 'anthropic' }).length).toBe(1);
    expect(visibleClients({ ...store, query: 'settings.json' }).length).toBe(1);
    expect(visibleClients({ ...store, query: 'nothing' }).length).toBe(0);
  });

  it('setFilter 在列表为空时保持选中为空串', () => {
    setFilter('via');
    expect(getA2aStore().filter).toBe('via');
    expect(getA2aStore().selected).toBe('');
    clearFilters();
    expect(getA2aStore().filter).toBe('all');
  });
});

describe('A2A 模型列表区', () => {
  const catalog: A2aCatalogModel[] = [
    { id: 'claude-sonnet-4', name: 'Claude Sonnet 4', reasoningLevels: ['high'], effort: '', label: 'Claude Sonnet 4 · claude-sonnet-4' },
    { id: 'claude-haiku-4', name: 'Claude Haiku 4', reasoningLevels: ['low'], effort: '', label: 'Claude Haiku 4 · claude-haiku-4' },
  ];

  it('目录为空时按 A2A 空态提示「没有可添加的模型」', () => {
    render(
      <A2aModelsFields
        client={makeClient()}
        draft={emptyProfile()}
        update={() => undefined}
        catalog={[]}
        disabled={false}
      />,
    );
    expect(screen.getByText('模型列表')).toBeTruthy();
    expect(screen.getByText('没有可添加的模型')).toBeTruthy();
    expect(screen.getByRole('button', { name: '手动添加模型' })).toBeTruthy();
  });

  it('目录有数据时逐项列出并显示已选计数', () => {
    render(
      <A2aModelsFields
        client={makeClient()}
        draft={emptyProfile()}
        update={() => undefined}
        catalog={catalog}
        disabled={false}
      />,
    );
    expect(screen.getByText('Claude Sonnet 4')).toBeTruthy();
    expect(screen.getByText('Claude Haiku 4')).toBeTruthy();
    expect(screen.getByText('已选择 0 / 2 个')).toBeTruthy();
  });

  it('勾选候选模型会回调选中的模型 ID 列表', () => {
    const onChange = vi.fn();
    const update = vi.fn((patch: Partial<A2aProfile>) => onChange(patch.models?.map((model) => model.id)));
    render(
      <A2aModelsFields
        client={makeClient()}
        draft={emptyProfile()}
        update={update}
        catalog={catalog}
        disabled={false}
      />,
    );
    fireEvent.click(screen.getByLabelText('Claude Sonnet 4 · claude-sonnet-4'));
    expect(update).toHaveBeenCalled();
    expect(onChange).toHaveBeenCalledWith(['claude-sonnet-4']);
  });

  it('split 排布给出「可选模型 / 已选模型」双栏（一键接入形态）', () => {
    render(
      <A2aModelsFields
        client={makeClient()}
        draft={emptyProfile()}
        update={() => undefined}
        catalog={catalog}
        disabled={false}
        quick
        layout="split"
      />,
    );
    expect(screen.getByText('可选模型')).toBeTruthy();
    expect(screen.getByText('已选模型')).toBeTruthy();
    expect(screen.getByText('网关目录共 2 个')).toBeTruthy();
  });
});
