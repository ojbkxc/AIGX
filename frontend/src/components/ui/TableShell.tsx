/**
 * AIGX · 表格页脚分页（客户端切片口径）——纯逻辑 + React hook。
 *
 * 对齐 Agent2API table-shell.tsx 的功能集（DOM 手法不照抄）：
 * - 档位合法校验：读回的每页条数不在当前档位表里 → 夹到 fallback（防「存了
 *   不存在的档、表却按它分页」）
 * - 每页条数按表各自记住（localStorage 键带页面域）
 * - 越界自动夹回：total 变小后当前页超出总页数 → 夹回最后一页
 * - 旧键一次性搬迁：页面各自的旧分页键（aigx-mm-pagesize 等）迁到统一键，
 *   搬完删旧键，用户记忆不丢
 *
 * 性能：读走 lazy init；写只在用户改档位时发生；本模块不在 render 内
 * 重复 JSON.parse（档位表是常量数组，legacy 搬迁只在首次 init 扫一次）。
 */
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import Pagination from './Pagination';

/** 客户端切片默认档位（对齐上游 CLIENT_PAGE_SIZES；'all' = 全部） */
export const CLIENT_PAGE_SIZES = [20, 50, 100, 200, 'all'] as const;
/** 服务端分页档位（无「全部」档） */
export const SERVER_PAGE_SIZES = [20, 50, 100, 200] as const;

export type PageSize = number | 'all';

const isPageSize = (v: unknown, sizes: readonly PageSize[]): v is PageSize =>
  sizes.some((s) => s === v);

function lsGet(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function lsSet(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // 存储不可用：静默（只影响下次打开）
  }
}

function lsRemove(key: string): void {
  try {
    localStorage.removeItem(key);
  } catch {
    // 静默
  }
}

/**
 * 每页条数 hook（档位校验 + 旧键一次性搬迁 + 持久化）。
 *
 * @param key 统一存储键（如 'aigx-tbl-pagesize.models'）
 * @param sizes 档位表（CLIENT_PAGE_SIZES / SERVER_PAGE_SIZES 或自定义）
 * @param fallback 存储缺失/脏值时的档位（默认 50）
 * @param legacyKeys 旧分页键列表（按优先级；首次读时搬迁到 key 并删旧键）
 */
export function usePageSize(
  key: string,
  sizes: readonly PageSize[] = CLIENT_PAGE_SIZES,
  fallback: PageSize = 50,
  legacyKeys: string[] = [],
): [PageSize, (n: PageSize) => void] {
  const [size, setSize] = useState<PageSize>(() => {
    // 旧键一次性搬迁：只在存储缺失时做，搬完删旧键（旧版本回滚会重读默认，可接受）
    if (lsGet(key) === null && legacyKeys.length) {
      for (const lk of legacyKeys) {
        const raw = lsGet(lk);
        if (raw === null) continue;
        const n = Number(raw);
        if (Number.isFinite(n) && isPageSize(n, sizes)) {
          lsSet(key, String(n));
        }
        lsRemove(lk);
        break; // 只认第一个有值的旧键
      }
    }
    const raw = lsGet(key);
    if (raw === null) return fallback;
    if (raw === 'all' && isPageSize('all', sizes)) return 'all';
    const n = Number(raw);
    return Number.isFinite(n) && isPageSize(n, sizes) ? n : fallback;
  });

  const set = (n: PageSize): void => {
    if (!isPageSize(n, sizes)) return;
    setSize(n);
    lsSet(key, String(n));
  };

  return [size, set];
}

/** 客户端切片：数据一次取回，这里只切窗口。 */
export function useClientPaging<T>(
  rows: T[],
  size: PageSize,
): {
  /** 切片后的当前页行 */
  pageRows: T[];
  /** 总页数（'all' 恒 1） */
  totalPages: number;
  /** 当前页（越界自动夹回） */
  page: number;
  setPage: (p: number) => void;
} {
  const totalPages = size === 'all' ? 1 : Math.max(1, Math.ceil(rows.length / size));
  const [rawPage, setRawPage] = useState(1);
  // total 变化导致越界 → 夹回最后一页（render 期用派生值，不在 effect 里 setState 抖动）
  const page = Math.min(Math.max(1, rawPage), totalPages);
  const pageRows = useMemo(
    () => (size === 'all' ? rows : rows.slice((page - 1) * size, page * size)),
    [rows, page, size],
  );
  useEffect(() => {
    // 数据域切换后页码复位由调用方决定；这里只保证 rawPage 越界时收敛
    if (rawPage !== page) setRawPage(page);
  }, [rawPage, page]);
  return { pageRows, totalPages, page, setPage: setRawPage };
}

/**
 * 表格页脚：总数/区间 + 每页档位 + 数字分页 + 跳页框（常驻，一页时禁用）。
 * 对齐上游 table-shell.tsx 页脚功能集；样式挂 `.tbl-foot` 体系（见 AccountSessions.css 同款类）。
 */
export function TablePaginationFooter({
  total,
  page,
  totalPages,
  size,
  sizes = CLIENT_PAGE_SIZES,
  onSizeChange,
  onPageChange,
  ariaLabelSize = '每页条数',
}: {
  total: number;
  page: number;
  totalPages: number;
  size: PageSize;
  sizes?: readonly PageSize[];
  onSizeChange: (n: PageSize) => void;
  onPageChange: (p: number) => void;
  ariaLabelSize?: string;
}): JSX.Element {
  const { t } = useTranslation();
  const [jump, setJump] = useState('');
  const submitJump = (): void => {
    const n = Number(jump);
    if (Number.isFinite(n) && n >= 1 && n <= totalPages) onPageChange(Math.floor(n));
    setJump('');
  };
  const from = size === 'all' ? (total > 0 ? 1 : 0) : (page - 1) * size + 1;
  const to = size === 'all' ? total : Math.min(page * size, total);
  return (
    <div className="tbl-foot">
      <span className="tbl-foot-info">
        {t('共')} {total} {t('条')}
        {total > 0 && ` · ${t('当前第')} ${from}–${to} ${t('条')}`}
      </span>
      <div className="tbl-foot-right">
        <select
          className="form-input tbl-pagesize"
          value={String(size)}
          onChange={(e) => {
            const v = e.target.value;
            onSizeChange(v === 'all' ? 'all' : Number(v));
            onPageChange(1);
          }}
          aria-label={t(ariaLabelSize)}
        >
          {sizes.map((n) => (
            <option key={String(n)} value={String(n)}>
              {n === 'all' ? t('全部') : `${n} / ${t('页')}`}
            </option>
          ))}
        </select>
        <Pagination page={page} totalPages={totalPages} onChange={onPageChange} />
        <span className="tbl-jump">
          {t('跳至')}
          <input
            className="form-input"
            value={jump}
            onChange={(e) => setJump(e.target.value.replace(/\D/g, ''))}
            onKeyDown={(e) => {
              if (e.key === 'Enter') submitJump();
              if (e.key === 'Escape') setJump('');
            }}
            onBlur={submitJump}
            placeholder={String(page)}
            disabled={totalPages <= 1}
            aria-label={t('跳至页码')}
          />
          {t('页')}
        </span>
      </div>
    </div>
  );
}
