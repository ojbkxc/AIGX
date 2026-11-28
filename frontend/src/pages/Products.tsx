import { useEffect, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Tabs } from '../components/ui';
import PageHeader from '../components/PageHeader';
import { api } from '../api';
import { useToast } from '../components/Toast';
import Plans from './Plans';
import Redemptions from './Redemptions';
import Epay from './Epay';
import Groups from './Groups';
import './Products.css';

/**
 * 商品 —— 套餐 / 兑换码 / 计费设置 三 Tab。
 *
 * 三个 Tab 各自独立加载：首次访问只挂载当前 Tab，切过去才挂载另一个
 * （之后保留在 DOM 里，切回不重新拉数据、不丢筛选状态）。
 *
 * 「计费设置」Tab 迁自「系统设置 → 计费」：易支付 + 用户分组（子标签）+
 * 按次计费（billing_flat_quota）+ 价格同步，面板组件直接复用，逻辑不重复。
 */
type ProductTab = 'plans' | 'redemptions' | 'billing';

const TAB_KEYS: ProductTab[] = ['plans', 'redemptions', 'billing'];

interface PriceSyncConfig {
  enabled?: boolean;
  sync_url?: string;
  interval_secs?: number | null;
}

/** 「计费设置」Tab：易支付 / 用户分组 / 按次计费 / 价格同步。 */
function BillingSettings(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  /**
   * 子标签也走深链：`/groups` 这类旧入口会落到 `?tab=billing&sub=groups`。
   * 只认这两个值，非法值回落 `epay`。
   */
  const [sub, setSub] = useState<'epay' | 'groups'>(
    new URLSearchParams(window.location.search).get('sub') === 'groups'
      ? 'groups'
      : 'epay',
  );

  /** 子标签切换同步写回 query，让 \/groups\ 这类深链能被复制/刷新保持。 */
  const changeSub = (next: 'epay' | 'groups'): void => {
    setSub(next);
    const nextParams = new URLSearchParams(window.location.search);
    if (next === 'epay') nextParams.delete('sub');
    else nextParams.set('sub', next);
    window.history.replaceState({}, '', '?' + nextParams.toString());
  };

  // 按次计费（billing_flat_quota，走通用 limits 端点；只取/存这一个字段）
  const [flatQuota, setFlatQuota] = useState('');
  const [quotaSaving, setQuotaSaving] = useState(false);

  // 价格同步
  const [priceSync, setPriceSync] = useState<PriceSyncConfig | null>(null);
  const [priceSyncLoading, setPriceSyncLoading] = useState(false);
  const [priceSyncSaving, setPriceSyncSaving] = useState(false);
  const [priceSyncTriggering, setPriceSyncTriggering] = useState(false);

  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const res = (await api.getLimits()) as
          | { data?: { billing_flat_quota?: number } }
          | { billing_flat_quota?: number };
        if (!alive) return;
        const data = (res as { data?: { billing_flat_quota?: number } }).data
          ?? (res as { billing_flat_quota?: number });
        setFlatQuota(data?.billing_flat_quota != null ? String(data.billing_flat_quota) : '');
      } catch {
        // 读不到就不填，保持空（保存时仍可写）
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  const loadPriceSync = async (): Promise<void> => {
    setPriceSyncLoading(true);
    try {
      const res = (await api.getPriceSyncConfig()) as
        | { data?: PriceSyncConfig }
        | PriceSyncConfig;
      setPriceSync((res as { data?: PriceSyncConfig }).data ?? (res as PriceSyncConfig));
    } catch {
      // 价格同步可能未启用，静默处理
    } finally {
      setPriceSyncLoading(false);
    }
  };

  useEffect(() => {
    void loadPriceSync();
  }, []);

  const saveQuota = async (): Promise<void> => {
    const n = Number(flatQuota);
    if (!Number.isFinite(n) || n <= 0) {
      addToast(t('按次计费固定额度必须大于 0'), 'error');
      return;
    }
    setQuotaSaving(true);
    try {
      await api.updateLimits({ billing_flat_quota: n });
      addToast(t('设置更新成功'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setQuotaSaving(false);
    }
  };

  const savePriceSync = async (): Promise<void> => {
    setPriceSyncSaving(true);
    try {
      await api.updatePriceSyncConfig(priceSync as unknown as Record<string, unknown>);
      addToast(t('价格同步配置已更新'));
      await loadPriceSync();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setPriceSyncSaving(false);
    }
  };

  const triggerPriceSync = async (): Promise<void> => {
    setPriceSyncTriggering(true);
    try {
      await api.triggerPriceSync();
      addToast(t('价格同步已触发，请稍后查看同步结果'));
      await loadPriceSync();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setPriceSyncTriggering(false);
    }
  };

  return (
    <div>
      <Tabs<'epay' | 'groups'>
        ariaLabel={t('计费设置分区')}
        items={[
          { key: 'epay', label: t('易支付配置') },
          { key: 'groups', label: t('用户分组') },
        ]}
        active={sub}
        onChange={changeSub}
      />
      <div style={{ marginTop: 16 }}>
        {sub === 'epay' && <Epay />}
        {sub === 'groups' && <Groups />}
      </div>

      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('按次计费')}</h2>
        </div>
        <div className="card-body">
          <div className="settings-form">
            <div className="form-group">
              <label>{t('按次计费固定额度')}</label>
              <input
                className="form-input"
                type="number"
                min="1"
                placeholder="2"
                value={flatQuota}
                onChange={(e) => setFlatQuota(e.target.value)}
              />
              <span className="form-hint">
                {t('订阅计费模式为「按次」时，每次请求固定扣减的额度（全局统一，默认 2）。')}
              </span>
            </div>
            <div className="settings-actions">
              <button className="btn btn-primary" onClick={() => void saveQuota()} disabled={quotaSaving}>
                {quotaSaving ? t('保存中...') : t('保存更改')}
              </button>
            </div>
          </div>
        </div>
      </div>

      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('价格同步')}</h2>
        </div>
        <div className="card-body">
          {priceSyncLoading && !priceSync ? (
            <p className="form-hint">{t('加载中...')}</p>
          ) : priceSync ? (
            <div className="settings-form">
              <div className="form-group">
                <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <input
                    type="checkbox"
                    checked={Boolean(priceSync.enabled)}
                    onChange={(e) => setPriceSync({ ...priceSync, enabled: e.target.checked })}
                  />
                  {t('启用自动同步')}
                </label>
                <span className="form-hint">{t('开启后按间隔自动从同步 URL 拉取最新价格')}</span>
              </div>
              <div className="form-group">
                <label>{t('同步 URL')}</label>
                <input
                  className="form-input"
                  placeholder="https://example.com/prices.json"
                  value={priceSync.sync_url || ''}
                  onChange={(e) => setPriceSync({ ...priceSync, sync_url: e.target.value })}
                />
                <span className="form-hint">{t('价格数据源的 JSON URL')}</span>
              </div>
              <div className="form-group">
                <label>{t('同步间隔（秒）')}</label>
                <input
                  className="form-input"
                  type="number"
                  min="60"
                  placeholder="3600"
                  value={priceSync.interval_secs ?? ''}
                  onChange={(e) =>
                    setPriceSync({ ...priceSync, interval_secs: e.target.value === '' ? null : Number(e.target.value) })
                  }
                />
                <span className="form-hint">{t('自动同步间隔，建议 >= 300 秒')}</span>
              </div>
              <div className="settings-actions" style={{ display: 'flex', gap: 12 }}>
                <button className="btn btn-primary" onClick={() => void savePriceSync()} disabled={priceSyncSaving}>
                  {priceSyncSaving ? t('保存中...') : t('保存配置')}
                </button>
                <button className="btn btn-outline" onClick={() => void triggerPriceSync()} disabled={priceSyncTriggering}>
                  {priceSyncTriggering ? t('同步中...') : t('立即同步')}
                </button>
              </div>
            </div>
          ) : (
            <div className="empty-state">
              <p>{t('价格同步未启用或加载失败')}</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export default function Products(): JSX.Element {
  const { t } = useTranslation();
  const [params, setParams] = useSearchParams();
  const tabParam = params.get('tab');
  const tab: ProductTab = TAB_KEYS.includes(tabParam as ProductTab)
    ? (tabParam as ProductTab)
    : 'plans';

  const [mounted, setMounted] = useState<Set<ProductTab>>(() => new Set([tab]));

  const change = (next: ProductTab): void => {
    if (next === tab) return;
    setMounted((prev) => (prev.has(next) ? prev : new Set(prev).add(next)));
    const nextParams = new URLSearchParams(params);
    if (next === 'plans') nextParams.delete('tab');
    else nextParams.set('tab', next);
    setParams(nextParams, { replace: true });
  };


  return (
    <div>
      <PageHeader
        title={t('套餐与兑换码')}
        subtitle={t(
          '套餐定义带额度与有效期的商品模板；兑换码是同一份额度的一次性发放凭证，兑换后生成对应额度的密钥。'
        )}
      />

      <Tabs<ProductTab>
        className="pd-tabs"
        ariaLabel={t('套餐与兑换码')}
        items={[
          { key: 'plans', label: t('套餐管理') },
          { key: 'redemptions', label: t('兑换码') },
          { key: 'billing', label: t('计费设置') },
        ]}
        active={tab}
        onChange={change}
      />

      <div hidden={tab !== 'plans'}>
        {mounted.has('plans') && <Plans embedded />}
      </div>
      <div hidden={tab !== 'redemptions'}>
        {mounted.has('redemptions') && <Redemptions embedded />}
      </div>
      <div hidden={tab !== 'billing'}>
        {mounted.has('billing') && <BillingSettings />}
      </div>
    </div>
  );
}
