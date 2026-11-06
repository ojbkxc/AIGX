// 渠道健康面板（自旧 Dashboard.tsx :1163 迁入，2026-11-06 重构）：
// 顶部汇总条（健康/警告/异常/熔断计数）+ 筛选排序栏（状态筛选 / 名称搜索 /
// 错误率·请求量·延迟·成功率排序，默认异常·熔断在前健康在后）+ 精简卡片
// （名称+综合状态徽章、大号成功率、错误率/延迟/请求量；最近错误、连续空响应、
// 实时错误率折叠到详情行）+ 数据新鲜度（updatedAt 相对时间）。
// 状态口径全部前端计算，不写死到后端：成功率 >=95 健康 / >=80 警告 / <80 异常，
// circuit_breaker=open 视为熔断（卡片徽章优先于成功率分级），halfopen 徽章显示半开。
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { fmtInt } from '../../lib/format';
import type { ChannelHealthRow } from '../../hooks/useDashboard';

type Cat = 'circuit' | 'halfopen' | 'abnormal' | 'warning' | 'healthy';
type SortKey = 'default' | 'error' | 'requests' | 'latency' | 'success';
type SortDir = 'desc' | 'asc';

/** 成功率（%）：优先后端 success_rate，缺失时按 success_count/total_requests 兜底 */
function rateOf(c: ChannelHealthRow): number {
  const totalReq = c.total_requests ?? 0;
  return c.success_rate != null ? c.success_rate : totalReq > 0 ? ((c.success_count ?? 0) / totalReq) * 100 : 0;
}

/** 错误率（%）：优先后端 error_rate，缺失时用总请求减成功数推导 */
function errorRateOf(c: ChannelHealthRow): number {
  const totalReq = c.total_requests ?? 0;
  return c.error_rate != null ? c.error_rate : totalReq > 0 ? ((totalReq - (c.success_count ?? 0)) / totalReq) * 100 : 0;
}

/** 断路器状态（后端已是机器可读小写枚举 open/halfopen/closed） */
function breakerOf(c: ChannelHealthRow): string {
  return String(c.circuit_breaker || 'closed').toLowerCase();
}

/** 综合状态：熔断/半开优先，其次按成功率阈值分级 */
function categoryOf(c: ChannelHealthRow): Cat {
  const b = breakerOf(c);
  if (b === 'open') return 'circuit';
  if (b === 'halfopen') return 'halfopen';
  const r = rateOf(c);
  if (r >= 95) return 'healthy';
  if (r >= 80) return 'warning';
  return 'abnormal';
}

const CAT_STYLE: Record<Cat, { badgeBg: string; badgeColor: string; dot: string }> = {
  circuit: { badgeBg: 'rgba(239,68,68,0.15)', badgeColor: 'rgb(239,68,68)', dot: 'rgb(239,68,68)' },
  abnormal: { badgeBg: 'rgba(239,68,68,0.15)', badgeColor: 'rgb(239,68,68)', dot: 'rgb(239,68,68)' },
  halfopen: { badgeBg: 'rgba(234,179,8,0.15)', badgeColor: 'rgb(234,179,8)', dot: 'rgb(234,179,8)' },
  warning: { badgeBg: 'rgba(234,179,8,0.15)', badgeColor: 'rgb(234,179,8)', dot: 'rgb(234,179,8)' },
  healthy: { badgeBg: 'rgba(34,197,94,0.15)', badgeColor: 'rgb(34,197,94)', dot: 'rgb(34,197,94)' },
};

/** 徽章文案 key（全部复用既有 i18n 词条，见 i18n/zh.json · en.json） */
const CAT_LABEL: Record<Cat, string> = {
  circuit: '熔断',
  abnormal: '异常',
  halfopen: '半开',
  warning: '警告',
  healthy: '健康',
};

/** 严重度（越小越靠前）：默认排序用，异常/熔断在前、健康在后 */
const SEVERITY: Record<Cat, number> = { circuit: 0, abnormal: 1, halfopen: 2, warning: 3, healthy: 4 };

