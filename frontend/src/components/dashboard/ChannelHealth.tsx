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
  const [filter, setFilter] = useState<'all' | Cat>('all');
  const [search, setSearch] = useState('');
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({ key: 'default', dir: 'desc' });
  const [now, setNow] = useState(() => Date.now());

  // 新鲜度每 15s 重算一次；未传 updatedAt 时不起定时器
  useEffect(() => {
    if (updatedAt == null) return;
    const timer = setInterval(() => setNow(Date.now()), 15000);
    return () => clearInterval(timer);
  }, [updatedAt]);

  // 只统计/展示状态为「启用」的供应商（后端 status 取值 enabled/disabled，见
  // legacy.rs handle_channel_health，每条渠道恒带该字段）。非 enabled 一律隐藏，
  // 不参与汇总/筛选/排序。
  const enabledRows = useMemo(
    () => data.filter((c) => c.status === 'enabled'),
    [data],
  );
  const disabledCount = data.length - enabledRows.length;

  // 汇总条：健康/警告/异常按成功率三档计，熔断单独按断路器 open 计
  // （一个渠道可同时计入异常与熔断，四项之和可能大于渠道总数）
  const counts = useMemo(() => {
    const c = { healthy: 0, warning: 0, abnormal: 0, circuit: 0 };
    for (const ch of enabledRows) {
      const r = rateOf(ch);
      if (r >= 95) c.healthy += 1;
      else if (r >= 80) c.warning += 1;
      else c.abnormal += 1;
      if (breakerOf(ch) === 'open') c.circuit += 1;
    }
    return c;
  }, [enabledRows]);

  // 筛选 + 搜索 + 排序（口径与汇总条一致，全部前端计算；已排除停用供应商）
  const rows = useMemo(() => {
    const q = search.trim().toLowerCase();
    const filtered = enabledRows.filter((c) => {
      if (filter !== 'all') {
        const r = rateOf(c);
        if (filter === 'circuit') {
          if (breakerOf(c) !== 'open') return false;
        } else if (filter === 'abnormal' && !(r < 80)) {
          return false;
        } else if (filter === 'warning' && !(r >= 80 && r < 95)) {
          return false;
        } else if (filter === 'healthy' && !(r >= 95)) {
          return false;
        } else if (filter === 'halfopen' && breakerOf(c) !== 'halfopen') {
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
  }, [enabledRows, filter, search, sort]);

  // 数据新鲜度：「N 秒前更新」，跨档位用分钟/小时文案
  const elapsed = updatedAt != null ? Math.max(0, Math.floor((now - updatedAt) / 1000)) : null;
  const freshLabel = elapsed != null
    ? elapsed < 60
      ? t('{{n}} 秒前更新', { n: elapsed })
      : elapsed < 3600
        ? t('{{n}} 分钟前更新', { n: Math.floor(elapsed / 60) })
        : t('{{n}} 小时前更新', { n: Math.floor(elapsed / 3600) })
    : null;

  // 汇总 chips 兜底胶囊（异常/熔断为红、警告为黄、健康为绿），点击即筛选
  const chips: Array<{ key: 'all' | Cat; label: string; count: number; color: string }> = [
    { key: 'all', label: t('全部'), count: enabledRows.length, color: 'var(--text-main)' },
    { key: 'healthy', label: t(CAT_LABEL.healthy), count: counts.healthy, color: CAT_STYLE.healthy.dot },
    { key: 'warning', label: t(CAT_LABEL.warning), count: counts.warning, color: CAT_STYLE.warning.dot },
    { key: 'abnormal', label: t(CAT_LABEL.abnormal), count: counts.abnormal, color: CAT_STYLE.abnormal.dot },
    { key: 'circuit', label: t(CAT_LABEL.circuit), count: counts.circuit, color: CAT_STYLE.circuit.dot },
  ];
  // 表头可排序的列（默认严重度由 sort.key==='default' 提供）
  const sortCols: Array<{ key: SortKey; label: string }> = [
    { key: 'success', label: t('成功率') },
    { key: 'error', label: t('错误率') },
    { key: 'latency', label: t('平均延迟') },
    { key: 'requests', label: t('请求数') },
  ];
  const toggleSort = (key: SortKey): void => {
    setSort((prev) => (
      prev.key === key ? { key, dir: prev.dir === 'desc' ? 'asc' : 'desc' } : { key, dir: 'desc' }
    ));
  };
  const rowColor = (bad: number, warn: number, value: number): string =>
    value >= bad ? CAT_STYLE.abnormal.dot : value >= warn ? CAT_STYLE.warning.dot : 'var(--text-main)';

  return (
    <div>
      {/* 汇总 chips：健康/警告/异常/熔断计数，点击即筛选（再次点击回到全部） */}
      <div className="ch-chipbar">
        {chips.map((chip) => {
          const active = filter === chip.key;
          return (
            <button
              key={chip.key}
              type="button"
              aria-pressed={active}
              onClick={() => setFilter(active && chip.key !== 'all' ? 'all' : chip.key)}
              className={`ch-chip${active ? ' is-active' : ''}`}
              style={active ? { borderColor: 'transparent', background: chip.color, color: 'var(--bg-color)' } : undefined}
            >
              <span className="ch-chip-dot" style={{ background: chip.color }} />
              {chip.label} <b>{chip.count}</b>
            </button>
          );
        })}
        {disabledCount > 0 && (
          <span className="ch-hidden-hint">{t('已隐藏 {{n}} 个已停用供应商', { n: disabledCount })}</span>
        )}
      </div>

      {/* 工具条：搜索 + 数据新鲜度（排序改为点表头） */}
      <div className="ch-tools">
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={t('搜索')}
          aria-label={t('搜索')}
          className="ch-search"
        />
        {freshLabel && <span className="ch-fresh">{freshLabel}</span>}
      </div>

      {enabledRows.length === 0 ? (
        <div className="empty-state" style={{ padding: 22, textAlign: 'center' }}>
          <p style={{ margin: 0 }}>{t('暂无启用中的供应商')}</p>
        </div>
      ) : rows.length === 0 ? (
        <div className="empty-state" style={{ padding: 22, textAlign: 'center' }}>
          <p style={{ margin: 0 }}>{t('无匹配供应商')}</p>
        </div>
      ) : (
        <div className="ch-table-scroll">
          <table className="ch-table">
            <thead>
              <tr>
                <th>{t('供应商')}</th>
                <th>{t('状态')}</th>
                {sortCols.map((col) => {
                  const active = sort.key === col.key;
                  const arrow = active ? (sort.dir === 'desc' ? ' ↓' : ' ↑') : '';
                  return (
                    <th key={col.key} className="num">
                      <button type="button" className={`ch-th-sort${active ? ' is-active' : ''}`} onClick={() => toggleSort(col.key)}>
                        {col.label}{arrow}
                      </button>
                    </th>
                  );
                })}
                <th>{t('最近错误')}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((c, i) => {
                const cat = categoryOf(c);
                const st = CAT_STYLE[cat];
                const successRate = rateOf(c);
                const errorRate = errorRateOf(c);
                const lastError = c.health?.last_error ?? c.last_error;
                return (
                  <tr key={c.id ?? i}>
                    <td className="ch-name" title={c.name ?? undefined}>
                      {c.name || `Channel#${c.id ?? i}`}
                    </td>
                    <td>
                      <span className="ch-badge" style={{ background: st.badgeBg, color: st.badgeColor }}>
                        {t(CAT_LABEL[cat])}
                      </span>
                    </td>
                    <td className="num" style={{ color: successRate < 80 ? CAT_STYLE.abnormal.dot : successRate < 95 ? CAT_STYLE.warning.dot : 'var(--text-main)', fontWeight: 600 }}>
                      {successRate.toFixed(1)}%
                    </td>
                    <td className="num" style={{ color: rowColor(20, 5, errorRate), fontWeight: 600 }}>
                      {errorRate.toFixed(1)}%
                    </td>
                    <td className="num">{fmtInt(c.avg_latency_ms || 0, lang)}ms</td>
                    <td className="num">{fmtInt(c.total_requests ?? c.requests ?? 0, lang)}</td>
                    <td className="ch-err" title={lastError ?? undefined}>{lastError || '—'}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
