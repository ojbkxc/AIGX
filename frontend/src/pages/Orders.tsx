import { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { Card, Badge, Button, EmptyState, SkeletonTable } from '../components/ui';
import type { Order, EpayConfig } from './types';

/** 易支付配置（仅取展示需要的字段，其余保持后端形状） */
interface EpayDisplay {
  price?: number;
}

/** 后端 PageInfo 契约响应（对齐 new-api） */
interface OrdersPage {
  success: boolean;
  data?: Order[];
  items?: Order[];
  total?: number;
  page?: number;
  page_size?: number;
}

const PAGE_SIZE = 10;

export default function Orders(): JSX.Element {
  const [orders, setOrders] = useState<Order[]>([]);
  const [total, setTotal] = useState(0);
  const [page, setPage] = useState(1);
  const [keyword, setKeyword] = useState('');
  const [searchInput, setSearchInput] = useState('');
  const [epay, setEpay] = useState<EpayDisplay | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [statusFilter, setStatusFilter] = useState('');
  const [error, setError] = useState('');
  const [completing, setCompleting] = useState<string | null>(null);
  const { t } = useTranslation();

  const load = useCallback(
    async (p = page, kw = keyword) => {
      setRefreshing(true);
      setLoading(true);
      setError('');
      try {
        // 并行拉取订单分页与易支付配置（旧订单无 quota 字段时按 amount × price 回退计算配额）
        const [orderRes, epayRes] = await Promise.all([
          api.listOrders({ p, page_size: PAGE_SIZE, keyword: kw }),
          api.getEpayConfig().catch(() => null),
        ]);
        const res = orderRes as unknown as OrdersPage;
        // PageInfo 契约：items/total 优先；旧形状 data 兜底
        const list = res?.items ?? res?.data ?? [];
        setOrders(Array.isArray(list) ? (list as Order[]) : []);
        setTotal(res?.total ?? (Array.isArray(list) ? list.length : 0));
        if (epayRes) {
          const cfg: EpayConfig | null = (epayRes.data ?? null) as unknown as EpayConfig | null;
          setEpay(cfg ? { price: (cfg as { price?: number }).price } : null);
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
        setRefreshing(false);
      }
    },
    [page, keyword],
  );

  useEffect(() => {
    void load(1, '');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const goToPage = (p: number) => {
    setPage(p);
    void load(p, keyword);
  };

  const doSearch = () => {
    setPage(1);
    setKeyword(searchInput.trim());
    void load(1, searchInput.trim());
  };

  /** 管理端手动补单：线下收款后把 pending 订单标记 paid 并入账（幂等） */
  const completeOrder = async (tradeNo: string) => {
    if (!window.confirm(t('确认补单？将立即为该订单用户入账配额。'))) return;
    setCompleting(tradeNo);
    try {
      await api.completeOrder(tradeNo);
      void load(page, keyword);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCompleting(null);
    }
  };

  // 配额数值格式化（与 Wallet 页保持一致）
  const fmtQuota = (q: number | null | undefined): string => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  const statusTone = (status: string | undefined): 'success' | 'neutral' | 'warning' =>
    status === 'paid' ? 'success' : status === 'expired' ? 'neutral' : 'warning';

  const statusLabel = (status: string | undefined): string =>
    status === 'paid' ? t('已支付') : status === 'expired' ? t('已过期') : t('待支付');

  const visibleOrders = statusFilter
    ? orders.filter((o) => (o.status || 'pending') === statusFilter)
    : orders;

  const totalPages = Math.max(1, Math.ceil(total / PAGE_SIZE));

  if (loading) return <SkeletonTable columns={5} rows={6} />;

  return (
    <div>
      <div className="page-header">
        <h1>{t('订单记录')}</h1>
        <p>{t('所有用户的充值订单（管理员视图）')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Card
        title={
          <>
            {t('所有订单')} ({total})
          </>
        }
        actions={
          <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
            <input
              className="form-input"
              value={searchInput}
              onChange={(e) => setSearchInput(e.target.value)}
              onKeyDown={(e) => e.key === 'Enter' && doSearch()}
              placeholder={t('搜索订单号 / 用户ID / 支付方式')}
              style={{ width: 220 }}
              aria-label={t('搜索')}
            />
            <Button variant="outline" size="sm" onClick={doSearch}>
              {t('搜索')}
            </Button>
            <select
              className="form-input"
              value={statusFilter}
              onChange={(e) => setStatusFilter(e.target.value)}
              style={{ width: 130 }}
              aria-label={t('状态')}
            >
              <option value="">{t('全部')}</option>
              <option value="pending">{t('待支付')}</option>
              <option value="paid">{t('已支付')}</option>
              <option value="expired">{t('已过期')}</option>
            </select>
            <Button variant="outline" size="sm" onClick={() => void load(page, keyword)} disabled={refreshing} style={{ gap: 6 }}>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"
                style={refreshing ? { animation: 'spin 0.9s linear infinite' } : undefined}>
                <polyline points="23 4 23 10 17 10" />
                <polyline points="1 20 1 14 7 14" />
                <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
              </svg>
              {refreshing ? t('刷新中…') : t('刷新')}
            </Button>
          </div>
        }
      >
        {visibleOrders.length === 0 ? (
          <EmptyState message={t('暂无订单')} />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>{t('订单号')}</th>
                  <th>{t('用户ID')}</th>
                  <th>{t('金额')}</th>
                  <th>{t('配额')}</th>
                  <th>{t('支付方式')}</th>
                  <th>{t('状态')}</th>
                  <th>{t('创建时间')}</th>
                  <th>{t('支付时间')}</th>
                  <th>{t('操作')}</th>
                </tr>
              </thead>
              <tbody>
                {visibleOrders.map((o) => (
                  <tr key={o.trade_no || o.id || ''}>
                    <td>
                      <code className="key-value" style={{ maxWidth: 240 }}>
                        {o.trade_no || o.id || '—'}
                      </code>
                    </td>
                    <td style={{ fontSize: 12 }}>{o.user_id ? String(o.user_id).slice(0, 8) + '…' : '—'}</td>
                    <td>¥{Number(o.money ?? o.amount ?? 0).toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</td>
                    <td>{fmtQuota(o.quota != null ? o.quota : (o.amount ?? 0) * (epay?.price || 1))}</td>
                    <td>{o.payment_method || o.method || '—'}</td>
                    <td>
                      <Badge tone={statusTone(o.status)}>{statusLabel(o.status)}</Badge>
                    </td>
                    <td>{o.create_time ? new Date(o.create_time * 1000).toLocaleString() : '—'}</td>
                    <td>{o.paid_time ? new Date(o.paid_time * 1000).toLocaleString() : '—'}</td>
                    <td>
                      {(o.status || 'pending') === 'pending' && (
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => void completeOrder(o.trade_no || o.id || '')}
                          disabled={completing === (o.trade_no || o.id || '')}
                        >
                          {completing === (o.trade_no || o.id || '') ? t('补单中…') : t('补单')}
                        </Button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {totalPages > 1 && (
          <div style={{ display: 'flex', alignItems: 'center', gap: 12, marginTop: 16, justifyContent: 'flex-end' }}>
            <Button variant="outline" size="sm" onClick={() => goToPage(page - 1)} disabled={page <= 1}>
              {t('上一页')}
            </Button>
            <span style={{ fontSize: 13 }}>
              {page} / {totalPages}
            </span>
            <Button variant="outline" size="sm" onClick={() => goToPage(page + 1)} disabled={page >= totalPages}>
              {t('下一页')}
            </Button>
          </div>
        )}
      </Card>
    </div>
  );
}
