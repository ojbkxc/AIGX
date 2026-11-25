/**
 * 「定时补签」视图的**条目快照同步**回归。
 *
 * 钉住一件事：后端快照刷新（保存后 `load()`、别处改了同一计划）后，补签编辑器里
 * 未保存的草稿要跟着换成新值。旧实现用 `useState(entry.retryTime)` 取初值，
 * 挂载后永不更新 —— 保存成功、快照已换新，输入框还停在改前的时刻与间隔，
 * 看起来像「保存没生效」。
 *
 * 编辑中**不**覆盖（避免冲掉正在输入的值），所以用例先把编辑器关掉再刷新快照。
 */
import { describe, expect, it, vi, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/react';
import type { CheckinAutoState, CheckinScheduleEntry } from '../types';

vi.mock('../components/Toast', () => ({
  useToast: () => (): void => undefined,
  ToastProvider: ({ children }: { children: React.ReactNode }) => children,
}));

import CheckinMakeupView from './checkin-makeup';

function entry(overrides: Partial<CheckinScheduleEntry> = {}): CheckinScheduleEntry {
  return {
    id: 's1',
    label: '每日签到',
    displayLabel: '每日签到',
    enabled: true,
    time: '09:00',
    providers: ['codearts'],
    providerTimes: {},
    providerRows: [],
    retryTime: '00:10',
    retryGapMinutes: 10,
    retryLimit: 3,
    lastFiredDate: null,
    lastFiredToday: false,
    pendingProviders: [],
    retryCount: 0,
    retryExhausted: [],
    nextRunAt: null,
    lastResult: null,
    ...overrides,
  };
}

function autoWith(target: CheckinScheduleEntry): CheckinAutoState {
  return {
    enabled: true,
    running: false,
    providerOptions: [],
    schedules: [target],
  } as CheckinAutoState;
}

function view(target: CheckinScheduleEntry): JSX.Element {
  return (
    <CheckinMakeupView
      auto={autoWith(target)}
      saving={false}
      defaultRetryTime="00:10"
      onToggleMakeup={() => undefined}
      onSaveMakeup={() => undefined}
      onSaveRetry={() => undefined}
      onEditSchedule={() => undefined}
      onGoSchedule={() => undefined}
    />
  );
}

describe('定时补签 · 条目快照同步', () => {
  afterEach(() => cleanup());

  it('快照刷新后，未在编辑中的补签时刻与间隔跟随新值', () => {
    const first = entry({ retryTime: '00:10', retryGapMinutes: 10 });
    const { rerender } = render(view(first));

    // 打开再关掉编辑器（模拟用户看过一眼但没有保存）
    fireEvent.click(screen.getByText('设置补签'));
    expect((screen.getByLabelText('补签不早于') as HTMLInputElement).value).toBe('00:10');
    fireEvent.click(screen.getByText('设置补签'));

    // 后端快照换成新值（保存成功 / 别处改过同一计划）
    rerender(view(entry({ retryTime: '02:30', retryGapMinutes: 30 })));

    fireEvent.click(screen.getByText('设置补签'));
    // 旧实现这里仍是 '00:10' / '10'：useState 初值只在挂载时求值
    expect((screen.getByLabelText('补签不早于') as HTMLInputElement).value).toBe('02:30');
    expect((screen.getByLabelText('区间结束后等待（分钟）') as HTMLInputElement).value).toBe('30');
  });

  it('快照刷新后，活动窗口重试的间隔与次数跟随新值', () => {
    const first = entry({ retryGapMinutes: 10, retryLimit: 3 });
    const { rerender } = render(view(first));

    // 打开再关掉「调整重试」（MakeupPlan 与 ActivityRetryPlan 各有一颗「设置补签/调整重试」）
    fireEvent.click(screen.getByText('调整重试'));
    expect((screen.getByLabelText('每账号每天最多重试（次）') as HTMLInputElement).value).toBe('3');
    fireEvent.click(screen.getByText('调整重试'));

    rerender(view(entry({ retryGapMinutes: 30, retryLimit: 7 })));

    fireEvent.click(screen.getByText('调整重试'));
    expect((screen.getByLabelText('重试间隔（分钟）') as HTMLInputElement).value).toBe('30');
    expect((screen.getByLabelText('每账号每天最多重试（次）') as HTMLInputElement).value).toBe('7');
  });
});
