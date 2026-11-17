// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 签到中心的「活动福利 / 月度赠送」卡（对照 A2A ui-shared checkin-campaign-rewards.tsx
// + checkin-campaign-types.ts，按 AIGX 自研组件重写）。
//
// 契约：GET /api/accounts/{id}/campaigns → {campaigns, riskNote, notice, monthlyGifts, redemption}
//       POST /api/accounts/{id}/campaigns/claim {campaignId} → {campaignId, outcome, campaign}
// 活动由插件申报；没申报的家 400（该账号不进这张卡）。上游资格与协议全在插件里，
// 网关只做转发，所以这里只做「读 + 领」。

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Gift, Loader2, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, EmptyState } from '../components/ui';
import type { CheckinCampaignPreview, CheckinCampaignReward, CheckinMonthlyGift } from '../types';
import './CheckinCampaignCard.css';

type Tone = 'success' | 'info' | 'warning' | 'danger' | 'neutral';

/** 活动状态 → 色调 + 文案（字面量 t()，i18n 扫描器才登记得到） */
function campaignStatus(t: (s: string) => string, status: string): { tone: Tone; label: string } {
  switch (status) {
    case 'claimable': return { tone: 'info', label: t('可领取') };
    case 'claimed': return { tone: 'success', label: t('已领取') };
    case 'inactive': return { tone: 'neutral', label: t('未开始') };
    case 'ineligible': return { tone: 'neutral', label: t('不符合条件') };
    case 'confirming': return { tone: 'warning', label: t('确认中') };
    default: return { tone: 'danger', label: t('失败') };
  }
}

/** 月度赠送状态 → 色调 + 文案 */
function giftStatus(t: (s: string) => string, status: string): { tone: Tone; label: string } {
  switch (status) {
    case 'pending': return { tone: 'warning', label: t('待到账') };
    case 'granted': return { tone: 'success', label: t('已发放') };
    case 'failed': return { tone: 'danger', label: t('发放失败') };
    case 'cancelled': return { tone: 'neutral', label: t('已取消') };
    case 'processing': return { tone: 'info', label: t('处理中') };
    default: return { tone: 'neutral', label: t('未知') };
  }
}

/** 毫秒/秒时间戳 → 本地时间（认毫秒优先：> 1e11） */
function fmtTime(value: unknown): string {
  const n = Number(value);
  if (!Number.isFinite(n) || n <= 0) return '—';
  const ms = n < 1e11 ? n * 1000 : n;
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return '—';
  }
}

function fmtAmount(amount?: number | null, unit?: string): string {
  if (amount == null || !Number.isFinite(amount)) return '';
  return `${amount}${unit ? ` ${unit}` : ''}`;
}

interface CheckinCampaignCardProps {
  /** 目标账号 id（一次只看一个账号；多账号用一个下拉切换） */
  accountId: string;
  accountName: string;
  /** 该账号的 provider（用于文案区分与 400 处理） */
  provider: string;
  /** 换账号回调（卡头下拉） */
  onAccountChange?: (accountId: string) => void;
  /** 可作为目标的账号清单（同 provider 且带 campaigns 动作的家） */
  accounts?: Array<{ id: string; name: string }>;
}

/**
 * CheckinCampaignCard — 活动福利与月度赠送。
 *
 * 只在「福利通道」/「活动」Tab 里挂载：活动是一次性或限时项，不进每日调度。
 * 领取后整张卡回读（不发乐观更新 —— 上游状态字段多，猜错会让人以为已经领到）。
 */
