import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Gift, Play, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { providerIcon } from '../lib/providerIcons';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, EmptyState, Input, Loading } from '../components/ui';
import type { BenefitResult, CheckinBenefitsData } from '../types';
import './BenefitsPanel.css';

/**
 * 福利通道（签到中心第 4 个 tab）
 *
 * 对照 Agent2API 的「新手福利 / 保活 / 成长任务 / 限时套餐」：这些动作
 * **一次性或限时**，领完即结清，所以不进每日调度，只能在这里手动触发。
 * 每个账号只渲染它自己 provider 支持的通道（后端 `actions_for` 决定）。
 */

/** 动作显示名与一句说明。 */
const ACTION_META: Record<string, { label: string; hint: string }> = {
  newbie: { label: '领新人礼', hint: 'CodeArts 新人注册礼（一次性）' },
  onboarding: { label: '盘点新手任务', hint: '只读：还剩哪些没领（结清后零上游请求）' },
  onboarding_claim: { label: '领取新手任务', hint: '领取全部未完成项' },
  keepalive: { label: '日活保活', hint: '探测 → 领取 → 免费模型最小对话（桌面 + 网页）' },
  growth: { label: '盘点成长任务', hint: '只读：WorkBuddy 国内版成长任务树' },
  plan: { label: '套餐盘点', hint: 'ZCode 限时套餐预览（领取需先在前端求解验证码）' },
  umid: { label: 'UMID 组件状态', hint: 'Qoder 国际版风控组件探测（不代为下载）' },
};

/** 结果徽标色调。 */
const OUTCOME_TONE: Record<string, 'success' | 'info' | 'warning' | 'danger' | 'neutral'> = {
  done: 'success',
  already: 'info',
  pending: 'warning',
  failed: 'danger',
  skipped: 'neutral',
};

const OUTCOME_LABEL: Record<string, string> = {
  done: '已领取',
  already: '已结清',
  pending: '待重试',
  failed: '失败',
  skipped: '不适用',
};

export default function BenefitsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [data, setData] = useState<CheckinBenefitsData | null>(null);
  const [error, setError] = useState('');
  const [running, setRunning] = useState<string>('');
  const [results, setResults] = useState<Record<string, BenefitResult>>({});
  const [keepaliveDraft, setKeepaliveDraft] = useState('');
  const [savingKeepalive, setSavingKeepalive] = useState(false);

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getCheckinBenefits();
      setData(res?.data ?? null);
      setKeepaliveDraft((res?.data?.keepalive?.models ?? []).join(', '));
      setError('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const run = async (accountId: string, action: string): Promise<void> => {
    const key = `${accountId}:${action}`;
    setRunning(key);
    try {
      const res = await api.runCheckinBenefit(accountId, action);
      const result = res?.data;
      if (result) {
        setResults((prev) => ({ ...prev, [key]: result }));
        addToast(result.message, result.ok ? 'success' : 'error');
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setRunning('');
    }
  };

  const saveKeepalive = async (): Promise<void> => {
    setSavingKeepalive(true);
    try {
      const models = keepaliveDraft
        .split(/[,\s]+/)
        .map((item) => item.trim())
        .filter((item) => item.length > 0);
      const res = await api.saveKeepaliveConfig(models);
      setKeepaliveDraft((res?.data?.models ?? models).join(', '));
      addToast(t('保活模型链已保存'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSavingKeepalive(false);
    }
  };

  if (loading) return <Loading />;
  if (error) return <div className="error-message">{error}</div>;
  if (!data || data.accounts.length === 0) {
    return (
      <EmptyState
        message={t(
          '没有可用福利通道的账号。福利通道覆盖 CodeArts / Loomy / 小浣熊 / OfficeAce / WorkBuddy / ZCode / Qoder 国际版。',
        )}
      />
    );
  }

  return (
    <>
      <Card
        title={t('福利与一次性领取')}
        actions={
          <Button variant="outline" onClick={() => void load()}>
            <RefreshCw size={14} /> {t('刷新')}
          </Button>
        }
      >
        <p className="bp-note">
          {t('一次性或限时的额度领取（新手礼 / 保活 / 成长任务 / 限时套餐）。这些动作不进每日调度，只能手动触发；已结清的通道再点也是零上游请求。')}
        </p>
        <div className="bp-list">
          {data.accounts.map((account) => {
            const icon = providerIcon(account.provider);
            return (
              <div key={account.id} className="bp-row">
                <div className="bp-head">
                  {icon ? (
                    <img className="bp-logo" src={icon} alt="" />
                  ) : (
                    <Gift size={16} />
                  )}
                  <span className="bp-name">{account.name || account.id}</span>
                  <span className="bp-provider">{account.provider}</span>
                  {account.keepalive?.date && (
                    <span className="bp-keepalive-note">
                      {t('保活')} {account.keepalive.date} · {account.keepalive.keepalive_ok ?? 0}/2
                    </span>
                  )}
                </div>
                <div className="bp-actions">
                  {account.actions.map((action) => {
                    const meta = ACTION_META[action] ?? { label: action, hint: '' };
                    const key = `${account.id}:${action}`;
                    const result = results[key];
                    return (
                      <div key={action} className="bp-action">
                        <Button
                          variant="outline"
                          title={t(meta.hint)}
                          disabled={running === key}
                          onClick={() => void run(account.id, action)}
                        >
                          <Play size={13} /> {t(meta.label)}
                        </Button>
                        {result && (
                          <Badge tone={OUTCOME_TONE[result.outcome] ?? 'neutral'}>
                            {t(OUTCOME_LABEL[result.outcome] ?? result.outcome)} · {result.message}
                          </Badge>
                        )}
                      </div>
                    );
                  })}
                </div>
              </div>
            );
          })}
        </div>
      </Card>

      <Card title={t('日活保活免费模型链')}>
        <p className="bp-note">
          {t('WorkBuddy 国际版保活用这些模型各打一发最小对话（不消耗积分）。逗号分隔；留空 = 恢复内置默认。')}
        </p>
        <Input
          label={t('免费模型（按可用性排序）')}
          value={keepaliveDraft}
          placeholder={data.keepalive.default_models.join(', ')}
          onChange={(e) => setKeepaliveDraft(e.target.value)}
        />
        <div className="bp-actions" style={{ marginTop: 12 }}>
          <Button onClick={() => void saveKeepalive()} disabled={savingKeepalive}>
            {savingKeepalive ? t('保存中…') : t('保存')}
          </Button>
          <Button
            variant="outline"
            onClick={() => setKeepaliveDraft(data.keepalive.default_models.join(', '))}
          >
            {t('恢复默认')}
          </Button>
        </div>
      </Card>
    </>
  );
}
