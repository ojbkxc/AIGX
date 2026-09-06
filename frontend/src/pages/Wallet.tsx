import { useState, useEffect, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Button, Card, Input, Loading, EmptyState } from '../components/ui';

interface WalletUser {
  email?: string;
  username?: string;
  quota?: number;
  used_quota?: number;
}

interface EpayConfig {
  enabled?: boolean;
  pay_address?: string;
  min_topup?: number;
  price?: number;
  pay_methods?: string[];
}

interface WalletOrder {
  trade_no: string;
  money?: number;
  amount?: number;
  quota?: number | null;
  payment_method?: string;
  status?: string;
  create_time?: number;
}

// 预设充值档位（new-api 式快捷金额）
const AMOUNT_PRESETS = [10, 50, 100, 500];

export default function Wallet(): JSX.Element {
  const [me, setMe] = useState<WalletUser | null>(null);
  const [epay, setEpay] = useState<EpayConfig | null>(null);
  const [orders, setOrders] = useState<WalletOrder[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [amount, setAmount] = useState('10');
  const [method, setMethod] = useState('alipay');
  const [submitting, setSubmitting] = useState(false);

  // 兑换码
  const [redeemCode, setRedeemCode] = useState('');
  const [redeeming, setRedeeming] = useState(false);

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [meRes, epayRes, orderRes] = await Promise.all([
        api.getMe().catch(() => null),
        api.getEpayInfo().catch(() => null),
        api.myOrders().catch(() => null),
      ]);
      if (meRes) setMe(meRes.data || null);
      if (epayRes) setEpay(epayRes.data || null);
      if (orderRes) setOrders(Array.isArray(orderRes.data) ? orderRes.data : []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const fmtQuota = (q: number | undefined): string => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  const handleTopup = async () => {
    const amt = Math.floor(Number(amount));

    // 空串/NaN 会得到 NaN，NaN <= 0 为 false，会绕过校验直接进 topup → 用 isFinite 兜底
    if (!Number.isFinite(amt) || amt <= 0) {
      setError(t('请输入有效金额'));
      return;
    }
    if (epay && amt < (epay.min_topup || 1)) {
      setError(`${t('最低充值')} ${epay.min_topup} ${t('元')}`);
      return;
    }
    setSubmitting(true);
    setError('');
    try {
      const res = await api.topup(amt, method);
      const params: Record<string, string> = res?.data || {};
      const url: string | undefined = res?.url;
      if (!url) {
        setError(t('支付网关未返回跳转地址，请检查易支付配置'));
        setSubmitting(false);
        return;
      }
      // 构造表单并提交（易支付网关要求 POST 表单跳转）
      const formEl = document.createElement('form');
      formEl.method = 'POST';
      formEl.action = url;
      for (const [k, v] of Object.entries(params)) {
        const input = document.createElement('input');
        input.type = 'hidden';
        input.name = k;
        input.value = String(v);
        formEl.appendChild(input);
      }
      document.body.appendChild(formEl);
      formEl.submit();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  };

  // 兑换码兑换（放在 loading 早退之前，保证 hooks 与事件处理函数定义顺序稳定）
  const handleRedeem = async () => {
    if (!redeemCode.trim()) {
      setError(t('请输入兑换码'));
      return;
    }
    setRedeeming(true);
    setError('');
    try {
      const res = await api.redeem(redeemCode.trim());
      const msg = res?.message || res?.msg || t('兑换成功');
      addToast(String(msg));
      setRedeemCode('');
      // 刷新账户信息
      const meRes = await api.getMe();
      if (meRes) setMe(meRes.data || null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRedeeming(false);
    }
  };

  if (loading) return <Loading text={t('加载钱包')} />;

  const remaining = me ? (me.quota || 0) - (me.used_quota || 0) : 0;
  const methods = (epay && epay.pay_methods && epay.pay_methods.length > 0) ? epay.pay_methods : ['alipay', 'wxpay'];

  return (
    <div>
      <div className="page-header">
        <h1>{t('钱包充值')}</h1>
        <p>{t('通过易支付为账户充值配额')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {!epay || epay.enabled === false ? (
        <Card>
          <EmptyState
            message={t('管理员尚未配置易支付，暂无法充值。请联系管理员在「易支付」页面完成配置。')}
            icon="💳"
          />
        </Card>
      ) : (
        <>
          <Card bodyClassName="">
            <div style={{ display: 'flex', justifyContent: 'space-between', flexWrap: 'wrap', gap: 16 }}>
              <div>
                <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('当前账户')}</div>
                <div style={{ fontSize: 18, fontWeight: 600 }}>{me?.email || '—'}</div>
                {me?.username && <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>@{me.username}</div>}
              </div>
              <div>
                <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('剩余配额')}</div>
                <div style={{ fontSize: 22, fontWeight: 700, color: 'var(--accent-color)' }}>
                  {fmtQuota(remaining)}
                </div>
              </div>
              <div>
                <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('兑换倍率')}</div>
                <div style={{ fontSize: 16, fontWeight: 600 }}>{t('1 元 =')} {epay.price || 1} {t('配额')}</div>
              </div>
            </div>
          </Card>

          <Card title={t('充值')} bodyClassName="">
            {/* 预设档位（new-api 式）：快捷选择 + 实时换算 */}
            <div className="wallet-amount-grid">
              {AMOUNT_PRESETS.map((v) => {
                const active = Math.floor(Number(amount)) === v;
                return (
                  <button
                    key={v}
                    type="button"
                    className={`wallet-amount-preset ${active ? 'active' : ''}`}
                    onClick={() => setAmount(String(v))}
                  >
                    <span className="wallet-amount-num">¥{v}</span>
                    <span className="wallet-amount-quota">{fmtQuota(v * (epay.price || 1))} {t('配额')}</span>
                  </button>
                );
              })}
            </div>
            <form onSubmit={(e: FormEvent) => { e.preventDefault(); void handleTopup(); }} style={{ display: 'grid', gap: 16, maxWidth: 480, marginTop: 14 }}>
              <Input
                label={t('充值金额（元）')}
                type="number"
                step="1"
                min={epay.min_topup || 1}
                value={amount}
                onChange={(e) => setAmount(e.target.value)}
                hint={`${t('最低')} ${epay.min_topup || 1} ${t('元，将获得')} ${fmtQuota((Number(amount) || 0) * (epay.price || 1))} ${t('配额')}`}
              />
              {/* 支付方式按钮网格（new-api 式） */}
              <div>
                <div style={{ fontSize: 13, fontWeight: 500, marginBottom: 8 }}>{t('支付方式')}</div>
                <div className="wallet-method-grid">
                  {methods.map((m) => (
                    <button
                      key={m}
                      type="button"
                      className={`wallet-method ${method === m ? 'active' : ''}`}
                      onClick={() => setMethod(m)}
                    >
                      <span className="wallet-method-icon">{m === 'alipay' ? '支付宝' : m === 'wxpay' ? '微信' : m}</span>
                    </button>
                  ))}
                </div>
              </div>
              {/* 实付结算块 */}
              <div className="wallet-pay-summary">
                <span>{t('实付金额')}</span>
                <span className="wallet-pay-amount">¥{Math.floor(Number(amount)) || 0}</span>
                <span className="wallet-pay-arrow">→</span>
                <span>{t('获得配额')}</span>
                <span className="wallet-pay-quota">{fmtQuota((Number(amount) || 0) * (epay.price || 1))}</span>
              </div>
              <Button type="submit" disabled={submitting}>
                {submitting ? t('正在跳转...') : t('立即充值')}
              </Button>
            </form>
          </Card>
        </>
      )}

      <Card title={t('兑换码充值')} bodyClassName="">
        <form onSubmit={(e: FormEvent) => { e.preventDefault(); void handleRedeem(); }} style={{ display: 'grid', gap: 16, maxWidth: 480 }}>
          <Input
            label={t('兑换码')}
            value={redeemCode}
            onChange={(e) => setRedeemCode(e.target.value)}
            placeholder={t('输入兑换码直接充值配额')}
            hint={t('输入管理员发放的兑换码，即可将对应配额充入账户。')}
            style={{ fontFamily: 'monospace', letterSpacing: 1 }}
          />
          <Button type="submit" disabled={redeeming}>
            {redeeming ? t('兑换中...') : t('立即兑换')}
          </Button>
        </form>
      </Card>

      <Card title={`${t('我的订单')} (${orders.length})`}>
        {orders.length === 0 ? (
          <EmptyState message={t('暂无订单')} icon="🧾" />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>{t('订单号')}</th>
                  <th>{t('金额')}</th>
                  <th>{t('配额')}</th>
                  <th>{t('支付方式')}</th>
                  <th>{t('状态')}</th>
                  <th>{t('创建时间')}</th>
                </tr>
              </thead>
              <tbody>
                {orders.map((o) => (
                  <tr key={o.trade_no}>
                    <td><code className="key-value" style={{ maxWidth: 240 }}>{o.trade_no}</code></td>
                    <td>¥{Number(o.money || 0).toFixed(2)}</td>
                    <td>{fmtQuota(o.quota != null ? o.quota : (o.amount || 0) * (epay?.price || 1))}</td>
                    <td>{o.payment_method}</td>
                    <td>
                      <span className={
                        o.status === 'paid' ? 'badge badge-success'
                          : o.status === 'expired' ? 'badge badge-neutral'
                            : 'badge badge-warning'
                      }>
                        {o.status === 'paid' ? t('已支付') : o.status === 'expired' ? t('已过期') : t('待支付')}
                      </span>
                    </td>
                    <td>{o.create_time ? new Date(o.create_time * 1000).toLocaleString() : '—'}</td>
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
