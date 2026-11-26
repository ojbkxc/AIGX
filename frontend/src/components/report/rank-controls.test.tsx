import { describe, expect, it, beforeEach } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import i18n from 'i18next';
import { RankCountControl, RankShareFooter, rankShare, rankSlice } from './rank-controls';
import { drillRangeOf, logsDrilldownPath, primeLogsFilters } from './logs-drilldown';
import '../../i18n';

beforeEach(async () => {
  localStorage.clear();
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
});

describe('rankShare — 前 N 名占比（A2A rank-panel 的 share 计算）', () => {
  it('分母优先取窗口总量：榜被后端截断时占比不会被算成 100%', () => {
    // 榜内合计 100，但窗口总量 400 → 前 2 名（70）占 17.5%，不是 70%
    expect(rankShare(70, 400, 100)).toBeCloseTo(17.5, 5);
  });

  it('拿不到窗口总量时退回榜内合计', () => {
    expect(rankShare(70, 0, 100)).toBeCloseTo(70, 5);
  });

  it('分母为零 / 读数缺失时给 0，不产生 NaN', () => {
    expect(rankShare(0, 0, 0)).toBe(0);
    expect(rankShare(10, 0, 0)).toBe(0);
  });

  it('夹到 100 以内（窗口总量与榜内合计口径错位时不显示 137%）', () => {
    expect(rankShare(100, 50, 100)).toBe(100);
  });
});

describe('rankSlice — 前 N 名 / 全部', () => {
  const rows = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
  it('0 表示全部（不截断）', () => {
    expect(rankSlice(rows, 0)).toHaveLength(11);
  });
  it('前 N 名按给定顺序截断', () => {
    expect(rankSlice(rows, 5)).toEqual([1, 2, 3, 4, 5]);
  });
  it('行数不足 N 时原样返回', () => {
    expect(rankSlice([1, 2], 5)).toHaveLength(2);
  });
});

describe('RankCountControl — 5 / 10 / 全部', () => {
  it('只给三个选项：后端两个榜都 truncate(10)，「前 20」会是假选项', () => {
    const { container } = render(<RankCountControl count={5} onChange={() => {}} label="供应商显示数量" />);
    const labels = Array.from(container.querySelectorAll('button')).map((b) => b.textContent);
    expect(labels).toEqual(['前 5 名', '前 10 名', '全部']);
  });

  it('点击回传所选档位，当前档位带 aria-pressed', () => {
    const seen: number[] = [];
    const { container } = render(<RankCountControl count={5} onChange={(v) => seen.push(v)} label="x" />);
    const buttons = Array.from(container.querySelectorAll('button'));
    expect(buttons[0].getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(buttons[2]);
    expect(seen).toEqual([0]);
  });
});

describe('RankShareFooter — 榜尾合计行', () => {
  it('文案含名次数与百分比，右侧给该榜读数', () => {
    const { container } = render(<RankShareFooter count={5} share={62.5} metric="请求" totalText="合计 1,000" />);
    expect(container.textContent).toContain('前 5 名占全部请求 62.5%');
    expect(container.textContent).toContain('合计 1,000');
  });
});

describe('logsDrilldown — 报表行 → 请求日志', () => {
  it('路径复用 Logs 页既有筛选键（client / start / end），空维度不落参数', () => {
    expect(logsDrilldownPath({ client: 'Claude Code', start: '2026-11-01', end: '2026-11-07' }))
      .toBe('/logs?client=Claude+Code&start=2026-11-01&end=2026-11-07');
    expect(logsDrilldownPath({})).toBe('/logs');
  });

  it('写进 Logs 的筛选记忆：client + 起止（datetime-local）+ range=custom，其余维度清空', () => {
    primeLogsFilters({ client: 'Cline', start: '2026-11-01', end: '2026-11-07' });
    const saved = JSON.parse(localStorage.getItem('aigx-logs-filters') || '{}');
    expect(saved).toMatchObject({
      client: 'Cline',
      start: '2026-11-01T00:00',
      end: '2026-11-07T23:59',
      range: 'custom',
      // 上一次的筛选不能继续收窄结果
      user: '', model: '', channel: '', status: '', keyword: '',
    });
  });

  it('没有时间窗时不写起止，range 回到 all', () => {
    primeLogsFilters({ client: 'Cline' });
    const saved = JSON.parse(localStorage.getItem('aigx-logs-filters') || '{}');
    expect(saved.start).toBe('');
    expect(saved.end).toBe('');
    expect(saved.range).toBe('all');
  });

  it('drillRangeOf：自定义区间原样透传，days 档按「含今天」往前数', () => {
    expect(drillRangeOf({ start: '2026-01-01', end: '2026-01-31' }))
      .toEqual({ start: '2026-01-01', end: '2026-01-31' });
    const days7 = drillRangeOf({ days: 7 });
    expect(days7.start).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    expect(days7.end).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    // 起止相差 6 天（含今天共 7 天）
    const diff = (new Date(`${days7.end}T00:00:00`).getTime() - new Date(`${days7.start}T00:00:00`).getTime()) / 86400000;
    expect(diff).toBe(6);
  });
});
