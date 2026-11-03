import { useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { Tabs } from '../components/ui';
import Plans from './Plans';
import Redemptions from './Redemptions';
import './Products.css';

/**
 * 商品 —— 套餐与兑换码合并为单页双 Tab。
 *
 * 为什么合并：两者是同一件事的两个侧面 —— 套餐是「带额度与有效期的商品模板」，
 * 兑换码是「同一份额度的一次性发放凭证」（AIGX 的 once 套餐发出去的就是
 * 一个带额度的密钥）。管理员配置一次商品，再按同一份额度出码，原本要在
 * 两个菜单之间来回对照「面额 / 有效期 / 分组」是否一致。
 *
 * 两个 Tab 各自独立加载：首次访问只挂载当前 Tab，切过去才挂载另一个
 * （之后保留在 DOM 里，切回不重新拉数据、不丢筛选状态）。
 */
type ProductTab = 'plans' | 'redemptions';

const TAB_KEYS: ProductTab[] = ['plans', 'redemptions'];

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
      <div className="page-header">
        <h1>{t('套餐与兑换码')}</h1>
        <p>
          {t(
            '套餐定义带额度与有效期的商品模板；兑换码是同一份额度的一次性发放凭证，兑换后生成对应额度的密钥。'
          )}
        </p>
      </div>

      <Tabs<ProductTab>
        className="pd-tabs"
        ariaLabel={t('套餐与兑换码')}
        items={[
          { key: 'plans', label: t('套餐管理') },
          { key: 'redemptions', label: t('兑换码') },
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
    </div>
  );
}
