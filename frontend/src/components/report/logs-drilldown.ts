/**
 * 报表 → 请求日志的下钻（A2A report/source-stats.tsx:22-27 的 `openRequestLogs`
 * 在 AIGX 的对应物）。
 *
 * 两件事，缺一件这条链就是死的：
 *
 * 1) **路由参数**：AIGX 是 web + react-router，不是 A2A 的 Tauri 壳，所以走
 *    `/logs?client=…&start=…&end=…`（router 的 `?` 参数），不照搬 `wbApp.showPage()`。
 *    参数名逐字复用 Logs 页既有的筛选键（`client` / `model` / `channel` /
 *    `start` / `end`），不自创第二套。
 *
 * 2) **筛选记忆**：`src/pages/Logs.tsx` 目前**不从 URL 读筛选**，它启动时从
 *    localStorage 键 `aigx-logs-filters` 恢复（filterMemory）。只带 `?client=` 的话
 *    页面能打开、筛选不生效 —— 那是个看着像功能、实际什么都不做的链接。所以跳转前
 *    把同一份筛选写进那个记忆键：参数名与 Logs 的 `Filters` 类型逐键一致，跳过去
 *    就是「已按该客户端筛过」的列表。
 *
 * 记忆是「合并写回」，只写 client 会留下上次的 keyword/status 继续收窄结果；这里
 * 显式把其余维度清空，等价于「按这一个维度重来」。
 *
 * 键名与 `LS_LOGS_FILTERS`（Logs.tsx）同值：那边是页面私有常量、不可导出，改动
 * 它必须同步改这里（两处都在 `aigx-logs-filters` 字面量上）。
 */
import { setFilter } from '../../lib/filterMemory';
import { dayKey } from '../../lib/format';
import type { ReportWindow } from '../../api';

const LOGS_FILTER_KEY = 'aigx-logs-filters';

export interface LogsDrilldown {
  client?: string;
  model?: string;
  channel?: string;
  /** YYYY-MM-DD（本地自然日，闭区间） */
  start?: string;
  end?: string;
}

/** 窗口 → 本地自然日起止（days 档按「含今天」往前数，与报表口径一致） */
export function drillRangeOf(win: ReportWindow): { start?: string; end?: string } {
  if (win.start && win.end) return { start: win.start, end: win.end };
  if (typeof win.days === 'number' && win.days > 0) {
    const end = new Date();
    const start = new Date();
    start.setDate(start.getDate() - (win.days - 1));
    return { start: dayKey(start), end: dayKey(end) };
  }
  return {};
}

/** `/logs?…`（空维度不落参数，链接可分享） */
export function logsDrilldownPath(q: LogsDrilldown): string {
  const params = new URLSearchParams();
  if (q.client) params.set('client', q.client);
  if (q.model) params.set('model', q.model);
  if (q.channel) params.set('channel', q.channel);
  if (q.start) params.set('start', q.start);
  if (q.end) params.set('end', q.end);
  const qs = params.toString();
  return qs ? `/logs?${qs}` : '/logs';
}

/**
 * 把筛选写进 Logs 页的筛选记忆（跳转前调用）。
 * 日期用 datetime-local 形态（Logs 的 `start`/`end` 就是 `<input type=datetime-local>`），
 * 起日 00:00 / 止日 23:59，range 置 custom 与之配套（否则 Logs 会按档位重算起止）。
 */
export function primeLogsFilters(q: LogsDrilldown): void {
  setFilter(LOGS_FILTER_KEY, {
    user: '',
    model: q.model ?? '',
    channel: q.channel ?? '',
    status: '',
    keyword: '',
    client: q.client ?? '',
    start: q.start ? `${q.start}T00:00` : '',
    end: q.end ? `${q.end}T23:59` : '',
    range: q.start || q.end ? 'custom' : 'all',
  });
}
