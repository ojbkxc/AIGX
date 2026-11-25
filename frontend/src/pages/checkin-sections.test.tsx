/**
 * 签到中心四个分区（checkin-sections）的**并发加载态**回归。
 *
 * 钉住两件事（都是旧单值 `running: string` 实现做不到的）：
 * 1. 两个动作同时在跑时，先返回的那个不能把另一个的加载态清掉
 *    （旧实现 `finally` 无条件 `setRunning('')`，把别人那条一起清了）；
 * 2. 一个动作在跑**不阻止**另一个动作被点击
 *    （旧实现 `disabled={running !== ''}`：任何一颗在跑就把整片按钮全禁了）。
 *
 * 判据只看「这颗按钮自己」：文案是否还停在「查询中…」、是否仍 disabled。
 * i18n 不初始化：`t()` 对未注册 key 原样返回中文，断言直接比中文字面量。
 */
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, cleanup, waitFor, fireEvent } from '@testing-library/react';
import type { ReactElement } from 'react';
import type { BenefitResult, CheckinCenterData } from '../types';

/** 每个 (accountId, action) 一个可控 deferred，测试自己决定谁先回 */
type Deferred = {
  resolve: (value: { data: BenefitResult }) => void;
  reject: (error: unknown) => void;
};
const pending = vi.hoisted(() => new Map<string, Deferred>());

const apiMock = vi.hoisted(() => ({
  runCheckinBenefit: vi.fn(
    (accountId: string, action: string) =>
      new Promise((resolve, reject) => {
        pending.set(`${accountId}:${action}`, {
          resolve: resolve as (value: { data: BenefitResult }) => void,
          reject,
        });
      }),
  ),
}));

vi.mock('../api', () => ({ api: apiMock }));
vi.mock('../components/Toast', () => ({
  useToast: () => (): void => undefined,
  ToastProvider: ({ children }: { children: ReactElement }) => children,
}));

import CheckinCenterSections from './checkin-sections';

/** 两行新手福利（不同账号），其余字段给最小可用形状 */
function center(): CheckinCenterData {
  return {
    daily: { providers: [], outOfScope: [], todayDone: 0, todayEligible: 0 },
    extras: {
      onboarding: [
        { id: 'acc-a', name: '账号 A', provider: 'codearts', pending: 1 },
        { id: 'acc-b', name: '账号 B', provider: 'loomy', pending: 1 },
      ],
      plans: [],
      campaigns: [],
      growth: [],
    },
    auto: { enabled: true, running: false, providerOptions: [], schedules: [] },
    keepalive: { models: [], default_models: [] },
    history: [],
    adminPanels: [],
    checkinFeatures: {},
    settings: { enabled: true, gap_ms: 800 },
  } as CheckinCenterData;
}

/** 取某个账号行里的「查询任务 / 查询中…」按钮（同一行还有「一键领取」） */
function queryButton(accountName: string): HTMLButtonElement {
  const row = screen.getByText(accountName).closest('.ck-extra-row');
  if (!row) throw new Error(`未找到账号行：${accountName}`);
  const button = Array.from(row.querySelectorAll('button')).find((btn) =>
    btn.textContent === '查询任务' || btn.textContent === '查询中…',
  );
  if (!button) throw new Error(`账号行里没有「查询任务」按钮：${accountName}`);
  return button as HTMLButtonElement;
}

