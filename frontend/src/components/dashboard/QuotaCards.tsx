// 个人配额三卡（总配额 / 已用+进度条 / 剩余）——普通用户视图头部，
// 自旧 Dashboard.tsx :710 迁入。已耗 >80% 红、>50% 黄。
import { useTranslation } from 'react-i18next';
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
          <div className="stat-title">{t('总配额')}</div>
          <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>💰</div>
        </div>
        <div className="stat-value" style={{ fontSize: '26px' }}>{monthlyLimit != null ? fmtInt(monthlyLimit, lang) : '∞'}</div>
        <div className="stat-desc" style={{ fontSize: '11px' }}>{t('充值与赠送总额')}</div>
      </div>
      <div className="stat-card">
        <div className="stat-card-top">
          <div className="stat-title">{t('已用配额')}</div>
          <div className="stat-icon-badge" style={{ background: 'rgba(251, 146, 60, 0.15)', color: '#fb923c' }}>📈</div>
        </div>
        <div className="stat-value" style={{ fontSize: '26px' }}>{fmtInt(monthlyUsed, lang)}</div>
        <div className="stat-desc" style={{ fontSize: '11px' }}>
          {pct != null && <span>{pct.toFixed(1)}% {t('已消耗')}</span>}
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
      <div className="stat-card">
        <div className="stat-card-top">
          <div className="stat-title">{t('剩余配额')}</div>
          <div className="stat-icon-badge" style={{ background: 'rgba(16, 185, 129, 0.15)', color: '#34d399' }}>✅</div>
        </div>
        <div className="stat-value" style={{ fontSize: '26px' }}>
          {fmtInt(Math.max(0, (monthlyLimit ?? 0) - monthlyUsed), lang)}
        </div>
        <div className="stat-desc" style={{ fontSize: '11px' }}>
          {monthlyLimit == null ? t('未设限额') : t('按当前配额计算')}
        </div>
      </div>
    </div>
  );
}
