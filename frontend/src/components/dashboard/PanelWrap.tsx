// 面板统一外壳：Skeleton / 错误重试 / 空态都收在一处。
// 旧版「数据为空整块消失」让运维分不清「没数据」还是「接口挂了」，
// 这里错误态明确给出重试按钮，空态用 EmptyState 文案。
import { useTranslation } from 'react-i18next';
import { SkeletonList } from '../ui';
import type { UseQueryResult } from '@tanstack/react-query';

interface Props {
  title: string;
  /** 面板副标题右侧的小字（如窗口标签 / approx 标注） */
  extra?: JSX.Element | null;
  query: Pick<UseQueryResult<unknown, Error>, 'isPending' | 'isError' | 'refetch' | 'data'>;
  empty?: JSX.Element | null;
  /** data 为空时的文案（后端返回 null / 空数组） */
  emptyText?: string;
  children: (data: any) => JSX.Element;
  style?: React.CSSProperties;
  className?: string;
}

export default function PanelWrap({
  title,
  extra,
  query,
  empty,
  emptyText,
  children,
  style,
  className = 'section-card',
}: Props): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className={className} style={style}>
      <div className="section-card-header">
        <h2>{title}</h2>
        {extra ?? null}
      </div>
      <div className="section-card-body">
        {query.isPending ? (
          <SkeletonList rows={3} />
        ) : query.isError ? (
          <div className="empty-state" style={{ padding: 22, textAlign: 'center' }}>
            <p style={{ marginBottom: 8 }}>{t('面板数据加载失败')}</p>
            <button className="btn btn-outline btn-sm" onClick={() => void query.refetch()}>
              {t('重试')}
            </button>
          </div>
        ) : query.data == null || (Array.isArray(query.data) && query.data.length === 0) ? (
          empty ?? (
            <div className="empty-state" style={{ padding: 22 }}>
              <p>{emptyText ?? t('暂无数据')}</p>
            </div>
          )
        ) : (
          children(query.data)
        )}
      </div>
    </div>
  );
}