export default function ChannelHealth({
  data,
  lang,
  updatedAt,
}: {
  data: ChannelHealthRow[];
  lang?: string;
  /** 数据更新时间戳（ms），用于「N 秒前更新」新鲜度显示；未传则不显示 */
  updatedAt?: number;
}): JSX.Element {
  const { t } = useTranslation();
  const [filter, setFilter] = useState<'all' | 'abnormal' | 'warning' | 'circuit'>('all');
  const [search, setSearch] = useState('');
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({ key: 'default', dir: 'desc' });
  const [now, setNow] = useState(() => Date.now());

  // 新鲜度每 15s 重算一次；未传 updatedAt 时不起定时器
  useEffect(() => {
    if (updatedAt == null) return;
    const timer = setInterval(() => setNow(Date.now()), 15000);
    return () => clearInterval(timer);
  }, [updatedAt]);

  // 汇总条：健康/警告/异常按成功率三档计，熔断单独按断路器 open 计
  // （一个渠道可同时计入异常与熔断，四项之和可能大于渠道总数）
  const counts = useMemo(() => {
    const c = { healthy: 0, warning: 0, abnormal: 0, circuit: 0 };
    for (const ch of data) {
      const r = rateOf(ch);
      if (r >= 95) c.healthy += 1;
      else if (r >= 80) c.warning += 1;
      else c.abnormal += 1;
      if (breakerOf(ch) === 'open') c.circuit += 1;
    }
    return c;
  }, [data]);

  // 筛选 + 搜索 + 排序（口径与汇总条一致，全部前端计算）
  const rows = useMemo(() => {
    const q = search.trim().toLowerCase();
    const filtered = data.filter((c) => {
      if (filter !== 'all') {
        const r = rateOf(c);
        if (filter === 'circuit') {
          if (breakerOf(c) !== 'open') return false;
        } else if (filter === 'abnormal' && !(r < 80)) {
          return false;
        } else if (filter === 'warning' && !(r >= 80 && r < 95)) {
          return false;
        }
      }
      if (q && !(c.name ?? `Channel#${c.id ?? ''}`).toLowerCase().includes(q)) return false;
      return true;
    });
    return [...filtered].sort((a, b) => {
      if (sort.key === 'default') {
        const d = SEVERITY[categoryOf(a)] - SEVERITY[categoryOf(b)];
        return d !== 0 ? d : errorRateOf(b) - errorRateOf(a);
      }
      const dir = sort.dir === 'desc' ? -1 : 1;
      switch (sort.key) {
        case 'error': return (errorRateOf(a) - errorRateOf(b)) * dir;
        case 'requests': return ((a.total_requests ?? 0) - (b.total_requests ?? 0)) * dir;
        case 'latency': return ((a.avg_latency_ms ?? 0) - (b.avg_latency_ms ?? 0)) * dir;
        case 'success': return (rateOf(a) - rateOf(b)) * dir;
      }
    });
  }, [data, filter, search, sort]);

  // 数据新鲜度：「N 秒前更新」，跨档位用分钟/小时文案
  const elapsed = updatedAt != null ? Math.max(0, Math.floor((now - updatedAt) / 1000)) : null;
  const freshLabel = elapsed != null
    ? elapsed < 60
      ? t('{{n}} 秒前更新', { n: elapsed })
      : elapsed < 3600
        ? t('{{n}} 分钟前更新', { n: Math.floor(elapsed / 60) })
        : t('{{n}} 小时前更新', { n: Math.floor(elapsed / 3600) })
    : null;

  const filters: Array<{ key: 'all' | 'abnormal' | 'warning' | 'circuit'; label: string; color: string }> = [
    { key: 'all', label: t('全部'), color: 'var(--text-main)' },
    { key: 'abnormal', label: t('异常'), color: CAT_STYLE.abnormal.dot },
    { key: 'warning', label: t('警告'), color: CAT_STYLE.warning.dot },
    { key: 'circuit', label: t('熔断'), color: CAT_STYLE.circuit.dot },
  ];
  const sorts: Array<{ key: SortKey; label: string }> = [
    { key: 'default', label: t('默认排序') },
    { key: 'error', label: t('错误率') },
    { key: 'requests', label: t('请求量') },
    { key: 'latency', label: t('延迟') },
    { key: 'success', label: t('成功率') },
  ];

  return (
    <div>
      {/* 顶部状态汇总条 */}
      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginBottom: 12 }}>
        {(['healthy', 'warning', 'abnormal', 'circuit'] as const).map((cat) => {
          const st = CAT_STYLE[cat];
          return (
            <span key={cat} style={{
              display: 'inline-flex', alignItems: 'center', gap: 6,
              padding: '4px 10px', borderRadius: 999, fontSize: 12,
              background: 'var(--surface-2)', color: 'var(--text-muted)',
            }}>
              <span style={{ width: 8, height: 8, borderRadius: '50%', background: st.dot, display: 'inline-block' }} />
              {t(CAT_LABEL[cat])} <b style={{ color: st.badgeColor }}>{counts[cat]}</b>
            </span>
          );
        })}
      </div>

      {/* 筛选排序栏：状态筛选 + 搜索 + 排序 + 数据新鲜度 */}
      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center', marginBottom: 12 }}>
        <div style={{ display: 'flex', gap: 4, flexWrap: 'wrap' }}>
          {filters.map((f) => {
            const active = filter === f.key;
            return (
              <button key={f.key} type="button" onClick={() => setFilter(f.key)} style={{
                padding: '3px 10px', borderRadius: 999, fontSize: 12,
                border: `1px solid ${active ? 'transparent' : 'var(--border-color)'}`,
                background: active ? f.color : 'transparent',
                color: active ? 'var(--bg-color)' : 'var(--text-muted)',
                cursor: 'pointer',
              }}>
                {f.label}
              </button>
            );
          })}
        </div>
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t('搜索')}
          style={{
            height: 26, width: 150, padding: '0 10px', borderRadius: 999,
            fontSize: 12, border: '1px solid var(--input-border)',
            background: 'var(--input-bg)', color: 'var(--input-text)',
          }}
        />
        <div style={{ display: 'flex', gap: 4, marginLeft: 'auto' }}>
          {sorts.map((s) => {
            const active = sort.key === s.key && s.key !== 'default';
            const arrow = active ? (sort.dir === 'desc' ? ' ↓' : ' ↑') : '';
            return (
              <button key={s.key} type="button" onClick={() => setSort((prev) => (
                prev.key === s.key
                  ? { key: s.key, dir: prev.dir === 'desc' ? 'asc' : 'desc' }
                  : { key: s.key, dir: 'desc' }
              ))} style={{
                padding: '3px 10px', borderRadius: 999, fontSize: 12,
                border: `1px solid ${sort.key === s.key ? 'transparent' : 'var(--border-color)'}`,
                background: sort.key === s.key ? 'var(--text-main)' : 'transparent',
                color: sort.key === s.key ? 'var(--bg-color)' : 'var(--text-muted)',
                cursor: 'pointer',
              }}>
                {s.label}{s.key === 'default' ? '' : arrow}
              </button>
            );
          })}
        </div>
        {freshLabel && (
          <span style={{ fontSize: 12, color: 'var(--text-faint)', whiteSpace: 'nowrap' }}>{freshLabel}</span>
        )}
      </div>

      {/* 卡片网格：异常/熔断默认置顶（由 rows 排序保证） */}
      {rows.length === 0 ? (
        <div className="empty-state" style={{ padding: 22, textAlign: 'center' }}>
          <p style={{ margin: 0 }}>{t('无匹配渠道')}</p>
        </div>
      ) : (
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(240px, 1fr))', gap: 12 }}>
          {rows.map((c, i) => {
            const totalReq = c.total_requests ?? 0;
            const successRate = rateOf(c);
            const errorRate = errorRateOf(c);
            const cat = categoryOf(c);
            const st = CAT_STYLE[cat];
            // 次要信息折叠到详情行
            const details: Array<{ label: string; value: string; color?: string }> = [];
            if (c.health?.last_error) {
              details.push({ label: t('最近错误'), value: c.health.last_error });
            }
            if (c.consecutive_empty != null && c.consecutive_empty > 0) {
              details.push({
                label: t('连续空响应'),
                value: String(c.consecutive_empty),
                color: c.consecutive_empty >= 3 ? CAT_STYLE.abnormal.badgeColor : 'var(--text-main)',
              });
            }
            if (c.health?.error_rate != null && c.health.error_rate > 0) {
              details.push({ label: t('实时错误率'), value: `${(c.health.error_rate * 100).toFixed(1)}%` });
            }
            return (
              <div key={c.id ?? i} className="stat-card" style={{ padding: 16 }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: 8 }}>
                  <span style={{ fontWeight: 600, fontSize: 14, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={c.name ?? undefined}>
                    {c.name || `Channel#${c.id ?? i}`}
                  </span>
                  <span style={{
                    padding: '2px 8px', borderRadius: 999, fontSize: 11, flexShrink: 0,
                    background: st.badgeBg, color: st.badgeColor,
                  }}>
                    {t(CAT_LABEL[cat])}
                  </span>
                </div>
                <div style={{ marginTop: 10, display: 'flex', alignItems: 'baseline', gap: 6 }}>
                  <span style={{ fontSize: 28, fontWeight: 700, color: st.badgeColor, lineHeight: 1 }}>{successRate.toFixed(1)}%</span>
                  <span style={{ fontSize: 12, color: 'var(--text-muted)' }}>{t('成功率')}</span>
                </div>
                <div style={{ marginTop: 10, display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '4px 12px', fontSize: 13 }}>
                  <div style={{ color: 'var(--text-muted)' }}>
                    {t('错误率')}: <span style={{ color: errorRate > 20 ? CAT_STYLE.abnormal.badgeColor : 'var(--text-main)', fontWeight: 600 }}>{errorRate.toFixed(1)}%</span>
                  </div>
                  <div style={{ color: 'var(--text-muted)' }}>
                    {t('平均延迟')}: <span style={{ color: 'var(--text-main)' }}>{fmtInt(c.avg_latency_ms || 0, lang)}ms</span>
                  </div>
                  <div style={{ color: 'var(--text-muted)', gridColumn: 'span 2' }}>
                    {t('请求数')}: <span style={{ color: 'var(--text-main)' }}>{fmtInt(totalReq || c.requests || 0, lang)}</span>
                  </div>
                </div>
                {details.length > 0 && (
                  <div style={{ marginTop: 8, paddingTop: 8, borderTop: '1px solid var(--border-color)', fontSize: 12 }}>
                    {details.map((d, j) => (
                      <div key={j} style={{ color: 'var(--text-muted)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={d.value}>
                        {d.label}: <span style={{ color: d.color ?? 'var(--text-main)', fontWeight: 600 }}>{d.value}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
