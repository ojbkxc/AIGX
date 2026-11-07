import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Activity, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, EmptyState, Loading } from '../components/ui';
import { isAdmin } from '../lib/utils';
import type {
  DiagnosticsBreaker,
  DiagnosticsBreakers,
  DiagnosticsChannelProbes,
  DiagnosticsSummary,
} from '../types';
import './Diagnostics.css';

/** 熔断器三态 → 徽章色调 */
const BREAKER_STATE_TONE: Record<string, 'success' | 'info' | 'warning' | 'danger'> = {
  closed: 'success',
  open: 'danger',
  halfopen: 'warning',
};

/** 探测结果 → 徽章色调（alive / timeout / dead） */
const PROBE_RESULT_TONE: Record<string, 'success' | 'info' | 'danger'> = {
  alive: 'success',
  timeout: 'info',
  dead: 'danger',
};

/** 探测结果 → 中文标签 */
function probeResultLabel(t: (s: string) => string, result: string): string {
  if (result === 'alive') return t('存活');
  if (result === 'timeout') return t('超时');
  return t('不可达');
}

/** 熔断器状态 → 中文标签 */
function breakerStateLabel(t: (s: string) => string, state: string): string {
  if (state === 'closed') return t('闭合');
  if (state === 'open') return t('熔断');
  if (state === 'halfopen') return t('半开');
  return state;
}

/** uptime 秒 → 人类可读 */
function fmtUptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  const parts: string[] = [];
  if (d > 0) parts.push(`${d}d`);
  if (h > 0 || d > 0) parts.push(`${h}h`);
  if (m > 0 || parts.length > 0) parts.push(`${m}m`);
  parts.push(`${s}s`);
  return parts.join(' ');
}

/**
 * Diagnostics — 渠道健康面板（管理员专属）。
 *
 * 后端 /api/diagnostics/* 三件套的承接：
 * - 系统体检摘要：渠道统计 / 今日用量 / 24h 错误 top5 / 运行信息
 * - 渠道批量测活：一键触发全部启用渠道的轻量探测（只探测不修改）
 * - 熔断器快照：逐渠道断路器三态与冷却剩余
 *
 * 2026-11-07 起并入「渠道与账号池」页第三 Tab「健康」：三项数据全部以渠道为
 * 粒度，放在渠道页旁边能直接「看到 → 去修」（07 报告 E.3）。独立顶级菜单
 * /diagnostics 已下线（路由重定向防书签失效）。`embedded` 时去掉页面级
 * 标题区（宿主页的 Tab 已表达层级），数据逻辑不变。
 */
