import { useState, useEffect, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Input, Loading, EmptyState } from '../components/ui';

interface WalletUser {
  email?: string;
  username?: string;
  quota?: number;
  used_quota?: number;
  aff_count?: number;
  aff_quota?: number;
  aff_history_quota?: number;
}

interface CheckinState {
  enabled?: boolean;
  min_quota?: number;
  max_quota?: number;
  checked_today?: boolean;
  month_total?: number;
  count?: number;
}

interface EpayConfig {
  enabled?: boolean;
  pay_address?: string;
  min_topup?: number;
  price?: number;
  amount_discount?: Record<string, number>;
  pay_methods?: string[];
}

/** 订阅套餐卡片数据（#85，对齐后端 /api/subscription/plans） */
interface SubPlan {
  id: string;
  name: string;
  price?: number;
  description?: string;
  duration_unit?: string;
  duration_value?: number;
  custom_seconds?: number;
  total_amount?: number;
  quota_reset_period?: string;
  allow_balance_pay?: boolean;
  upgrade_group?: string;
  sort_order?: number;
  [key: string]: unknown;
}

/** 我的订阅实例（#85，对齐后端 /api/subscription/self） */
interface MySubscription {
  id: string;
  plan_id: string;
  amount_total: number;
  amount_used: number;
  start_time: number;
  end_time: number;
  status: string;
  next_reset_time: number;
  upgrade_group?: string;
  plan_name?: string;
  plan_price?: number;
  [key: string]: unknown;
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

  // 邀请返利
  const [affCode, setAffCode] = useState('');
  const [claiming, setClaiming] = useState(false);
  // 每日签到
  const [checkin, setCheckin] = useState<CheckinState | null>(null);
  const [checkingIn, setCheckingIn] = useState(false);

  const [amount, setAmount] = useState('10');
  const [method, setMethod] = useState('alipay');
  const [submitting, setSubmitting] = useState(false);

