import React, { useState, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';

// 进程级系统监控面板（批次6）：CPU 差分采样 / 内存 / 负载 / 进程 RSS。
// 后端约定：Linux 全量采集；非 Linux available_flag=false 时降级提示。
// CPU 使用率需要两次请求差分——面板 10s 轮询天然满足。
export default function SystemMonitorPanel() {
  const { t } = useTranslation();
  const [snap, setSnap] = useState(null);
  const [error, setError] = useState('');
  const timer = useRef(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const res = await api.getSystemMonitor();
        if (!cancelled) {
          setSnap(res?.data || res || null);
          setError('');
        }
      } catch (err) {
        if (!cancelled) setError(err.message);
      }
    };
    load();
    timer.current = setInterval(load, 10_000);
    return () => {
      cancelled = true;
      if (timer.current) clearInterval(timer.current);
    };
  }, []);

  if (error) return <div className="error-message">{error}</div>;
  if (!snap) return null;

  const { cpu, memory, load, process } = snap;

  const bar = (percent, color) => (
    <div style={{ height: 6, background: 'var(--bg-color)', borderRadius: 3, overflow: 'hidden', marginTop: 6 }}>
      <div style={{
        width: `${Math.min(100, Math.max(0, percent))}%`,
        height: '100%',
        background: color,
        borderRadius: 3,
        transition: 'width 0.6s ease',
      }} />
    </div>
  );

  const fmtBytes = (b) => {
    if (!b || b <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.min(units.length - 1, Math.floor(Math.log(b) / Math.log(1024)));
    return `${(b / 1024 ** i).toFixed(1)} ${units[i]}`;
  };

  const usageColor = (p) => (p > 85 ? 'rgb(239,68,68)' : p > 60 ? 'rgb(234,179,8)' : 'rgb(34,197,94)');

  return (
    <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(200px, 1fr))', gap: 12 }}>
      {/* CPU */}
      <div className="stat-card" style={{ padding: 14 }}>
        <div className="security-stat-label">{t('CPU 使用率')}</div>
        <div style={{ fontSize: 22, fontWeight: 700, color: usageColor(cpu?.usage_percent || 0) }}>
          {cpu?.sampled ? `${(cpu.usage_percent || 0).toFixed(1)}%` : '—'}
        </div>
        {cpu?.sampled && bar(cpu.usage_percent, usageColor(cpu.usage_percent))}
        <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 6 }}>
          {cpu?.core_count || '—'} {t('核心')}
        </div>
      </div>

      {/* 内存 */}
      <div className="stat-card" style={{ padding: 14 }}>
        <div className="security-stat-label">{t('内存使用率')}</div>
        <div style={{ fontSize: 22, fontWeight: 700, color: usageColor(memory?.usage_percent || 0) }}>
          {memory?.available_flag ? `${(memory.usage_percent || 0).toFixed(1)}%` : '—'}
        </div>
        {memory?.available_flag && bar(memory.usage_percent, usageColor(memory.usage_percent))}
        <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 6 }}>
          {memory?.available_flag
            ? `${fmtBytes(memory.used)} / ${fmtBytes(memory.total)}`
            : t('当前平台不可用')}
        </div>
      </div>

      {/* 负载 */}
      <div className="stat-card" style={{ padding: 14 }}>
        <div className="security-stat-label">{t('系统负载')}</div>
        <div style={{ fontSize: 22, fontWeight: 700 }}>
          {load?.available_flag ? (load.load_1m || 0).toFixed(2) : '—'}
        </div>
        <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 6 }}>
          {load?.available_flag
            ? `1m ${(load.load_1m || 0).toFixed(2)} · 5m ${(load.load_5m || 0).toFixed(2)} · 15m ${(load.load_15m || 0).toFixed(2)}`
            : t('当前平台不可用')}
        </div>
      </div>

      {/* 进程 */}
      <div className="stat-card" style={{ padding: 14 }}>
        <div className="security-stat-label">{t('网关进程')}</div>
        <div style={{ fontSize: 22, fontWeight: 700 }}>
          {fmtBytes(process?.rss_bytes || 0)}
        </div>
        <div style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 6 }}>
          PID {process?.pid ?? '—'} · {t('运行')} {Math.floor((process?.uptime_secs || 0) / 3600)}h
          {' '}{Math.floor(((process?.uptime_secs || 0) % 3600) / 60)}m
        </div>
      </div>
    </div>
  );
}
