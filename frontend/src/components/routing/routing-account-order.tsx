/**
 * 账号消耗顺序（对齐 Agent2API v3.5.1 `routing-account-order.tsx` 的展示位）。
 *
 * A2A 这里挂两个「按到期 / 按余额」排序下拉，数据来自 `AccountSorting`
 * （账号页的排序配置 + 积分到期时间）。AIGX 的这些口径住在账号页
 * （`Components/AccountSortDialog.tsx`），路由侧没有任何一处读到排序快照 ——
 * 所以本组件只落**同一位置的同一行**：图标 + 「消耗顺序」+ 一个入口，
 * 不渲染空下拉（那会让人以为路由页能改，实际改了不生效）。
 *
 * 数据源缺口记在交付清单；接入后按 A2A 补 `RoutingSelect` 两个下拉即可。
 */
import { useTranslation } from 'react-i18next';
import { ArrowUpDown, ChevronRight } from 'lucide-react';

export function RoutingConsumptionOrder({ independent, disabled, onSettings }: {
  /** 独立路由（模型级/组成员的独立账号顺序） */
  independent: boolean;
  disabled: boolean;
  /** 打开「账号排序」的口子；缺省时入口禁用并给出说明 */
  onSettings?(): void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="rt-consumption-order">
      <ArrowUpDown size={13} aria-hidden="true" />
      <span>{t('消耗顺序')}</span>
      <button
        type="button"
        className="rt-consumption-link"
        disabled={disabled || !onSettings}
        title={
          independent
            ? t('独立路由的账号顺序不受全局消耗规则影响')
            : t('调整到期时间、可用余额及其他消耗规则的优先级')
        }
        onClick={onSettings}
      >
        {independent ? t('独立账号顺序') : t('仅按手动顺序')}
        <ChevronRight size={11} aria-hidden="true" />
      </button>
      <small className="rt-consumption-note">
        {t('账号消耗规则（积分到期 / 可用余额）在账号页维护，本页暂未读取排序快照。')}
      </small>
    </div>
  );
}