  // 兑换码
  const [redeemCode, setRedeemCode] = useState('');
  const [redeeming, setRedeeming] = useState(false);
  // 订阅（#85 套餐订阅化）
  const [subPlans, setSubPlans] = useState<SubPlan[]>([]);
  const [mySubs, setMySubs] = useState<MySubscription[]>([]);
  const [buyingSubId, setBuyingSubId] = useState<string | null>(null);
  // 兑换码/充值表单内联错误（页面顶部 error 离表单太远，用户看不到）
  const [redeemError, setRedeemError] = useState('');
  const [topupError, setTopupError] = useState('');
  // 兑换码是不可逆消费，提交前二次确认
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  useEffect(() => {
    void load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [meRes, epayRes, orderRes, affRes, checkinRes, subPlansRes, mySubsRes] = await Promise.all([
        api.getMe().catch(() => null),
        api.getEpayInfo().catch(() => null),
        api.myOrders().catch(() => null),
        api.getAffCode().catch(() => null),
        api.checkinStatus().catch(() => null),
        api.subscriptionPlans().catch(() => null),
        api.subscriptionSelf().catch(() => null),
      ]);
      if (meRes) setMe(meRes.data as WalletUser | null);
      if (epayRes) setEpay(epayRes.data as EpayConfig | null);
      if (orderRes) setOrders(Array.isArray(orderRes.data) ? (orderRes.data as unknown as WalletOrder[]) : []);
      if (affRes) setAffCode(String(affRes.data ?? ''));
      if (subPlansRes && Array.isArray(subPlansRes.data)) setSubPlans(subPlansRes.data as unknown as SubPlan[]);
      if (mySubsRes && Array.isArray(mySubsRes.data)) setMySubs(mySubsRes.data as unknown as MySubscription[]);
      // 签到未启用时后端返回 success=false，data 为空 → 不渲染签到卡片
      if (checkinRes && (checkinRes as { success?: boolean }).success) {
        setCheckin((checkinRes.data ?? null) as CheckinState | null);
      }
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

  // 与后端 pay_money/topup_quota 对齐：档位折扣（金额 → 折扣比例）
  const discountFor = (amt: number): number => {
    const d = epay?.amount_discount;
    if (!d) return 1;
    const v = d[String(amt)] ?? d[amt];
    return typeof v === 'number' && v > 0 ? v : 1;
  };

  const payMoney = (amt: number): number => {
    const money = amt * discountFor(amt);
    return Math.round(money * 100) / 100;
  };

  const quotaFor = (amt: number): number => {
    const quota = amt * (epay?.price || 1) * discountFor(amt);
    return quota;
  };

  const handleTopup = async () => {
    const amt = Math.floor(Number(amount));

    // 空串/NaN 会得到 NaN，NaN <= 0 为 false，会绕过校验直接进 topup → 用 isFinite 兜底
    if (!Number.isFinite(amt) || amt <= 0) {
      setTopupError(t('请输入有效金额'));
      return;
    }
    if (epay && amt < (epay.min_topup || 1)) {
      setTopupError(`${t('最低充值')} ${epay.min_topup} ${t('元')}`);
      return;
    }
    setSubmitting(true);
    setError('');
    setTopupError('');
    try {
      const res = await api.topup(amt, method);
      const data = (res?.data ?? {}) as Record<string, unknown> & { url?: string };
      const params: Record<string, string> = {};
      for (const [k, v] of Object.entries(data)) {
        if (k !== 'url') params[k] = String(v);
      }
      const url: string | undefined = data.url;
      if (!url) {
        setTopupError(t('支付网关未返回跳转地址，请检查易支付配置'));
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
      setTopupError(err instanceof Error ? err.message : String(err));
    } finally {
      setSubmitting(false);
    }
  };

  /** 余额购买订阅（#85）：扣余额 → 建订阅实例（独立配额池 + 时长 + 分组升级） */
  const handleBuySubscription = (p: SubPlan) => {
    setConfirmState({
      title: t('确认购买订阅'),
      message: (
        <>
          {t('即将以余额购买订阅「{{name}}」，将从余额扣费，订阅期间消耗优先走订阅配额池。', { name: p.name })}
          {p.total_amount ? <><br />{t('订阅总配额')}：{fmtQuota(p.total_amount)}</> : null}
          <br />{t('售价')}：¥{Number(p.price || 0).toFixed(2)}
        </>
      ),
      confirmText: t('确认购买'),
      onConfirm: async () => {
        setBuyingSubId(p.id);
        try {
          await api.subscriptionBalancePay(p.id);
          addToast(t('订阅购买成功'));
          // 刷新余额与订阅列表
          const [meRes, subsRes] = await Promise.all([
            api.getMe().catch(() => null),
            api.subscriptionSelf().catch(() => null),
          ]);
          if (meRes) setMe(meRes.data as WalletUser | null);
          if (subsRes && Array.isArray(subsRes.data)) setMySubs(subsRes.data as unknown as MySubscription[]);
        } catch (err) {
          addToast(err instanceof Error ? err.message : String(err), 'error');
        } finally {
          setBuyingSubId(null);
        }
      },
    });
  };

  /** 每日签到：随机奖励直进可用配额（一人一天一次） */
  const handleCheckin = async () => {
    setCheckingIn(true);
    try {
      const res = await api.doCheckin();
      const awarded = Number((res?.data as { quota_awarded?: number } | null)?.quota_awarded ?? 0);
      addToast(`${t('签到成功，获得')} ${fmtQuota(awarded)} ${t('配额')}`);
      // 刷新余额与签到状态
      const [meRes, statusRes] = await Promise.all([
        api.getMe().catch(() => null),
        api.checkinStatus().catch(() => null),
      ]);
      if (meRes) setMe(meRes.data as WalletUser | null);
      if (statusRes && (statusRes as { success?: boolean }).success) {
        setCheckin((statusRes.data ?? null) as CheckinState | null);
      } else {
        setCheckin(null);
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err));
    } finally {
      setCheckingIn(false);
    }
  };

  /** 复制邀请链接（注册页读 ?aff= 参数自动填入邀请码） */
  const copyInviteLink = () => {
    const base = window.location.origin + '/register';
    const link = affCode ? `${base}?aff=${encodeURIComponent(affCode)}` : base;
    void navigator.clipboard.writeText(link).then(
      () => addToast(t('邀请链接已复制')),
      () => addToast(t('复制失败，请手动复制邀请码')),
    );
  };

  /** 领取邀请奖励：aff_quota 全额划转到可用配额 */
  const claimAff = async () => {
    setClaiming(true);
    try {
      const res = await api.affTransfer();
      const transferred = Number((res?.data as { transferred?: number } | null)?.transferred ?? 0);
      if (transferred > 0) {
        addToast(`${t('已领取')} ${fmtQuota(transferred)} ${t('配额')}`);
      } else {
        addToast(t('暂无可领取的邀请奖励'));
      }
      const meRes = await api.getMe();
      if (meRes) setMe(meRes.data as WalletUser | null);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err));
    } finally {
      setClaiming(false);
    }
  };