describe('签到中心分区 · 并发加载态', () => {
  beforeEach(() => {
    pending.clear();
    apiMock.runCheckinBenefit.mockClear();
  });
  afterEach(() => cleanup());

  it('一个动作在跑，不阻止另一个动作被点击（禁用只认自己那条）', async () => {
    render(
      <CheckinCenterSections
        section="onboarding"
        center={center()}
        busyKey={null}
        onSignOne={() => undefined}
        onRunAll={() => undefined}
        onOpenBenefits={() => undefined}
        onReload={() => undefined}
      />,
    );

    // A 行开始跑，且卡住不回
    fireEvent.click(queryButton('账号 A'));
    await waitFor(() => expect(queryButton('账号 A').textContent).toBe('查询中…'));
    expect(queryButton('账号 A').disabled).toBe(true);

    // B 行必须仍然可点：旧实现这里是 disabled（running !== ''）
    expect(queryButton('账号 B').textContent).toBe('查询任务');
    expect(queryButton('账号 B').disabled).toBe(false);
  });

  it('两个动作同时在跑，先返回的那个不能把另一个的加载态清掉', async () => {
    render(
      <CheckinCenterSections
        section="onboarding"
        center={center()}
        busyKey={null}
        onSignOne={() => undefined}
        onRunAll={() => undefined}
        onOpenBenefits={() => undefined}
        onReload={() => undefined}
      />,
    );

    fireEvent.click(queryButton('账号 A'));
    fireEvent.click(queryButton('账号 B'));
    await waitFor(() => {
      expect(queryButton('账号 A').textContent).toBe('查询中…');
      expect(queryButton('账号 B').textContent).toBe('查询中…');
    });

    // 只让 A 先回来
    pending.get('acc-a:onboarding')?.resolve({
      data: { ok: true, outcome: 'done', message: '已领取' },
    });
    await waitFor(() => {
      expect(queryButton('账号 A').textContent).toBe('查询任务');
    });
    expect(queryButton('账号 A').disabled).toBe(false);

    // B 仍在跑：旧实现在这里被 A 的 finally 清成了「查询任务」+ 可点
    expect(queryButton('账号 B').textContent).toBe('查询中…');
    expect(queryButton('账号 B').disabled).toBe(true);

    // B 自己回来后也该收敛
    pending.get('acc-b:onboarding')?.resolve({
      data: { ok: true, outcome: 'already', message: '已结清' },
    });
    await waitFor(() => expect(queryButton('账号 B').textContent).toBe('查询任务'));
  });

  it('「全部查询」整批共用一条忙碌键，逐条同时跑且不互相清态', async () => {
    const { container } = render(
      <CheckinCenterSections
        section="onboarding"
        center={center()}
        busyKey={null}
        onSignOne={() => undefined}
        onRunAll={() => undefined}
        onOpenBenefits={() => undefined}
        onReload={() => undefined}
      />,
    );

    /** 卡片头 `.ck-actions` 里的批量按钮（同一片还有状态筛选，按文案取） */
    const bulkButton = (): HTMLButtonElement => {
      const button = Array.from(container.querySelectorAll('.ck-actions button')).find(
        (btn) => btn.textContent === '全部查询' || btn.textContent === '查询中…',
      );
      if (!button) throw new Error('未找到「全部查询」按钮');
      return button as HTMLButtonElement;
    };

    fireEvent.click(bulkButton());
    await waitFor(() => {
      expect(queryButton('账号 A').textContent).toBe('查询中…');
      expect(queryButton('账号 B').textContent).toBe('查询中…');
    });
    // 批量按钮自身进忙碌态（文案换成「查询中…」且禁用）
    expect(bulkButton().textContent).toBe('查询中…');
    expect(bulkButton().disabled).toBe(true);

    // 一条先回，其余仍在跑
    pending.get('acc-a:onboarding')?.resolve({
      data: { ok: true, outcome: 'done', message: '已领取' },
    });
    await waitFor(() => expect(queryButton('账号 A').textContent).toBe('查询任务'));
    expect(queryButton('账号 B').textContent).toBe('查询中…');
    // 整批没跑完，批量按钮不能提前解锁
    expect(bulkButton().disabled).toBe(true);

    pending.get('acc-b:onboarding')?.resolve({
      data: { ok: true, outcome: 'already', message: '已结清' },
    });
    await waitFor(() => expect(queryButton('账号 B').textContent).toBe('查询任务'));
    await waitFor(() => expect(bulkButton().textContent).toBe('全部查询'));
    expect(bulkButton().disabled).toBe(false);
  });
});
