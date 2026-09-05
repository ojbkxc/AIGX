import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { Card, Badge, EmptyState, Loading } from '../components/ui';
import type { Order, EpayConfig } from './types';

/** 易支付配置（仅取展示需要的字段，其余保持后端形状） */
interface EpayDisplay {
  price?: number;
}

export default function Orders(): JSX.Element {
  const [orders, setOrders] = useState<Order[]>([]);
  const [epay, setEpay] = useState<EpayDisplay | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const { t } = useTranslation();

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      // 并行拉取订单与易支付配置（旧订单无 quota 字段时按 amount × price 回退计算配额）
      const [orderRes, epayRes] = await Promise.all([
        api.listOrders(),
        api.getEpayConfig().catch(() => null),
      ]);
      setOrders(Array.isArray(orderRes?.data) ? orderRes.data : []);
      if (epayRes) {
        const cfg: EpayConfig | null = epayRes.data ?? null;
        setEpay(cfg ? { price: (cfg as { price?: number }).price } : null);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
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

  if (loading) return <Loading text={t('加载订单')} />;

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
            {t('所有订单')} ({orders.length})
          </>
        }
      >
        {orders.length === 0 ? (
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
                {orders.map((o) => (
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