  // 兑换码兑换（放在 loading 早退之前，保证 hooks 与事件处理函数定义顺序稳定）
  const handleRedeem = async () => {    if (!redeemCode.trim()) {
      setRedeemError(t('请输入兑换码'));
      return;
    }
    setRedeeming(true);
    setError('');
    setRedeemError('');
    try {
      const res = await api.redeem(redeemCode.trim());
      const data = res?.data ?? {};
      const msg = (data as { message?: string }).message || (data as { msg?: string }).msg || t('兑换成功');
      addToast(String(msg));
      setRedeemCode('');
      // 刷新账户信息
      const meRes = await api.getMe();
      if (meRes) setMe(meRes.data as WalletUser | null);
    } catch (err) {
      setRedeemError(err instanceof Error ? err.message : String(err));
    } finally {
      setRedeeming(false);
    }
  };

  if (loading) return <Loading text={t('加载钱包')} />;

  const remaining = me ? (me.quota || 0) - (me.used_quota || 0) : 0;
  const methods = (epay && epay.pay_methods && epay.pay_methods.length > 0) ? epay.pay_methods : ['alipay', 'wxpay'];

  // 订阅展示辅助（#85）
  const fmtSubDuration = (p: SubPlan): string => {
    if (p.duration_unit === 'custom') {
      const s = Number(p.custom_seconds || 0);
      if (s <= 0) return '—';
      if (s % 86400 === 0) return `${s / 86400} ${t('天')}`;
      if (s % 3600 === 0) return `${s / 3600} ${t('小时')}`;
      return `${s} ${t('秒')}`;
    }
    const unitMap: Record<string, string> = { year: t('年'), month: t('个月'), day: t('天'), hour: t('小时') };
    const v = Number(p.duration_value || 0);
    if (v <= 0) return '—';
    return `${v} ${unitMap[p.duration_unit || 'month'] || ''}`;
  };

  const fmtResetPeriod = (period: string | undefined): string => {
    const map: Record<string, string> = {
      never: t('不重置'),
      daily: t('每日重置'),
      weekly: t('每周重置'),
      monthly: t('每月重置'),
      custom: t('周期重置'),
    };
    return map[period || 'never'] || period || '';
  };

