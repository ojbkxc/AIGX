import { useTranslation } from 'react-i18next';
import { ChevronLeft, ChevronRight, ChevronsLeft, ChevronsRight } from 'lucide-react';

export interface PaginationProps {
  page: number;
  totalPages: number;
  onChange: (page: number) => void;
  /**
   * 当前页左右各显示多少个数字页码（不是总槽位数）。
   * 默认 2：总槽位 = 首 + 末 + 当前 ± 2 + 最多 2 个省略号 ≈ 7~9 个按钮。
   * 原值 5 会渲染 11 个数字按钮，在表格页脚里过宽、把「每页条数/跳页」挤到换行。
   */
  siblingCount?: number;
}

/**
 * Pagination — 统一分页组件。
 *
 * - 页码 1-based；totalPages <= 1 时不渲染（父容器保证单页时仍有「共 N 条」信息）
 * - 结构：首页 · 上一页 · 数字页码 · 下一页 · 末页（首/末页按钮便于长列表跳转）
 * - 所有按钮统一 32×32、圆角 8px；当前页实心高亮；箭头用 lucide 图标（不依赖字体基线）
 * - 超过 siblingCount 时用省略号折叠中段；省略号与页码按钮等宽以保证对齐
 * - 无障碍：nav 语义 + aria-current + 每个按钮 aria-label
 */
export default function Pagination({
  page,
  totalPages,
  onChange,
  siblingCount = 2,
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

  // 统一入口：越界与「点击当前页」一律忽略，避免调用方拿到重复值
  const go = (target: number): void => {
    if (target >= 1 && target <= totalPages && target !== page) onChange(target);
  };

  return (
    <nav className="pagination-bar" aria-label={t('分页导航')}>
      <button
        type="button"
        className="pagination-item"
        aria-label={t('首页')}
        disabled={page <= 1}
        onClick={() => go(1)}
      >
        <ChevronsLeft size={16} aria-hidden="true" />
      </button>
      <button
        type="button"
        className="pagination-item"
        aria-label={t('上一页')}
        disabled={page <= 1}
        onClick={() => go(page - 1)}
      >
        <ChevronLeft size={16} aria-hidden="true" />
      </button>
      {slots.map((s, i) =>
        s === -1 ? (
          <span key={`ellipsis-${i}`} className="pagination-ellipsis" aria-hidden="true">
            …
          </span>
        ) : (
          <button
            key={s}
            type="button"
            className={`pagination-item pagination-num${s === page ? ' is-active' : ''}`}
            aria-label={t('第 {{n}} 页', { n: s })}
            aria-current={s === page ? 'page' : undefined}
            onClick={() => go(s)}
          >
            {s}
          </button>
        ),
      )}
      <button
        type="button"
        className="pagination-item"
        aria-label={t('下一页')}
        disabled={page >= totalPages}
        onClick={() => go(page + 1)}
      >
        <ChevronRight size={16} aria-hidden="true" />
      </button>
      <button
        type="button"
        className="pagination-item"
        aria-label={t('末页')}
        disabled={page >= totalPages}
        onClick={() => go(totalPages)}
      >
        <ChevronsRight size={16} aria-hidden="true" />
      </button>
    </nav>
  );
}
