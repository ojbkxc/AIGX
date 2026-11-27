/**
 * 「运行信息」侧栏（`agent-ops-panel.tsx`）回归。
 *
 * 钉四件事——都是**行为面**，不看内部实现：
 *   1. 三段取数各自独立：`/api/purge` 挂了只是「网关状态」空态，渠道与最近请求照常渲染
 *      （此前没有这块面板，是因为页面右栏被 A2A 客户端检测占着；换成运维面之后，
 *      最容易被写成「一个 catch 全清空」的形态，这条防的就是那个回归）。
 *   2. 最近错误从**同一批**最近请求里筛：非 2xx 或带 error_msg 才算，2xx 干净行不许误报。
 *   3. 三段全挂 → 显式「加载失败 + 重试」，而不是静默空态（静默空态会被读成「一切正常」）。
 *   4. 缺字段按「未知」渲染：`inflight` 不下发时显示 `—`，**不能**补 0 冒充「当前无在途」。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';

// i18n：把 key 原样返回，断言的是 key（与 tool-call-card.test.tsx 同口径）
vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

const getPurge = vi.fn();
const listChannels = vi.fn();
const listRequestLogs = vi.fn();

vi.mock('../api', () => ({
  api: {
    getPurge: (...a: unknown[]) => getPurge(...a),
    listChannels: (...a: unknown[]) => listChannels(...a),
    listRequestLogs: (...a: unknown[]) => listRequestLogs(...a),
  },
}));

import AgentOpsPanel, { fmtStamp, humanUptime, toRecentRows } from './agent-ops-panel';

const ok = <T,>(data: T) => Promise.resolve({ success: true, data });

function purgeData(patch: Record<string, unknown> = {}) {
  return {
    runtime: { pid: 1, uptime_secs: 90_061, upstream: '本地上游', inflight: 3, ...patch },
    stats_24h: { responses: 42 },
  };
}

function logRow(patch: Record<string, unknown> = {}) {
  return {
    id: 'log-1',
    created_at: 1_700_000_000,
    model: 'gpt-4o',
    status_code: 200,
    latency_ms: 120,
    error_msg: null,
    ...patch,
  };
}

afterEach(() => {
  cleanup();
  getPurge.mockReset();
  listChannels.mockReset();
  listRequestLogs.mockReset();
});

describe('运行信息侧栏 — 三段取数各自独立', () => {
  it('网关状态取数失败时，供应商与最近请求照常渲染（不整栏清空）', async () => {
    getPurge.mockReturnValue(Promise.reject(new Error('boom')));
    listChannels.mockReturnValue(ok([{ status: 'enabled' }, { status: 'disabled' }, { status: 'enabled' }]));
    listRequestLogs.mockReturnValue(ok([logRow()]));

    render(<AgentOpsPanel />);

    // 供应商：3 家里 2 家启用
    await waitFor(() => expect(screen.getByText('2')).toBeTruthy());
    // 最近请求那一行在
    expect(screen.getByText('gpt-4o')).toBeTruthy();
    // 网关状态段是空态（且**没有**整栏「加载失败」——另两段成功了）
    expect(screen.getAllByText('暂无数据').length).toBe(1);
    expect(screen.queryByText('加载失败')).toBeNull();
  });

  it('三段全失败 → 显式「加载失败」+ 重试（不是静默空态）', async () => {
    getPurge.mockReturnValue(Promise.reject(new Error('a')));
    listChannels.mockReturnValue(Promise.reject(new Error('b')));
    listRequestLogs.mockReturnValue(Promise.reject(new Error('c')));

    render(<AgentOpsPanel />);

    await waitFor(() => expect(screen.getByText('加载失败')).toBeTruthy());
    expect(screen.getByText('重试')).toBeTruthy();
  });
});

describe('运行信息侧栏 — 最近错误从同一批里筛', () => {
  it('2xx 干净行不误报；非 2xx 与带 error_msg 的才进「最近错误」', async () => {
    getPurge.mockReturnValue(ok(purgeData()));
    listChannels.mockReturnValue(ok([{ status: 'enabled' }]));
    listRequestLogs.mockReturnValue(ok([
      logRow({ id: 'a', status_code: 500, error_msg: '上游 500' }),
      logRow({ id: 'b', status_code: 200, error_msg: null }),
      logRow({ id: 'c', status_code: 200, error_msg: '上游拒答' }),
    ]));

    render(<AgentOpsPanel />);

    await waitFor(() => expect(screen.getByText('最近错误')).toBeTruthy());
    // 两条错：500 那条的 error_msg 与「2xx 但带错误文本」那条
    expect(screen.getByText('上游 500')).toBeTruthy();
    expect(screen.getByText('上游拒答')).toBeTruthy();
    // 干净那条的正文不会出现在错误区（正文也不进列表，只显示模型+状态码+耗时）
    expect(screen.queryAllByText('正常')).toHaveLength(0);
  });

  it('没有任何错误行时，「最近错误」显式显示「正常」', async () => {
    getPurge.mockReturnValue(ok(purgeData()));
    listChannels.mockReturnValue(ok([{ status: 'enabled' }]));
    listRequestLogs.mockReturnValue(ok([logRow()]));

    render(<AgentOpsPanel />);

    await waitFor(() => expect(screen.getByText('正常')).toBeTruthy());
  });
});

describe('运行信息侧栏 — 缺字段按未知渲染', () => {
  it('后端不下发 inflight 时显示「—」，不补 0', async () => {
    getPurge.mockReturnValue(ok(purgeData({ inflight: undefined })));
    listChannels.mockReturnValue(ok([]));
    listRequestLogs.mockReturnValue(ok([]));

    render(<AgentOpsPanel />);

    await waitFor(() => expect(screen.getByText('在途请求')).toBeTruthy());
    // 关键：不许补 0 冒充「当前无在途」——那会被读成真实测得的 0
    const inflightRow = screen.getByText('在途请求').parentElement;
    expect(inflightRow?.querySelector('dd')?.textContent).toBe('—');
  });

  it('刷新按钮会重新取数一次（不是只渲染时取一次）', async () => {
    getPurge.mockReturnValue(ok(purgeData()));
    listChannels.mockReturnValue(ok([{ status: 'enabled' }]));
    listRequestLogs.mockReturnValue(ok([logRow()]));

    render(<AgentOpsPanel />);
    await waitFor(() => expect(getPurge).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole('button', { name: '刷新' }));

    await waitFor(() => expect(getPurge).toHaveBeenCalledTimes(2));
  });
});

describe('运行信息侧栏 — 纯函数取值表', () => {
  it('humanUptime：天/小时/分钟/秒四档与未知', () => {
    const t = (k: string) => k;
    expect(humanUptime(null, t)).toBe('—');
    expect(humanUptime(Number.NaN, t)).toBe('—');
    expect(humanUptime(45, t)).toBe('45 秒');
    expect(humanUptime(125, t)).toBe('2 分钟');
    expect(humanUptime(3_661, t)).toBe('1 小时 1 分钟');
    expect(humanUptime(90_061, t)).toBe('1 天 1 小时');
  });

  it('toRecentRows：缺 created_at 的行丢弃（排不出先后就不排），并截到 5 条', () => {
    const rows = toRecentRows(
      Array.from({ length: 8 }, (_v, i) => logRow({ id: `r${i}`, created_at: 1_700_000_000 + i })),
    );
    expect(rows).toHaveLength(5);
    expect(rows[0].id).toBe('r0');

    const withBad = toRecentRows([
      logRow({ id: 'ok' }),
      { id: 'no-ts', model: 'x' },
      { id: 'zero-ts', created_at: 0 },
      null,
      'garbage',
    ]);
    expect(withBad.map((r) => r.id)).toEqual(['ok']);
  });

  it('fmtStamp：unix 秒按本地时区格式化，非法值给占位', () => {
    const d = new Date(1_700_000_000 * 1000);
    const p = (n: number) => String(n).padStart(2, '0');
    expect(fmtStamp(1_700_000_000)).toBe(
      `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`,
    );
    expect(fmtStamp(Number.NaN)).toBe('—');
  });
});