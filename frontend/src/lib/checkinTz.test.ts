/**
 * 签到时区边界值单测（#21 补做项：本地 0 点 / UTC 0 点 / 跨日显式构造）。
 *
 * 被测：src/lib/checkinTz.ts 的 nextRunLabel / localDateKey。
 * 语义基准 = 后端 src/checkin/store.rs timezone_offset（小时；0=跟随系统本地
 * 时区）+ scheduler.rs beijing()（0 → chrono::Local，其余 UTC+固定偏移）。
 *
 * 确定性策略：
 * - 固定偏移（如 8）用 Date.UTC 构造 now，期望值全部手算——绝对确定。
 * - offset=0（跟随系统）依赖运行机器时区，无法手算固定值 → 用「镜像断言」：
 *   期望值由测试内联的同语义参考实现（本地 Date 方法）现算，两实现一致即过。
 *   这测的是「offset=0 走系统帧」这一语义本身，不锚死任何具体时区。
 */
import { describe, expect, it } from 'vitest';
import { localDateKey, nextRunLabel } from './checkinTz';

describe('nextRunLabel — UTC+8（缺省档）日切边界', () => {
  // 北京 2025-03-10 23:59 = UTC 15:59Z；北京 3-11 00:00 = UTC 16:00Z
  const before0 = new Date('2025-03-10T15:59:00Z');
  const at0 = new Date('2025-03-10T16:00:00Z');

  it('23:59 计划在 23:59 → 今天（边界相等算今天）', () => {
    expect(nextRunLabel('23:59', 8, before0)).toBe('03-10 23:59');
  });

  it('23:45 计划、现刻 23:30（未到）→ 今天', () => {
    // 北京 2025-03-10 23:30 = UTC 15:30Z
    expect(nextRunLabel('23:45', 8, new Date('2025-03-10T15:30:00Z'))).toBe('03-10 23:45');
  });

  it('23:45 计划、现刻 23:59（已过）→ 明天 03-11', () => {
    expect(nextRunLabel('23:45', 8, before0)).toBe('03-11 23:45');
  });

  it('跨日：UTC 16:00Z（北京 0 点整）后，23:45 → 明天 03-11', () => {
    expect(nextRunLabel('23:45', 8, at0)).toBe('03-11 23:45');
  });

  it('跨日：00:00 计划在北京 0 点整 → 今天 03-11（新的一天边界内）', () => {
    expect(nextRunLabel('00:00', 8, at0)).toBe('03-11 00:00');
  });

  it('上午计划在下午现刻之前 → 明天（普通跨日）', () => {
    // 北京 3-10 20:00 = UTC 12:00Z；09:00 已过 → 明天 3-11
    expect(nextRunLabel('09:00', 8, new Date('2025-03-10T12:00:00Z'))).toBe('03-11 09:00');
  });
});

describe('nextRunLabel — UTC 0 点档（offset=0 跟随系统，镜像断言）', () => {
  /** 参考实现：系统本地帧的同语义（测试内独立写一遍，两实现一致即语义成立） */
  function refNextRun(time: string, now: Date): string {
    const pad = (n: number): string => String(n).padStart(2, '0');
    const nowHM = `${pad(now.getHours())}:${pad(now.getMinutes())}`;
    const today = `${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
    if (time >= nowHM) return `${today} ${time}`;
    const d = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1);
    return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${time}`;
  }

  it('UTC 0 点边界：跟随系统帧（与本地参考实现一致）', () => {
    // UTC 0 点附近两针（前一分钟/整点），按当前系统时区换算后行为必须与参考一致
    for (const iso of ['2025-03-10T23:59:00Z', '2025-03-11T00:00:00Z']) {
      const now = new Date(iso);
      expect(nextRunLabel('00:00', 0, now)).toBe(refNextRun('00:00', now));
      expect(nextRunLabel('12:00', 0, now)).toBe(refNextRun('12:00', now));
    }
  });

  it('offset=0 的「今天/明天」翻转点 = 系统本地 0 点（跨日语义）', () => {
    // 在系统本地时区里构造「本地 0 点整」与「本地 0 点前 1 分钟」
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    const beforeMidnight = new Date(midnight.getTime() - 60_000);
    expect(nextRunLabel('00:00', 0, midnight)).toBe(refNextRun('00:00', midnight));
    expect(nextRunLabel('00:00', 0, beforeMidnight)).toBe(refNextRun('00:00', beforeMidnight));
    // 本地 0 点整：00:00 计划应算「今天」（边界相等）；0 点前 1 分钟算「明天」
    expect(nextRunLabel('00:00', 0, midnight)).toMatch(/\d{2}-\d{2} 00:00/);
  });
});

describe('localDateKey — last_fired_date 比对键口径', () => {
  it('UTC+8：UTC 16:00Z（北京 0 点）后日期翻到 03-11', () => {
    expect(localDateKey(8, new Date('2025-03-10T15:59:00Z'))).toBe('2025-03-10');
    expect(localDateKey(8, new Date('2025-03-10T16:00:00Z'))).toBe('2025-03-11');
  });

  it('offset=0（跟随系统）与本地 Date 现算日期一致（镜像断言）', () => {
    const now = new Date();
    const ref = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
    expect(localDateKey(0, now)).toBe(ref);
  });

  it('负偏移：UTC-5（offset=-5），UTC 04:59Z 仍是前一天、05:00Z 翻日', () => {
    // UTC-5 的 0 点 = UTC 05:00Z
    expect(localDateKey(-5, new Date('2025-03-10T04:59:00Z'))).toBe('2025-03-09');
    expect(localDateKey(-5, new Date('2025-03-10T05:00:00Z'))).toBe('2025-03-10');
  });
});
