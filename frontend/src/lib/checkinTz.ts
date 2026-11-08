/**
 * 签到时区工具（纯函数，无依赖——便于边界值单测）。
 *
 * **时区语义与后端严格对齐**（src/checkin/store.rs:120-152 CheckinSettings
 * .timezone_offset + src/checkin/scheduler.rs beijing()）：
 * - `timezone_offset` 单位是**小时**（i32，缺省 8 = UTC+8）；
 * - **0 = 跟随系统本地时区**（后端 beijing() 仅在 0 时用 chrono::Local）；
 * - 其余按「UTC + 固定偏移」计算（非 IANA 名，直接做 UTC 数学，避免
 *   Etc/GMT 正负号陷阱）。
 *
 * 展示口径（下次执行 / 今日已执行徽标）必须与后端执行口径一致——
 * 此前硬编码 Asia/Shanghai 导致 offset=0 时展示与执行脱钩（#21 补做项）。
 */

/** 单位偏移的小时数（缺省 8 = UTC+8；0 = 跟随系统本地时区） */
export type TzOffsetHours = number;

const pad2 = (n: number): string => String(n).padStart(2, '0');

/**
 * 把 now 投影到「配置时区」的墙上钟帧。
 * offset=0 → 系统 Date（本地帧）；否则 Date + offset 小时后取 UTC 字段。
 */
function wallClock(now: Date, offset: TzOffsetHours): {
  year: number; month: number; day: number; hour: number; minute: number;
} {
  if (offset === 0) {
    // 系统本地帧（与后端 chrono::Local 同语义）
    return {
      year: now.getFullYear(), month: now.getMonth() + 1, day: now.getDate(),
      hour: now.getHours(), minute: now.getMinutes(),
    };
  }
  const shifted = new Date(now.getTime() + offset * 3_600_000);
  return {
    year: shifted.getUTCFullYear(), month: shifted.getUTCMonth() + 1, day: shifted.getUTCDate(),
    hour: shifted.getUTCHours(), minute: shifted.getUTCMinutes(),
  };
}

/**
 * 「下次执行」展示：MM-DD HH:mm（配置时区）。
 * 当天该时刻**尚未到**（time >= 当前 HH:mm，字符串比较即字典序，HH:MM 定宽
 * 两位下与数值比较等价）→ 今天；否则明天（跨日滚动，含「刚好相等算今天」）。
 */
export function nextRunLabel(time: string, offset: TzOffsetHours, now: Date = new Date()): string {
  const w = wallClock(now, offset);
  const nowHM = `${pad2(w.hour)}:${pad2(w.minute)}`;
  const today = `${pad2(w.month)}-${pad2(w.day)}`;
  if (time >= nowHM) return `${today} ${time}`;
  // 明天：在墙钟帧上加一天再取字段（对 offset!==0 用 UTC 数学；offset=0 用本地加法）
  let tomorrow: { month: number; day: number };
  if (offset === 0) {
    const d = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1);
    tomorrow = { month: d.getMonth() + 1, day: d.getDate() };
  } else {
    const shifted = new Date(now.getTime() + offset * 3_600_000 + 86_400_000);
    tomorrow = { month: shifted.getUTCMonth() + 1, day: shifted.getUTCDate() };
  }
  return `${pad2(tomorrow.month)}-${pad2(tomorrow.day)} ${time}`;
}

/** 「今日已执行」比对键：配置时区的 YYYY-MM-DD（与后端 last_fired_date 同口径） */
export function localDateKey(offset: TzOffsetHours, now: Date = new Date()): string {
  const w = wallClock(now, offset);
  return `${w.year}-${pad2(w.month)}-${pad2(w.day)}`;
}
