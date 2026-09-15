import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { isAdmin } from '../lib/utils';
import './StatusLine.css';

/** 后端 /api/dashboard/realtime + channel_health + tokens/today 的最小结构 */
interface RealtimeStats {
  qps?: number;
  rps?: number;
  avg_latency_ms?: number;
}

interface ChannelHealth {
  id?: string | number;
  name?: string;
  error_rate?: number | null;
  circuit_breaker?: string;
  total_requests?: number;
  requests?: number;
}

interface TodayTokens {
  total_tokens?: number;
  request_count?: number;
}

interface StatusData {
  realtime: RealtimeStats | null;
  channels: ChannelHealth[] | null;
  today: TodayTokens | null;
}

/**
 * StatusLine — 底部常驻状态行（cc-haha StatusLine 模式）。
 * 仅管理员可见：今日请求 / 错误率 / 熔断渠道数 / 实时 RPS / 平均延迟。
 * 30s 轮询 + 300ms 防抖合并渲染；指标越阈值变色（错误率>5% 或熔断渠道>0 变警告色）。
 */
export default function StatusLine(): JSX.Element | null {
  const { t } = useTranslation();
  const [data, setData] = useState<StatusData>({ realtime: null, channels: null, today: null });
  const [failed, setFailed] = useState(false);
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (!isAdmin()) return;

    let cancelled = false;

    const load = async (): Promise<void> => {
      // 300ms 防抖：路由切换等连续触发时只拉一次
      if (debounceRef.current) clearTimeout(debounceRef.current);
      debounceRef.current = setTimeout(async () => {
        try {
          const [rt, ch, td] = await Promise.all([
            api.getRealtime().catch(() => null),
            api.getChannelHealth().catch(() => null),
            api.getTodayTokens().catch(() => null),
          ]);
          if (cancelled) return;
          setFailed(false);
          setData({
            realtime: ((rt as { data?: RealtimeStats })?.data ?? rt) as RealtimeStats | null,
            channels: ((ch as { data?: ChannelHealth[] })?.data ?? ch) as ChannelHealth[] | null,
            today: ((td as { data?: TodayTokens })?.data ?? td) as TodayTokens | null,
          });
        } catch {
          if (!cancelled) setFailed(true);
        }
      }, 300);
    };

    void load();
    // 30s 轮询：与 Dashboard 实时指标刷新节奏一致
    timerRef.current = setInterval(load, 30000);

    return () => {
      cancelled = true;
      if (timerRef.current) clearInterval(timerRef.current);
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, []);

  if (!isAdmin()) return null;

  const channels = data.channels || [];
  const todayRequests = data.today?.request_count || 0;
  const rps = data.realtime?.rps ?? data.realtime?.qps ?? 0;
  const latency = data.realtime?.avg_latency_ms ?? 0;

  // 聚合：总请求数 / 加权错误率 / 熔断渠道数
  const totalReq = channels.reduce((s, c) => s + (c.total_requests ?? c.requests ?? 0), 0);
  const weightedError = channels.reduce((s, c) => s + (c.error_rate ?? 0) * (c.total_requests ?? c.requests ?? 0), 0);
  const errorRate = totalReq > 0 ? weightedError / totalReq : 0;
  const openBreakers = channels.filter((c) => c.circuit_breaker === 'open').length;

  const errorBad = errorRate > 0.05;
  const latencyBad = latency > 3000;
  const cls = (bad: boolean): string => (bad ? 'statusline-seg statusline-seg-bad' : 'statusline-seg');

  return (
    <div className="statusline" role="status" aria-label={t('网关状态')}>
      <span className="statusline-dot" aria-hidden="true" />
      <span className={cls(false)}>{t('今日请求')} {todayRequests.toLocaleString()}</span>
      <span className={cls(errorBad)}>
        {t('错误率')} {(errorRate * 100).toFixed(1)}%
      </span>
      <span className={cls(openBreakers > 0)}>
        {t('熔断渠道')} {openBreakers}
      </span>
      <span className={cls(false)}>RPS {rps.toFixed(1)}</span>
      <span className={cls(latencyBad)}>
        {t('平均延迟')} {latency ? `${Math.round(latency)}ms` : '—'}
      </span>
      {failed && <span className="statusline-seg statusline-seg-bad">{t('状态获取失败')}</span>}
    </div>
  );
}
