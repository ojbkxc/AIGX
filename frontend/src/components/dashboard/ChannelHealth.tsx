// 渠道健康卡网格（自旧 Dashboard.tsx :1163 迁入）：断路器状态点、
// 成功率/错误率（错误率 >20% 红）、平均延迟、连续空响应、实时错误率 EMA。
import { useTranslation } from 'react-i18next';
import { fmtInt } from '../../lib/format';
import type { ChannelHealthRow } from '../../hooks/useDashboard';

export default function ChannelHealth({ data, lang }: { data: ChannelHealthRow[]; lang?: string }): JSX.Element {
  const { t } = useTranslation();
  return (
    <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(240px, 1fr))', gap: 12 }}>
      {data.map((c, i) => {
        const totalReq = c.total_requests ?? 0;
        const successRate = c.success_rate != null ? c.success_rate : totalReq > 0 ? ((c.success_count ?? 0) / totalReq) * 100 : 0;
        const errorRate = c.error_rate != null ? c.error_rate : totalReq > 0 ? ((totalReq - (c.success_count ?? 0)) / totalReq) * 100 : 0;
        const isHealthy = successRate >= 95;
        const isWarning = successRate >= 80 && successRate < 95;
        // 断路器状态：后端 circuit_breaker 已为机器可读小写枚举
        // ("open"/"halfopen"/"closed"，见 channel/circuit_breaker.rs get_state)
        const breakerState = String(c.circuit_breaker || 'closed').toLowerCase();
        const breakerColor = breakerState === 'open' ? 'rgb(239,68,68)' : breakerState === 'halfopen' ? 'rgb(234,179,8)' : 'rgb(34,197,94)';
        const breakerLabel = breakerState === 'open' ? t('熔断') : breakerState === 'halfopen' ? t('半开') : t('正常');
        return (
          <div key={c.id ?? i} className="stat-card" style={{ padding: 16 }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: 8 }}>
              <span style={{ fontWeight: 600, fontSize: 14 }}>{c.name || `Channel#${c.id ?? i}`}</span>
              <span style={{
                padding: '2px 8px', borderRadius: 999, fontSize: 11,
                background: isHealthy ? 'rgba(34,197,94,0.15)' : isWarning ? 'rgba(234,179,8,0.15)' : 'rgba(239,68,68,0.15)',
                color: isHealthy ? 'rgb(34,197,94)' : isWarning ? 'rgb(234,179,8)' : 'rgb(239,68,68)',
              }}>
                {isHealthy ? t('健康') : isWarning ? t('警告') : t('异常')}
              </span>
            </div>
            <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4, display: 'flex', alignItems: 'center', gap: 6 }}>
              <span style={{ width: 8, height: 8, borderRadius: '50%', background: breakerColor, display: 'inline-block' }} />
              {t('断路器')}: <span style={{ color: breakerColor, fontWeight: 600 }}>{breakerLabel}</span>
            </div>
            <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4 }}>
              {t('成功率')}: <span style={{ color: 'var(--text-main)', fontWeight: 600 }}>{successRate.toFixed(1)}%</span>
            </div>
            <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4 }}>
              {t('错误率')}: <span style={{ color: errorRate > 20 ? 'rgb(239,68,68)' : 'var(--text-main)', fontWeight: 600 }}>{errorRate.toFixed(1)}%</span>
            </div>
            <div style={{ height: 4, background: 'var(--bg-color)', borderRadius: 2, marginBottom: 4, overflow: 'hidden' }}>
              <div style={{
                width: `${Math.min(100, errorRate)}%`,
                height: '100%',
                background: errorRate > 20 ? 'rgb(239,68,68)' : errorRate > 5 ? 'rgb(234,179,8)' : 'rgb(34,197,94)',
                borderRadius: 2,
                transition: 'width 0.3s ease',
              }} />
            </div>
            <div style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 4 }}>
              {t('平均延迟')}: <span style={{ color: 'var(--text-main)' }}>{fmtInt(c.avg_latency_ms || 0, lang)}ms</span>
            </div>
            {c.consecutive_empty != null && c.consecutive_empty > 0 && (
              <div style={{ fontSize: 13, color: c.consecutive_empty >= 3 ? 'rgb(239,68,68)' : 'var(--text-muted)' }}>
                {t('连续空响应')}: <span style={{ fontWeight: 600 }}>{c.consecutive_empty}</span>
              </div>
            )}
            {c.health?.error_rate != null && c.health.error_rate > 0 && (
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>
                {t('实时错误率')}: <span style={{ color: c.health.error_rate > 0.5 ? 'rgb(234,179,8)' : 'var(--text-main)', fontWeight: 600 }}>
                  {(c.health.error_rate * 100).toFixed(1)}%
                </span>
              </div>
            )}
            {c.health?.last_error && (
              <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 4, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={c.health.last_error}>
                {t('最近错误')}: {c.health.last_error}
              </div>
            )}
            <div style={{ fontSize: 13, color: 'var(--text-muted)', marginTop: 4 }}>
              {t('请求数')}: <span style={{ color: 'var(--text-main)' }}>{fmtInt(totalReq || c.requests || 0, lang)}</span>
            </div>
          </div>
        );
      })}
    </div>
  );
}
