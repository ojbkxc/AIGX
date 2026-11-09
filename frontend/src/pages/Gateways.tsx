import { useEffect, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Tabs } from '../components/ui';
import Channels from './Channels';
import AccountSessions from './AccountSessions';
import Diagnostics from './Diagnostics';
import CheckinCenter from './CheckinCenter';
import GatewayRuntimePanel from '../components/GatewayRuntimePanel';
import './Gateways.css';

/**
 * 供应商 —— 单页四 Tab。
 *
 * 为什么合并：`channel_type='account'` 的渠道**只是账号池的内部容器**
 * （base_url / api_key 留空，凭证与调度由 account_session 负责），
 * 上游 Agent2API 也只有 provider → account 两层。2026-11 IA 整合后，
 * 上游渠道与账号池合并成一张「供应商」表：账号池型供应商在行内显示
 * 账号数并可下钻到「账号池」Tab 明细（?tab=accounts&provider=<id>）。
 *
 * Tab 清单（合并史）：
 * - 供应商 / 账号池：2026-10 双 Tab 合并；2026-11 供应商列表不再过滤
 *   账号池型（统一列表 + 形态列），「上游渠道」Tab 更名「供应商」。
 * - 健康：2026-11-07 原独立「系统诊断」页并入（07 E.3）——诊断三件套
 *   （体检摘要/批量测活/熔断器）全部以渠道为粒度，看到问题就在旁边修。
 * - 网关：原独立 Tab 于 2026-11 移除，只读面板降级为供应商 Tab 顶部的
 *   可折叠信息条（默认收起）；编辑入口仍在「系统设置 → 网关」单一表单。
 * - 签到：2026-11 原独立「签到中心」页并入（每日额度领取与账号池强耦合）。
 *
 * 四个 Tab 都是各自独立加载的组件：首次访问只挂载当前 Tab，切过去
 * 才挂载另一个（之后保留在 DOM 里，切回不重新拉数据、不丢筛选状态）。
 */
type GatewayTab = 'providers' | 'accounts' | 'health' | 'checkin';

const TAB_KEYS: GatewayTab[] = ['providers', 'accounts', 'health', 'checkin'];

/** 旧 Tab 名兼容：upstream / gateway → providers（书签与外链不 404） */
const TAB_ALIASES: Record<string, GatewayTab> = {
  upstream: 'providers',
  gateway: 'providers',
};

export default function Gateways(): JSX.Element {
  const { t } = useTranslation();
  const [params, setParams] = useSearchParams();
  const tabParam = params.get('tab');
  const tab: GatewayTab = TAB_KEYS.includes(tabParam as GatewayTab)
    ? (tabParam as GatewayTab)
    : (TAB_ALIASES[tabParam ?? ''] ?? 'providers');
  /** 账号池下钻：?provider=<channel_id> 作为 AccountSessions 的初值 */
  const providerFilter = params.get('provider') ?? '';

  // 已访问过的 Tab 集合：挂载过就不卸载（切换只切显隐）
  const [mounted, setMounted] = useState<Set<GatewayTab>>(() => new Set([tab]));

  // 外部导航（供应商行的「管理账号」navigate 到 ?tab=accounts&provider=）
  // 会改 URL 但不经过 change()，必须把当前 tab 补进已挂载集合，
  // 否则面板 hidden 容器里 mounted 判定为 false → 空白。
  useEffect(() => {
    setMounted((prev) => (prev.has(tab) ? prev : new Set(prev).add(tab)));
  }, [tab]);

  // 旧 Tab 别名归一：URL 改写成默认形态（providers 不带 tab 参数），
  // 用 replace 避免在浏览器历史里留一条中间态。
  useEffect(() => {
    const raw = params.get('tab');
    if (!raw || !(raw in TAB_ALIASES)) return;
    const next = new URLSearchParams(params);
    next.delete('tab');
    setParams(next, { replace: true });
  }, [params, setParams]);

  const change = (next: GatewayTab): void => {
    if (next === tab) return;
    setMounted((prev) => (prev.has(next) ? prev : new Set(prev).add(next)));
    const nextParams = new URLSearchParams(params);
    if (next === 'providers') nextParams.delete('tab');
    else nextParams.set('tab', next);
    // 离开账号池 Tab 时清掉下钻参数：否则供应商列表里的「管理账号」留下的
    // ?provider= 会一直粘着，下一次手动进账号池仍按旧供应商过滤。
    if (next !== 'accounts') nextParams.delete('provider');
    setParams(nextParams, { replace: true });
  };

  return (
    <div>
      <div className="page-header gw-header">
        <h1>{t('供应商')}</h1>
        <p>
          {t(
            'API Key 型供应商保存 Key / 模型映射 / 成本价；账号池型供应商管理登录态账号的多凭证轮换。账号池 Tab 查看各供应商的账号明细，健康页承接体检、测活与熔断器快照，签到页为账号池账号领取每日额度。'
          )}
        </p>
      </div>

      <Tabs<GatewayTab>
        className="gw-tabs"
        ariaLabel={t('供应商')}
        items={[
          { key: 'providers', label: t('供应商') },
          { key: 'accounts', label: t('账号池') },
          { key: 'health', label: t('健康') },
          { key: 'checkin', label: t('签到') },
        ]}
        active={tab}
        onChange={change}
      />

      <div hidden={tab !== 'providers'}>
        {mounted.has('providers') && (
          <>
            {/* 网关 Tab 已移除：只读运行参数收进可折叠信息条（默认收起）。
                编辑入口唯一（系统设置 → 网关），这里不再产生第二份表单。 */}
            <details className="gw-runtime-collapse">
              <summary>{t('网关运行参数（只读）')}</summary>
              <GatewayRuntimePanel />
            </details>
            <Channels embedded />
          </>
        )}
      </div>
      <div hidden={tab !== 'accounts'}>
        {/* key=providerFilter：从供应商行「管理账号」下钻时强制重挂 AccountSessions，
            让 initialChannelFilter 生效（组件只把初值读进 useState，不改其内部逻辑）。
            无 provider 参数时 key 恒为 ''，普通切 Tab 不重挂、不丢筛选状态。 */}
        {mounted.has('accounts') && (
          <AccountSessions key={providerFilter} embedded initialChannelFilter={providerFilter} />
        )}
      </div>
      <div hidden={tab !== 'health'}>
        {mounted.has('health') && <Diagnostics embedded />}
      </div>
      <div hidden={tab !== 'checkin'}>
        {mounted.has('checkin') && <CheckinCenter embedded />}
      </div>
    </div>
  );
}