export default function Diagnostics({ embedded = false }: { embedded?: boolean }): JSX.Element {
  const { t } = useTranslation();
  const admin = isAdmin();
  const addToast = useToast();

  const [summary, setSummary] = useState<DiagnosticsSummary | null>(null);
  const [summaryError, setSummaryError] = useState('');
  const [loadingSummary, setLoadingSummary] = useState(true);

  const [probes, setProbes] = useState<DiagnosticsChannelProbes | null>(null);
  const [probing, setProbing] = useState(false);

  const [breakers, setBreakers] = useState<DiagnosticsBreakers | null>(null);

  const loadSummary = useCallback(async () => {
    setLoadingSummary(true);
    setSummaryError('');
    try {
      const res = await api.getDiagnosticsSummary();
      if (res?.success && res?.data) {
        setSummary(res.data);
      } else {
        setSummaryError(res?.error?.message || t('体检摘要加载失败'));
      }
    } catch (err) {
      setSummaryError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoadingSummary(false);
    }
  }, [t]);

  useEffect(() => {
    if (!admin) return;
    void loadSummary();
    api.getDiagnosticsBreakers()
      .then((res) => {
        if (res?.success && res?.data) setBreakers(res.data);
      })
      .catch(() => { /* 熔断器快照失败静默：不影响体检摘要 */ });
  }, [admin, loadSummary]);

  const runProbes = async () => {
    setProbing(true);
    try {
      const res = await api.getDiagnosticsChannels();
      if (res?.success && res?.data) {
        setProbes(res.data);
        addToast(`${t('测活完成')}：${res.data.alive}/${res.data.probed} ${t('存活')}`);
      } else {
        addToast(res?.error?.message || t('测活失败'), 'error');
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : t('测活失败'), 'error');
    } finally {
      setProbing(false);
    }
  };

  if (!admin) return <EmptyState message={t('无权限')} />;

  const channels = summary?.channels;
  const usage = summary?.today_usage;
  const errors = summary?.errors_24h;
  const runtime = summary?.runtime;

  return (
    <div>
      {/* 独立路由已下线，仅 embedded=false（直接渲染兜底）时带标题区 */}
      {!embedded && (
        <div className="page-header">
          <div>
            <h1>{t('系统诊断')}</h1>
            <p>{t('渠道健康、用量概况与熔断器状态的一站式体检（与 Agent 运维共用同一套探测口径）')}</p>
          </div>
          <button
            className="btn btn-outline btn-sm"
            onClick={() => void loadSummary()}
            disabled={loadingSummary}
            style={{ gap: 6 }}
          >
            <RefreshCw size={13} /> {t('刷新')}
          </button>
        </div>
      )}
      {embedded && (
        <div className="dx-embedded-actions">
          <button
            className="btn btn-outline btn-sm"
            onClick={() => void loadSummary()}
            disabled={loadingSummary}
            style={{ gap: 6 }}
          >
            <RefreshCw size={13} /> {t('刷新')}
          </button>
        </div>
      )}

      {loadingSummary ? (
        <Loading text={t('加载诊断数据')} />
      ) : summaryError ? (
        <EmptyState
          message={summaryError}
          action={<Button size="sm" onClick={() => void loadSummary()}>{t('重试')}</Button>}
        />
      ) : (
        <>
          {/* 体检摘要 */}
          <div className="dx-summary-grid">
            <Card title={t('渠道统计')}>
              <div className="dx-stat-grid">
                <div className="dx-stat">
                  <div className="dx-stat-value">{channels?.total ?? 0}</div>
                  <div className="dx-stat-label">{t('总数')}</div>
                </div>
                <div className="dx-stat">
                  <div className="dx-stat-value">{channels?.enabled ?? 0}</div>
                  <div className="dx-stat-label">{t('启用')}</div>
                </div>
                <div className="dx-stat">
                  <div className="dx-stat-value">{channels?.circuit_open ?? 0}</div>
                  <div className="dx-stat-label">{t('熔断中')}</div>
                </div>
                <div className="dx-stat">
                  <div className="dx-stat-value">{channels?.circuit_halfopen ?? 0}</div>
                  <div className="dx-stat-label">{t('半开')}</div>
                </div>
                <div className="dx-stat">
                  <div className="dx-stat-value">{channels?.cooldown ?? 0}</div>
                  <div className="dx-stat-label">{t('冷却中')}</div>
                </div>
              </div>
            </Card>

            <Card title={t('今日用量')}>
              <div className="dx-stat-grid">
                <div className="dx-stat">
                  <div className="dx-stat-value">{usage?.requests ?? 0}</div>
                  <div className="dx-stat-label">{t('请求数')}</div>
                </div>
                <div className="dx-stat">
                  <div className="dx-stat-value">{usage ? `${usage.success_rate.toFixed(1)}%` : '—'}</div>
                  <div className="dx-stat-label">{t('成功率')}</div>
                </div>
                <div className="dx-stat">
                  <div className="dx-stat-value">
                    {((usage?.input_tokens ?? 0) + (usage?.output_tokens ?? 0)).toLocaleString()}
                  </div>
                  <div className="dx-stat-label">{t('Tokens')}</div>
                </div>
              </div>
            </Card>

            <Card title={t('运行信息')}>
              <div className="dx-stat-grid">
                <div className="dx-stat">
                  <div className="dx-stat-value">{runtime?.version ?? '—'}</div>
                  <div className="dx-stat-label">{t('版本')}</div>
                </div>
                <div className="dx-stat">
                  <div className="dx-stat-value">{runtime?.pid ?? '—'}</div>
                  <div className="dx-stat-label">{t('PID')}</div>
                </div>
                <div className="dx-stat">
                  <div className="dx-stat-value">
                    {runtime?.uptime_available && runtime.uptime_secs != null
                      ? fmtUptime(runtime.uptime_secs)
                      : '—'}
                  </div>
                  <div className="dx-stat-label">{t('运行时长')}</div>
                </div>
              </div>
            </Card>
          </div>

          {/* 24h 错误 top5 */}
          <Card title={t('近 24 小时错误 TOP5')}>
            {errors && errors.total_failures === 0 ? (
              <EmptyState icon="✅" message={t('近 24 小时无失败请求')} />
            ) : (
              <table className="dx-table">
                <thead>
                  <tr>
                    <th style={{ width: 110 }}>{t('HTTP 状态码')}</th>
                    <th style={{ width: 90 }}>{t('次数')}</th>
                    <th>{t('最近错误消息')}</th>
                  </tr>
                </thead>
                <tbody>
                  {errors?.top?.map((e) => (
                    <tr key={e.status_code}>
                      <td>
                        <Badge tone="danger">{e.status_code}</Badge>
                      </td>
                      <td className="dx-num">{e.count}</td>
                      <td className="dx-message">{e.error_msg || '—'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </Card>

          {/* 渠道批量测活 */}
          <Card
            title={t('渠道批量测活')}
            actions={
              <button
                className="btn btn-primary btn-sm"
                onClick={() => void runProbes()}
                disabled={probing}
                style={{ gap: 6 }}
              >
                <Activity size={13} />
                {probing ? t('测活中...') : t('开始测活')}
              </button>
            }
          >
            <p className="dx-note">
              {t('对全部启用渠道并发执行轻量探测。只探测不修改：不写断路器、不改动渠道状态、不落库。')}
            </p>
            {!probes ? (
              <EmptyState icon="🩺" message={t('点击开始测活查看各渠道存活状态')} />
            ) : (
              <>
                <div className="dx-probe-summary">
                  <span>
                    {t('探测 {{n}} 个渠道，存活 {{alive}} 个')}（
                    {t('截止 {{s}}s')}）
                  </span>
                </div>
                {probes.channels.length === 0 ? (
                  <EmptyState message={t('没有启用的渠道可探测')} />
                ) : (
                  <table className="dx-table">
                    <thead>
                      <tr>
                        <th>{t('渠道')}</th>
                        <th style={{ width: 100 }}>{t('状态')}</th>
                        <th style={{ width: 90 }}>{t('延迟')}</th>
                        <th>{t('消息')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {probes.channels.map((p) => (
                        <tr key={p.channel_id}>
                          <td>{p.name}</td>
                          <td>
                            <Badge tone={PROBE_RESULT_TONE[p.result] ?? 'neutral'}>
                              {probeResultLabel(t, p.result)}
                            </Badge>
                          </td>
                          <td className="dx-num">{p.latency_ms}ms</td>
                          <td className="dx-message">{p.message || '—'}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                )}
              </>
            )}
          </Card>

          {/* 熔断器快照 */}
          <Card title={t('熔断器快照')}>
            <p className="dx-note">
              {t('逐渠道断路器状态。从未失败的渠道没有条目（= 隐含闭合）。reset 请用渠道页的重置熔断按钮。')}
            </p>
            {breakers && breakers.tracked === 0 ? (
              <EmptyState icon="✅" message={t('全部渠道断路器均为闭合状态')} />
            ) : (
              breakers && (
                <>
                  <div className="dx-probe-summary">
                    <span>
                      {t('跟踪 {{n}} 个渠道：熔断 {{o}} · 半开 {{h}}')}
                    </span>
                  </div>
                  <table className="dx-table">
                    <thead>
                      <tr>
                        <th>{t('渠道')}</th>
                        <th style={{ width: 80 }}>{t('状态')}</th>
                        <th style={{ width: 90 }}>{t('失败次数')}</th>
                        <th>{t('失败类型')}</th>
                        <th style={{ width: 110 }}>{t('冷却剩余')}</th>
                        <th style={{ width: 110 }}>{t('限流剩余')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {breakers.breakers.map((b: DiagnosticsBreaker) => (
                        <tr key={b.channel_id}>
                          <td>{b.channel_id}</td>
                          <td>
                            <Badge tone={BREAKER_STATE_TONE[b.state] ?? 'neutral'}>
                              {breakerStateLabel(t, b.state)}
                            </Badge>
                          </td>
                          <td className="dx-num">{b.failure_count}</td>
                          <td className="dx-message">{b.failure_type || '—'}</td>
                          <td className="dx-num">
                            {b.cooldown_remaining_secs > 0 ? `${b.cooldown_remaining_secs}s` : '—'}
                          </td>
                          <td className="dx-num">
                            {b.rate_limit_remaining_secs > 0 ? `${b.rate_limit_remaining_secs}s` : '—'}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </>
              )
            )}
          </Card>
        </>
      )}
    </div>
  );
}
