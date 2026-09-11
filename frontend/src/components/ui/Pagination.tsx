import { useTranslation } from 'react-i18next';
import Button from './Button';

export interface PaginationProps {
  page: number;
  totalPages: number;
  onChange: (page: number) => void;
  /** 最多显示多少个数字页码槽位（含省略号占位），默认 7 */
  siblingCount?: number;
}

/**
 * Pagination — 统一分页组件（数字页码 + 上一页/下一页 + 省略号）。
 *
 * - 页码 1-based；totalPages <= 1 时不渲染
 * - 当前页高亮；超过 siblingCount 时用省略号折叠中段
 * - 风格对齐 Channels/Logs 既有的 shadcn outline Button
 */
export default function Pagination({
  page,
  totalPages,
  onChange,
  siblingCount = 5,
}: PaginationProps): JSX.Element | null {
  const { t } = useTranslation();
  if (totalPages <= 1) return null;

  // 计算要显示的页码槽位：首页、末页、当前页 ± sibling，其余用 -1 占位表示省略号
  const slots: number[] = [];
  const last = totalPages;
  const left = Math.max(2, page - siblingCount);
  const right = Math.min(last - 1, page + siblingCount);

  slots.push(1);
  if (left > 2) slots.push(-1);
  for (let i = left; i <= right; i += 1) slots.push(i);
  if (right < last - 1) slots.push(-1);
  if (last > 1) slots.push(last);

  return (
    <div className="pagination-bar">
      <Button variant="outline" size="sm" disabled={page <= 1} onClick={() => onChange(page - 1)}>
        {t('上一页')}
      </Button>
      {slots.map((s, i) =>
        s === -1 ? (
          <span key={`ellipsis-${i}`} className="pagination-ellipsis">…</span>
        ) : (
          <button
            key={s}
            type="button"
            className={`pagination-page ${s === page ? 'active' : ''}`}
            onClick={() => s !== page && onChange(s)}
            disabled={s === page}
          >
            {s}
          </button>
        ),
      )}
      <Button variant="outline" size="sm" disabled={page >= totalPages} onClick={() => onChange(page + 1)}>
        {t('下一页')}
      </Button>
    </div>
  );
}