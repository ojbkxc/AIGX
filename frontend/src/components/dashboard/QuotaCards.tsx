// 个人配额卡（普通用户视图头部，自旧 Dashboard.tsx :710 迁入）。
// #17 锦上添花：原「总配额 / 已用 / 剩余」三卡合并为单卡+进度条——剩余=总−已用，
// 单卡信息量等价且不再占三格。已耗 >80% 红、>50% 黄。
import { useTranslation } from 'react-i18next';
import { Wallet } from 'lucide-react';
import { fmtInt } from '../../lib/format';

interface Props {
  monthlyUsed: number;
  monthlyLimit: number | null | undefined;
  lang?: string;
}

export default function QuotaCards({ monthlyUsed, monthlyLimit, lang }: Props): JSX.Element {
  const { t } = useTranslation();
  const pct = monthlyLimit != null && monthlyLimit > 0 ? Math.min(100, (monthlyUsed / monthlyLimit) * 100) : null;
  return (
    <div className="stats-grid" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(220px, 1fr))' }}>
      <div className="stat-card">
        <div className="stat-card-top">
          <div className="stat-title">{t('已用配额')}</div>
          <div
            className="stat-icon-badge"
            style={{
              background: 'color-mix(in srgb, var(--amber) 15%, transparent)',
              color: 'var(--amber)',
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
            }}
          >
            <Wallet size={14} />
          </div>
        </div>
        <div className="stat-value" style={{ fontSize: '26px' }}>
          {fmtInt(monthlyUsed, lang)}
          {pct != null && (
            <span style={{ fontSize: 13, marginLeft: 8, color: 'var(--text-muted)' }}>
              {pct.toFixed(1)}% {t('已消耗')}
            </span>
          )}
        </div>
        <div className="stat-desc" style={{ fontSize: '11px' }}>
          {monthlyLimit == null
            ? t('未设限额')
            : `${t('总配额')} ${fmtInt(monthlyLimit, lang)} · ${t('剩余配额')} ${fmtInt(Math.max(0, monthlyLimit - monthlyUsed), lang)}`}
        </div>
        {pct != null && (
          <div style={{ height: 4, background: 'var(--bg-color)', borderRadius: 2, marginTop: 8, overflow: 'hidden' }}>
            <div style={{
              width: `${pct}%`,
              height: '100%',
              background: pct > 80 ? 'rgb(239,68,68)' : pct > 50 ? 'rgb(234,179,8)' : 'var(--accent-color)',
              borderRadius: 2,
              transition: 'width 0.3s ease',
            }} />
          </div>
        )}
      </div>
    </div>
  );
}
