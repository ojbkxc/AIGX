// 报表 Top 榜共用件（对齐 A2A report/rank-panel.tsx:7-28 的「前 N 名 / 全部」
// 选择器，以及 :44-47 的榜尾「前 N 名占全部用量 X%」合计行）。
//
// 与 A2A 的两处刻意差异 —— 都是 AIGX 数据面逼出来的，不是省事：
//
// 1) 选项只给 前 5 / 前 10 / 全部。A2A 的 COUNTS 是 [5, 10, 20, 0]，但 AIGX 的
//    /report/top_sources 与 /report/top_clients 后端都 `truncate(10)`
//    （src/api/admin/dashboard.rs handle_report_top_sources、client_id.rs
//    handle_report_top_clients），第 11 行起根本不存在。给「前 20」会得到一个
//    和「全部」逐行相同的榜 —— 那是个骗人的选项。
//
// 2) 占比分母优先用窗口概览总量（overview 的 total_requests / tokens），拿不到
//    才退回「榜内合计」。后端截断 10 行后，榜内合计天然小于窗口总量；拿榜内合计
//    当分母，会把「前 5 名占全部请求 62%」显示成「占 100%」—— 数字看着更漂亮，
//    含义正好反了。
import { useTranslation } from 'react-i18next';
import './report-panels.css';

/** 0 = 全部（= 后端返回的全部行，当前上限 10） */
export const RANK_COUNTS = [5, 10, 0] as const;
export type RankCount = (typeof RANK_COUNTS)[number];

/** 截断到前 N 名（count=0 表示不截断） */
export function rankSlice<T>(rows: T[], count: RankCount): T[] {
  return count > 0 ? rows.slice(0, count) : rows;
}

/**
 * 前 N 名占全量的百分比（0-100，夹到 100 以内）。
 * total <= 0（概览没回来 / 该维度无数据）时退回 rowsTotal 兜底，两处都空返回 0。
 */
export function rankShare(shownSum: number, total: number, rowsTotal: number): number {
  const denom = total > 0 ? total : rowsTotal;
  if (!(denom > 0) || !(shownSum > 0)) return 0;
  return Math.min(100, (shownSum / denom) * 100);
}

/** 前 N 名 / 全部（分段控件，与报表页 .seg 同一套视觉） */
export function RankCountControl({
  count,
  onChange,
  label,
}: {
  count: RankCount;
  onChange: (next: RankCount) => void;
  /** 无障碍名（不同榜的语义不同：「供应商显示数量」/「客户端显示数量」） */
  label: string;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="seg" role="group" aria-label={label}>
      {RANK_COUNTS.map((value) => (
        <button
          key={value}
          type="button"
          className={`seg-btn${count === value ? ' is-active' : ''}`}
          aria-pressed={count === value}
          onClick={() => onChange(value)}
        >
          {value === 0 ? t('全部') : t('前 {{n}} 名', { n: value })}
        </button>
      ))}
    </div>
  );
}

/**
 * 榜尾合计行：左侧「前 N 名占全部{{metric}} X%」，右侧该榜读数（调用方给文案，
 * 与 A2A rank-panel 的 `rankTotalText(rows)` 同位）。
 */
export function RankShareFooter({
  count,
  share,
  metric,
  totalText,
}: {
  count: number;
  share: number;
  /** 口径词（请求数 / Token），与榜内占比列同一口径 */
  metric: string;
  totalText: string;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="rank-footer">
      <span>{t('前 {{n}} 名占全部{{metric}} {{pct}}', { n: count, metric, pct: `${share.toFixed(1)}%` })}</span>
      <span className="rank-footer-total">{totalText}</span>
    </div>
  );
}
