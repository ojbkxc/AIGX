import { useMemo, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { ArrowRight, Search } from 'lucide-react';
import { CONFIG_INDEX, matchConfigEntries, type ConfigEntry } from '../lib/config-index';
import { isAdmin } from '../lib/utils';
import './SettingsOverview.css';

/**
 * 配置总览 —— 「有哪些配置项、各自在哪一页」一屏看完。
 *
 * 存在的理由：AIGX 的配置面折在二级/三级 Tab 里（易支付在「套餐与兑换码 ▸ 计费设置」，
 * OAuth 在「系统设置 ▸ OAuth 登录」），而侧栏一级菜单只写页名、不含「易支付」「github」
 * 这些词。管理员按记忆去「系统设置」找易支付，看到的是一个没有它的页面，于是判断
 * 「线上没有这个菜单」。
 *
 * 本组件不改任何页面的信息架构，只把 `config-index.ts` 的描述渲染成一张带跳转的表：
 *   - 一进去就看到「有意支付配置 / 在套餐与兑换码 ▸ 计费设置」；
 *   - 点一行直接落到落实的 Tab（而不是该页的默认 Tab）；
 *   - 顶部搜索框可按别名过滤（输 github / 谷歌 / epay 都能命中）。
 *
 * 挂载点由调用方决定（当前挂在系统设置首页，见 Settings.tsx）。
 */
export default function SettingsOverview(): JSX.Element {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [q, setQ] = useState('');

  const admin = isAdmin();
  const entries = useMemo(() => matchConfigEntries(q, t, admin), [q, t, admin]);

  // 分组只影响视觉节奏：支付/登录是用户最常找的两块，放最前。
  const grouped = useMemo(() => {
    const order: Array<{ key: string; label: string; items: ConfigEntry[] }> = [
      { key: 'payment', label: '支付与计价', items: [] },
      { key: 'auth', label: '登录与账号', items: [] },
      { key: 'gateway', label: '网关与上游', items: [] },
      { key: 'security', label: '安全与准入', items: [] },
      { key: 'limits', label: '限额与请求约束', items: [] },
      { key: 'ops', label: '运维与信息', items: [] },
    ];
    for (const entry of entries) {
      const bucket = order.find((g) => g.key === entry.category);
      // 未知 category 不丢：兜到最后一组，宁可归错组也不能让配置从总览里消失。
      (bucket ?? order[order.length - 1]).items.push(entry);
    }
    return order.filter((g) => g.items.length > 0);
  }, [entries]);

  return (
    <div>
      <div className="card">
        <div className="card-header">
          <h2>{t('配置总览')}</h2>
          <p className="card-subtitle">
            {t('所有配置项与它们的实际位置。点一行直接跳过去——易支付、OAuth 等不在本页，在下面标的那一页里。')}
          </p>
        </div>
        <div className="card-body">
          <div className="config-overview-search">
            <Search size={15} />
            <input
              type="search"
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder={t('搜索配置项，如 易支付 / github / 谷歌 / linuxdo / 超时…')}
              aria-label={t('搜索配置项')}
            />
            {q && (
              <button type="button" className="btn btn-outline btn-sm" onClick={() => setQ('')}>
                {t('清空')}
              </button>
            )}
          </div>

          {grouped.length === 0 ? (
            <div className="config-overview-empty">
              {t('没有匹配「{{q}}」的配置项。换个别名试试：易支付 / epay / github / oauth。', { q })}
            </div>
          ) : (
            grouped.map((group) => (
              <section key={group.key} className="config-overview-group">
                <h3>{t(group.label)}</h3>
                <ul>
                  {group.items.map((entry) => (
                    <li key={entry.id}>
                      <button
                        type="button"
                        className="config-overview-row"
                        onClick={() => navigate(entry.path)}
                      >
                        <span className="config-overview-name">{t(entry.label)}</span>
                        {/* 位置用原文渲染：它描述的是**中文界面里的** Tab 名，
                            跟着界面语言翻译反而对不上用户眼前那一行字。 */}
                        <span className="config-overview-where">{entry.hint}</span>
                        <ArrowRight size={14} className="config-overview-go" />
                      </button>
                    </li>
                  ))}
                </ul>
              </section>
            ))
          )}
        </div>
      </div>

      {admin && (
        <div className="card" style={{ marginTop: 16 }}>
          <div className="card-header">
            <h2>{t('找不到某个配置？')}</h2>
          </div>
          <div className="card-body">
            <p style={{ margin: 0, fontSize: 13, lineHeight: 1.8 }}>
              {t('按 Ctrl+K（Mac 为 ⌘K）打开全局搜索，输入配置名或别名即可直达，例如「易支付」「github」「linuxdo」。')}
              {' '}
              {t('全部 {{n}} 个配置项已登记在总览里；总览里没有的，说明这个功能当前不存在。', {
                n: CONFIG_INDEX.length,
              })}
            </p>
          </div>
        </div>
      )}
    </div>
  );
}