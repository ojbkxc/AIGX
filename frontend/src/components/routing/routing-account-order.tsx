/**
 * 账号消耗顺序（对齐 Agent2API v3.5.1 `routing-account-order.tsx`）。
 *
 * 图标 + 「消耗顺序」+「积分到期 / 可用余额」两个下拉 + 消耗规则入口。
 * 数据来自 `routing-account-directory.ts` 的 sorting（GET/PUT
 * `/api/accounts/sorting`）—— 与账号页「排序规则」弹窗是同一份服务端配置，
 * 这里改的就是调度选路用的消耗规则，立即保存，不进路由档案草稿。
 *
 * 目录还没读到（sorting = null）时退化为只读说明位，不渲染空下拉
 * （那会让人以为路由页能改，实际改了不生效）。
 */
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowUpDown, ChevronRight } from 'lucide-react';
import type { AccountSortRule, AccountSortSettings } from '../../types';
import { RoutingSelect } from './routing-select';

export function RoutingConsumptionOrder({
  provider,
  sorting,
  balanceSortStates,
  independent,
  disabled,
  onSettings,
  onSaveSorting,
}: {
  /** 提供商 id；缺省 = 没有目录数据，只渲染说明位 */
  provider?: string;
  /** provider → 消耗规则（A2A `AccountSorting.rules` 的对应物） */
  sorting?: AccountSortSettings | null;
  balanceSortStates?: Record<string, string>;
  /** 独立路由（模型级/组成员的独立账号顺序） */
  independent: boolean;
  disabled: boolean;
  /** 打开完整「消耗规则」弹窗（账号页 AccountSortDialog 的同一入口） */
  onSettings?(): void;
  /** 立即保存一家 provider 的规则（A2A `saveGroupSortRules`） */
  onSaveSorting?(provider: string, rules: AccountSortRule[]): Promise<void>;
}): JSX.Element {
  const { t } = useTranslation();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const configured = (provider && sorting?.[provider]) || [];
  const active = configured.filter((rule) => rule.on);
  const credit = active.find((rule) => rule.key === 'credit');
  const balance = active.find((rule) => rule.key === 'balance');
  /** 可编辑 = 有目录数据 + 非独立路由 + 有保存口子（A2A：独立路由不受全局消耗规则影响） */
  const editable = Boolean(provider && sorting && !independent && onSaveSorting);

  const summary = independent
    ? t('独立账号顺序')
    : active.length
      ? active
        .map((rule) => rule.key === 'credit'
          ? (rule.dir === 'soon' ? t('到期 近→远') : t('到期 远→近'))
          : rule.key === 'balance'
            ? (rule.dir === 'high' ? t('余额大优先') : t('余额小优先'))
            : (rule.dir === 'new' ? t('新账号优先') : t('老账号优先')))
        .join(' → ')
      : t('仅按手动顺序');

  const changeRule = async (key: 'credit' | 'balance', value: string): Promise<void> => {
    if (!editable || saving || !provider || !onSaveSorting) return;
    const rule: AccountSortRule = key === 'credit'
      ? { key, on: value !== 'off', dir: value === 'late' ? 'late' : 'soon' }
      : { key, on: value !== 'off', dir: value === 'low' ? 'low' : 'high' };
    const rest = configured.filter((item) => item.key !== key);
    const next = [...rest, rule].sort((a, b) => {
      const order = { credit: 0, balance: 1, added: 2 };
      return order[a.key] - order[b.key];
    });
    setSaving(true);
    setError('');
    try {
      await onSaveSorting(provider, next);
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="rt-consumption-order">
      <ArrowUpDown size={13} aria-hidden="true" />
      <span>{t('消耗顺序')}</span>
      {editable ? (
        <>
          <RoutingSelect
            className="rt-credit-sort"
            label={t('积分到期时间')}
            value={credit ? (credit.on ? credit.dir : 'off') : 'off'}
            disabled={disabled || saving}
            options={[
              ['off', t('不按到期排序')],
              ['soon', t('到期 近→远')],
              ['late', t('到期 远→近')],
            ]}
            onChange={(value) => void changeRule('credit', value)}
          />
          <RoutingSelect
            className="rt-credit-sort"
            label={t('可用余额')}
            value={balance ? (balance.on ? balance.dir : 'off') : 'off'}
            disabled={disabled || saving}
            options={[
              ['off', t('不按余额排序')],
              ['high', t('余额大优先')],
              ['low', t('余额小优先')],
            ]}
            onChange={(value) => void changeRule('balance', value)}
          />
        </>
      ) : null}
      <button
        type="button"
        className="rt-consumption-link"
        disabled={disabled || saving || !onSettings}
        title={
          independent
            ? t('独立路由的账号顺序不受全局消耗规则影响')
            : t('调整到期时间、可用余额及其他消耗规则的优先级')
        }
        onClick={onSettings}
      >
        {editable ? t('消耗规则') : summary}
        <ChevronRight size={11} aria-hidden="true" />
      </button>
      {editable && active.length ? (
        <small className="rt-consumption-note">
          {t('优先级：{{rules}}；前一项相同才比较后一项。', { rules: summary })}
        </small>
      ) : null}
      {editable && balance && provider && balanceSortStates?.[provider] === 'mixedUnits' ? (
        <small className="rt-consumption-note rt-consumption-warn">
          {t('账号余额单位不一致，暂不按余额排序，继续比较其他规则')}
        </small>
      ) : null}
      {editable && balance && provider && balanceSortStates?.[provider] === 'noData' ? (
        <small className="rt-consumption-note">
          {t('暂无可比较余额；查到同单位的数值后自动生效，比例与无限额度不参与')}
        </small>
      ) : null}
      {error ? <small className="rt-consumption-note rt-consumption-warn" role="alert">{error}</small> : null}
      {!editable ? (
        <small className="rt-consumption-note">
          {independent
            ? t('独立路由的账号顺序不受全局消耗规则影响')
            : sorting
              ? t('账号消耗规则（积分到期 / 可用余额）与账号页「排序规则」是同一份配置。')
              : t('尚未读取到消耗规则（/api/accounts/sorting），完整规则在账号页「排序规则」维护。')}
        </small>
      ) : null}
    </div>
  );
}
