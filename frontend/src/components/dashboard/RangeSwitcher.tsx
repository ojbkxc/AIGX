// 报表时间档（A2A 六档含 custom）：前五档 = 含今天 N 个自然日；
// custom 档给起止两个原生日期输入（交叉自动拉齐另一端、禁未来日期），
// 档位与自定义窗口分别持久化 localStorage（改区间不动档位，反之亦然）。
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { ReportWindow } from '../../api';
import {
  DAY_KEY_RE,
  dayKey,
  readCustomRange,
  readRange,
  saveCustomRange,
  saveRange,
  type CustomRange,
} from '../../lib/format';

export const RANGE_OPTIONS = ['1', '7', '30', '90', '365', 'custom'] as const;
export type RangeKey = (typeof RANGE_OPTIONS)[number];
export const DEFAULT_RANGE: RangeKey = '30';

const LABEL_KEYS: Record<string, string> = {
  '1': '今日',
  '7': '近 7 日',
  '30': '近 30 日',
  '90': '近 90 日',
  '365': '近 365 日',
  custom: '自定义',
};

/** range → 后端窗口参数（custom 档消费 cur 的起止） */
export function windowOf(key: RangeKey, custom: CustomRange): ReportWindow {
  if (key === 'custom') {
    return DAY_KEY_RE.test(custom.start) && DAY_KEY_RE.test(custom.end)
      ? { start: custom.start, end: custom.end }
      : { days: 7 };
  }
  return { days: Number(key) };
}

/** range → 展示标签（标题插值用；custom 显示实际起止） */
export function rangeLabel(key: RangeKey, custom: CustomRange): string {
  return key === 'custom' ? `${custom.start} ~ ${custom.end}` : LABEL_KEYS[key];
}

export default function RangeSwitcher({
  range,
  custom,
  onChange,
  onCustomChange,
}: {
  range: RangeKey;
  custom: CustomRange;
  onChange: (r: RangeKey) => void;
  onCustomChange: (c: CustomRange) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  useEffect(() => setOpen(range === 'custom'), [range]);

  const today = dayKey(new Date());
  const pick = (k: RangeKey): void => {
    saveRange(k);
    onChange(k);
  };

  // 交叉拉齐：改起日超尾日 → 尾日起 = 起日；反之亦然；禁未来
  const setStart = (v: string): void => {
    if (!v) return;
    const start = v > today ? today : v;
    const next = { start, end: custom.end < start ? start : custom.end };
    saveCustomRange(next);
    onCustomChange(next);
  };
  const setEnd = (v: string): void => {
    if (!v) return;
    const end = v > today ? today : v;
    const next = { start: custom.start > end ? end : custom.start, end };
    saveCustomRange(next);
    onCustomChange(next);
  };

  return (
    <div className="range-switcher" role="tablist" aria-label={t('时间范围')}>
      {RANGE_OPTIONS.map((k) => (
        <button
          key={k}
          role="tab"
          aria-selected={range === k}
          className={`range-switcher-btn${range === k ? ' is-active' : ''}`}
          onClick={() => pick(k)}
        >
          {t(LABEL_KEYS[k])}
        </button>
      ))}
      {open ? (
        <span className="range-custom-inputs" style={{ display: 'inline-flex', gap: 6, alignItems: 'center', marginLeft: 8 }}>
          <input type="date" max={today} value={custom.start} onChange={(e) => setStart(e.target.value)} aria-label={t('开始日期')} />
          <span style={{ color: 'var(--text-muted)' }}>~</span>
          <input type="date" max={today} value={custom.end} onChange={(e) => setEnd(e.target.value)} aria-label={t('结束日期')} />
        </span>
      ) : null}
    </div>
  );
}

/** 初始档位：localStorage 合法值优先，否则默认（近 30 日） */
export function initialRange(): RangeKey {
  const saved = readRange(DEFAULT_RANGE, RANGE_OPTIONS, DEFAULT_RANGE) as RangeKey;
  return saved;
}

export { readCustomRange };