  const subStatusBadge = (s: MySubscription): { cls: string; text: string } => {
    const now = Math.floor(Date.now() / 1000);
    if (s.status === 'cancelled') return { cls: 'badge badge-neutral', text: t('已取消') };
    if (s.status === 'expired' || s.end_time <= now) return { cls: 'badge badge-neutral', text: t('已到期') };
    return { cls: 'badge badge-success', text: t('生效中') };
  };

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
                {me?.username && <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>{me.username}</div>}
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
                    <span className="wallet-amount-quota">
                      {fmtQuota(quotaFor(v))} {t('配额')}
                      {discountFor(v) < 1 && <em style={{ marginLeft: 4, fontStyle: 'normal', color: '#34d399' }}>{t('折扣')} {discountFor(v)}</em>}
                    </span>
                  </button>
                );
              })}
            </div>
            <form onSubmit={(e: FormEvent) => { e.preventDefault(); void handleTopup(); }} style={{ display: 'grid', gap: 16, maxWidth: 480, marginTop: 14 }}>
              {topupError && <div className="error-message">{topupError}</div>}
              <Input
                label={t('充值金额（元）')}
                type="number"
                step="1"
                min={epay.min_topup || 1}
                value={amount}
                onChange={(e) => setAmount(e.target.value)}
                hint={`${t('最低')} ${epay.min_topup || 1} ${t('元，将获得')} ${fmtQuota(quotaFor(Number(amount) || 0))} ${t('配额')}`}
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
                <span className="wallet-pay-amount">¥{payMoney(Number(amount) || 0)}</span>
                <span className="wallet-pay-arrow">→</span>
                <span>{t('获得配额')}</span>
                <span className="wallet-pay-quota">{fmtQuota(quotaFor(Number(amount) || 0))}</span>
              </div>
              <Button type="submit" disabled={submitting}>
                {submitting ? t('正在跳转...') : t('立即充值')}
              </Button>
            </form>
          </Card>
        </>
      )}

      {/* 每日签到卡片（对齐 new-api CheckinCard；未启用时不渲染） */}
      {checkin?.enabled && (
        <Card title={t('每日签到')} bodyClassName="">
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 24, alignItems: 'center' }}>
            <div>
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('今日状态')}</div>
              <div style={{ fontSize: 15, fontWeight: 600, marginTop: 4 }}>
                {checkin.checked_today ? t('已签到') : t('未签到')}
              </div>
            </div>
            <div>
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('奖励区间')}</div>
              <div style={{ fontSize: 15, fontWeight: 600, marginTop: 4 }}>
                {fmtQuota(checkin.min_quota)} ~ {fmtQuota(checkin.max_quota)}
              </div>
            </div>
            <div>
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('本月签到')}</div>
              <div style={{ fontSize: 15, fontWeight: 600, marginTop: 4 }}>{checkin.count ?? 0} {t('天')}</div>
            </div>
            <div>
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('本月累计奖励')}</div>
              <div style={{ fontSize: 15, fontWeight: 600, marginTop: 4, color: 'var(--accent-color)' }}>
                {fmtQuota(checkin.month_total)}
              </div>
            </div>
            <Button size="sm" onClick={() => void handleCheckin()} disabled={checkingIn || checkin.checked_today}>
              {checkin.checked_today ? t('今日已签') : checkingIn ? t('签到中...') : t('立即签到')}
            </Button>
          </div>
        </Card>
      )}

      {/* 订阅套餐购买（#85 套餐订阅化，对齐 new-api SubscriptionCard） */}
      {subPlans.length > 0 && (
        <Card title={t('订阅套餐')} bodyClassName="">
          <div className="plans-grid" style={{ marginBottom: mySubs.length ? 16 : 0 }}>
            {subPlans.map((p) => (
              <div key={p.id} className="plan-card" style={{ border: '1px solid var(--border-color, #e5e7eb)', borderRadius: 12, padding: 14, background: 'var(--bg-color, #f7f8fa)' }}>
                <div className="plan-card-head" style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 8 }}>
                  <strong style={{ fontSize: 14 }}>{p.name}</strong>
                  <span style={{ fontWeight: 700, color: 'var(--accent-color)' }}>¥{Number(p.price || 0).toFixed(2)}</span>
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4, fontSize: 12.5, marginBottom: 10 }}>
                  <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                    <span style={{ color: 'var(--text-muted)' }}>{t('时长')}</span>
                    <span>{fmtSubDuration(p)}</span>
                  </div>
                  <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                    <span style={{ color: 'var(--text-muted)' }}>{t('订阅配额')}</span>
                    <span>{p.total_amount ? fmtQuota(p.total_amount) : t('不限')}</span>
                  </div>
                  <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                    <span style={{ color: 'var(--text-muted)' }}>{t('周期重置')}</span>
                    <span>{fmtResetPeriod(p.quota_reset_period)}</span>
                  </div>
                  {p.upgrade_group && (
                    <div style={{ display: 'flex', justifyContent: 'space-between' }}>
                      <span style={{ color: 'var(--text-muted)' }}>{t('专属分组')}</span>
                      <span>{p.upgrade_group}</span>
                    </div>
                  )}
                </div>
                {p.description && (
                  <p style={{ fontSize: 12, color: 'var(--text-muted)', margin: '0 0 10px', lineHeight: 1.5 }}>{p.description}</p>
                )}
                <Button
                  size="sm"
                  style={{ width: '100%' }}
                  disabled={buyingSubId === p.id || p.allow_balance_pay === false}
                  onClick={() => handleBuySubscription(p)}
                >
                  {p.allow_balance_pay === false
                    ? t('不支持余额购买')
                    : buyingSubId === p.id ? t('购买中...') : t('余额购买')}
                </Button>
              </div>
            ))}
          </div>

          {/* 我的订阅列表 */}
          {mySubs.length > 0 && (
            <div className="table-wrapper">
              <table>
                <thead>
                  <tr>
                    <th>{t('套餐')}</th>
                    <th>{t('状态')}</th>
                    <th>{t('已用/总配额')}</th>
                    <th>{t('到期时间')}</th>
                    <th>{t('下次重置')}</th>
                  </tr>
                </thead>
                <tbody>
                  {mySubs.map((s) => {
                    const badge = subStatusBadge(s);
                    const now = Math.floor(Date.now() / 1000);
                    const active = s.status === 'active' && s.end_time > now;
                    return (
                      <tr key={s.id}>
                        <td>{s.plan_name || s.plan_id}</td>
                        <td><span className={badge.cls}>{badge.text}</span></td>
                        <td>
                          {s.amount_total > 0
                            ? `${fmtQuota(s.amount_used)} / ${fmtQuota(s.amount_total)}`
                            : `${fmtQuota(s.amount_used)} / ${t('不限')}`}
                        </td>
                        <td>{s.end_time > 0 ? new Date(s.end_time * 1000).toLocaleString() : '—'}</td>
                        <td>{active && s.next_reset_time > 0 ? new Date(s.next_reset_time * 1000).toLocaleString() : '—'}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </Card>
      )}

      {/* 邀请返利卡片（对齐 new-api AffiliateRewardsCard） */}
      <Card title={t('邀请返利')} bodyClassName="">
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 24, alignItems: 'center' }}>
          <div>
            <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('我的邀请码')}</div>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 4 }}>
              <code className="key-value" style={{ fontSize: 16, letterSpacing: 1 }}>{affCode || '—'}</code>
              <Button variant="outline" size="sm" onClick={copyInviteLink} disabled={!affCode}>
                {t('复制邀请链接')}
              </Button>
            </div>
          </div>
          <div>
            <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('已邀请人数')}</div>
            <div style={{ fontSize: 18, fontWeight: 600, marginTop: 4 }}>{me?.aff_count ?? 0}</div>
          </div>
          <div>
            <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('待领取奖励')}</div>
            <div style={{ fontSize: 18, fontWeight: 600, marginTop: 4, color: 'var(--accent-color)' }}>
              {fmtQuota(me?.aff_quota ?? 0)}
            </div>
          </div>
          <div>
            <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('累计邀请获得')}</div>
            <div style={{ fontSize: 18, fontWeight: 600, marginTop: 4 }}>{fmtQuota(me?.aff_history_quota ?? 0)}</div>
          </div>
          <Button size="sm" onClick={() => void claimAff()} disabled={claiming || !me?.aff_quota}>
            {claiming ? t('领取中...') : t('领取奖励')}
          </Button>
        </div>
      </Card>

      <Card title={t('兑换码充值')} bodyClassName="">        <form onSubmit={(e: FormEvent) => {
          e.preventDefault();
          const code = redeemCode.trim();
          if (!code) {
            setRedeemError(t('请输入兑换码'));
            return;
          }
          // 兑换码一经提交立即消费（不可逆），先确认再执行
          setConfirmState({
            title: t('确认兑换'),
            message: <>{t('即将兑换以下兑换码，兑换后立即生效且无法撤销：')}<br /><code className="key-value">{code}</code></>,
            confirmText: t('确认兑换'),
            onConfirm: () => handleRedeem(),
          });
        }} style={{ display: 'grid', gap: 16, maxWidth: 480 }}>
          {redeemError && <div className="error-message">{redeemError}</div>}
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

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

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
                    <td>¥{Number(o.money || 0).toLocaleString('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}</td>
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