export default function CheckinCampaignCard({
  accountId,
  accountName,
  provider,
  onAccountChange,
  accounts,
}: CheckinCampaignCardProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [data, setData] = useState<CheckinCampaignPreview | null>(null);
  const [loading, setLoading] = useState(false);
  const [notSupported, setNotSupported] = useState(false);
  const [error, setError] = useState('');
  const [claimingId, setClaimingId] = useState<string | null>(null);

  const load = async (): Promise<void> => {
    if (!accountId) return;
    setLoading(true);
    try {
      const res = await api.getCheckinCampaigns(accountId);
      setData(res?.data ?? null);
      setNotSupported(false);
      setError('');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      // 400「插件未提供活动福利」→ 这一家没接，给空态而不是红错
      if (/未提供活动福利|not.*campaign|400/i.test(msg) && /活动|活动福利/.test(msg)) {
        setNotSupported(true);
        setError('');
      } else {
        setError(msg);
      }
      setData(null);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [accountId]);

  const claim = async (reward: CheckinCampaignReward): Promise<void> => {
    if (claimingId) return;
    setClaimingId(reward.campaignId);
    try {
      const res = await api.claimCheckinCampaign(accountId, reward.campaignId);
      const outcome = res?.data?.outcome ?? '';
      if (outcome === 'claimed') addToast(t('活动奖励已领取'));
      else if (outcome === 'confirming') addToast(t('领取结果确认中，稍后刷新查看'));
      else if (outcome === 'already_claimed') addToast(t('该活动今天已经领过'));
      else if (outcome === 'ineligible' || outcome === 'inactive') addToast(t('该活动当前不可领取'));
      else if (outcome === 'failed') addToast(res?.data?.campaign ? t('领取失败') : t('领取失败'), 'error');
      else addToast(t('领取请求已提交'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setClaimingId(null);
    }
  };

  const campaigns = useMemo(() => data?.campaigns ?? [], [data]);
  const gifts: CheckinMonthlyGift[] = useMemo(() => data?.monthlyGifts ?? [], [data]);
  const claimable = useMemo(() => campaigns.filter((c) => c.status === 'claimable'), [campaigns]);

  return (
    <Card
      title={t('活动福利')}
      actions={
        <div className="cc-head-actions">
          {accounts && accounts.length > 1 && onAccountChange ? (
            <select
              className="form-input cc-account-pick"
              value={accountId}
              onChange={(e) => onAccountChange(e.target.value)}
              aria-label={t('选择账号')}
            >
              {accounts.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.name || a.id}
                </option>
              ))}
            </select>
          ) : (
            <span className="cc-account-name">{accountName || accountId}</span>
          )}
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
            {loading ? <Loader2 size={13} className="cc-spin" /> : <RefreshCw size={13} />}
            {t('刷新')}
          </Button>
        </div>
      }
    >
      {error && <div className="error-message">{error}</div>}
      {loading && !data ? (
        <div className="cc-loading">
          <Loader2 size={14} className="cc-spin" />
          {t('正在读取活动与月度赠送…')}
        </div>
      ) : notSupported ? (
        <EmptyState
          icon="🎁"
          message={t('该账号的插件未提供活动福利')}
          action={<p className="cc-hint">{t('活动由插件申报（campaigns / plans）；没接这一能力的家不显示活动卡。签到与保活照常可用。')}</p>}
        />
      ) : campaigns.length === 0 && gifts.length === 0 ? (
        <EmptyState icon="🎁" message={t('当前没有可领取的活动')} />
      ) : (
        <div className="cc-body">
          {data?.notice ? <p className="cc-notice">{data.notice}</p> : null}

          {campaigns.length > 0 && (
            <>
              {claimable.length > 0 && (
                <p className="cc-claimable-hint">{t('有 {{n}} 个活动可领取', { n: claimable.length })}</p>
              )}
              <table className="cc-table">
                <thead>
                  <tr>
                    <th>{t('活动')}</th>
                    <th>{t('奖励')}</th>
                    <th>{t('状态')}</th>
                    <th>{t('窗口')}</th>
                    <th>{t('领取时间')}</th>
                    <th style={{ width: 100 }}>{t('操作')}</th>
                  </tr>
                </thead>
                <tbody>
                  {campaigns.map((reward) => {
                    const view = campaignStatus(t, reward.status);
                    const busy = claimingId === reward.campaignId;
                    return (
                      <tr key={reward.campaignId}>
                        <td>
                          <div className="cc-title">{reward.title || reward.titleEn || reward.campaignId}</div>
                          {reward.description ? <div className="cc-desc">{reward.description}</div> : null}
                        </td>
                        <td className="cc-amount">{fmtAmount(reward.amount, reward.unit) || '—'}</td>
                        <td>
                          <Badge tone={view.tone}>{view.label}</Badge>
                        </td>
                        <td className="cc-time">
                          {reward.startsAt || reward.endsAt
                            ? `${fmtTime(reward.startsAt)} → ${fmtTime(reward.endsAt)}`
                            : '—'}
                        </td>
                        <td className="cc-time">{fmtTime(reward.claimedAt)}</td>
                        <td>
                          <Button
                            variant="outline"
                            size="sm"
                            disabled={reward.status !== 'claimable' || busy}
                            onClick={() => void claim(reward)}
                          >
                            {busy ? <Loader2 size={12} className="cc-spin" /> : <Gift size={12} />}
                            {t('领取')}
                          </Button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </>
          )}

          {gifts.length > 0 && (
            <div className="cc-gifts">
              <h4>{t('月度赠送')}</h4>
              <ul>
                {gifts.map((gift) => {
                  const view = giftStatus(t, gift.status);
                  return (
                    <li key={gift.id}>
                      <span className="cc-gift-period">
                        {gift.period ? t('第 {{n}} 期', { n: gift.period }) : gift.orderId || gift.id}
                      </span>
                      <span className="cc-gift-amount">{fmtAmount(gift.amount) || '—'}</span>
                      <Badge tone={view.tone}>{view.label}</Badge>
                      <span className="cc-time">
                        {gift.grantedAt ? t('发放于 {{t}}', { t: fmtTime(gift.grantedAt) }) : t('计划 {{t}}', { t: fmtTime(gift.scheduledAt) })}
                      </span>
                    </li>
                  );
                })}
              </ul>
            </div>
          )}

          {data?.riskNote ? <p className="cc-risk">{data.riskNote}</p> : null}
          {data?.redemption?.supported ? (
            <p className="cc-hint">{t('该家支持权益兑换码——在福利通道里用「兑换」动作处理。')}</p>
          ) : null}
          {provider ? <p className="cc-hint cc-hint-muted">{t('来源：{{provider}} 插件', { provider })}</p> : null}
        </div>
      )}
    </Card>
  );
}