import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { Card, Badge, Button, EmptyState, SkeletonTable } from '../components/ui';
import type { Order, EpayConfig } from './types';

/** 易支付配置（仅取展示需要的字段，其余保持后端形状） */
interface EpayDisplay {
  price?: number;
}

export default function Orders(): JSX.Element {
  const [orders, setOrders] = useState<Order[]>([]);
  const [epay, setEpay] = useState<EpayDisplay | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [statusFilter, setStatusFilter] = useState('');
  const [error, setError] = useState('');
  const { t } = useTranslation();

  useEffect(() => {
    void load();
  }, []);

  const load = async () => {
    setRefreshing(true);
    setLoading(true);
    setError('');
    try {
      // 并行拉取订单与易支付配置（旧订单无 quota 字段时按 amount × price 回退计算配额）
      const [orderRes, epayRes] = await Promise.all([
        api.listOrders(),
        api.getEpayConfig().catch(() => null),
      ]);
      setOrders(Array.isArray(orderRes?.data) ? (orderRes.data as Order[]) : []);
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
            {t('所有订单')} ({visibleOrders.length})
          </>
        }
        actions={
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
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
            <Button variant="outline" size="sm" onClick={() => void load()} disabled={refreshing} style={{ gap: 6 }}>
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
                    <td style={{ fontSize: 12 }}>{o.user_id ? o.user_id.slice(0, 8) + '…' : '—'}</td>
                    <td>¥{Number(o.money ?? o.amount ?? 0).toFixed(2)}</td>
                    <td>{fmtQuota(o.quota != null ? o.quota : (o.amount ?? 0) * (epay?.price || 1))}</td>
                    <td>{o.payment_method || o.method || '—'}</td>
                    <td>
                      <Badge tone={statusTone(o.status)}>{statusLabel(o.status)}</Badge>
                    </td>
                    <td>{o.create_time ? new Date(o.create_time * 1000).toLocaleString() : '—'}</td>
                    <td>{o.paid_time ? new Date(o.paid_time * 1000).toLocaleString() : '—'}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  );
}
