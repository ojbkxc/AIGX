import { useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Tabs } from '../components/ui';
import Channels from './Channels';
import AccountSessions from './AccountSessions';
import './Gateways.css';

/**
 * 渠道与账号池 —— 单页双 Tab。
 *
 * 为什么合并：`channel_type='account'` 的渠道**只是账号池的内部容器**
 * （base_url / api_key 留空，凭证与调度由 account_session 负责），
 * 对用户而言没有独立意义。上游 Agent2API 也只有 provider → account 两层。
 * 把「账号型渠道」从渠道列表里隐去、账号池单独一 Tab，用户看到的概念
 * 就与 A2A 一致，同时保留 AIGX 独有的真上游渠道（带 Key/映射/成本价）。
 *
 * 两个 Tab 都是各自独立加载的重组件：首次访问只挂载当前 Tab，切过去
 * 才挂载另一个（之后保留在 DOM 里，切回不重新拉数据、不丢筛选状态）。
 */
type GatewayTab = 'upstream' | 'accounts';

const TAB_KEYS: GatewayTab[] = ['upstream', 'accounts'];

export default function Gateways(): JSX.Element {
  const { t } = useTranslation();
  const [params, setParams] = useSearchParams();
  const tabParam = params.get('tab');
  const tab: GatewayTab = TAB_KEYS.includes(tabParam as GatewayTab)
    ? (tabParam as GatewayTab)
    : 'upstream';

  // 已访问过的 Tab 集合：挂载过就不卸载（切换只切显隐）
  const [mounted, setMounted] = useState<Set<GatewayTab>>(() => new Set([tab]));

  const change = (next: GatewayTab): void => {
    if (next === tab) return;
    setMounted((prev) => (prev.has(next) ? prev : new Set(prev).add(next)));
    const nextParams = new URLSearchParams(params);
    if (next === 'upstream') nextParams.delete('tab');
    else nextParams.set('tab', next);
    setParams(nextParams, { replace: true });
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t('渠道与账号池')}</h1>
        <p>
          {t(
            '上游渠道保存 Key / 模型映射 / 成本价；账号池管理登录态账号的多凭证轮换。账号型渠道是账号池的内部容器，不在此单独展示。'
          )}
        </p>
      </div>

      <Tabs<GatewayTab>
        className="gw-tabs"
        ariaLabel={t('渠道与账号池')}
        items={[
          { key: 'upstream', label: t('上游渠道') },
          { key: 'accounts', label: t('账号池') },
        ]}
        active={tab}
        onChange={change}
      />

      <div hidden={tab !== 'upstream'}>
        {mounted.has('upstream') && <Channels embedded />}
      </div>
      <div hidden={tab !== 'accounts'}>
        {mounted.has('accounts') && <AccountSessions embedded />}
      </div>
    </div>
  );
}
