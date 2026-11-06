// 管理员视图顶部的实时四卡 + 全站累计三卡。
// 实时四卡 30s 轮询（页面隐藏自动暂停）；累计三卡是「今日 / 总量 / 本月限额」
// ——限额是 token 口径，与钱包的货币口径不同，卡片里写明避免误读。
// 错误态：每个面板失败时该卡片（实时组失败则整组）显示错误消息 + 重试按钮
// （复用 PanelWrap 的错误重试交互），不再静默显示 0 值——fmtInt/fmtTokens 对
// undefined 会回退 0 /「—」，错误时看起来像「真实零」会误导运维。
import { useTranslation } from 'react-i18next';
import { fmtInt, fmtTokens } from '../../lib/format';
import type { RealtimeData, TodayTokens, UsageSummary } from '../../hooks/useDashboard';

interface Props {
  realtime?: RealtimeData | null;
  realtimePending: boolean;
  /** 实时四卡查询错误态：整组替换为错误消息 + 重试 */
  realtimeError?: boolean;
  realtimeRefetch?: () => Promise<unknown>;
  activeUsersFallback?: number;
  activeChannelsFallback?: number;
  today?: TodayTokens | null;
  /** 今日用量查询错误态 */
  todayError?: boolean;
  todayRefetch?: () => Promise<unknown>;
  usage?: UsageSummary | null;
  /** Token 统计查询错误态 */
  usageError?: boolean;
  usageRefetch?: () => Promise<unknown>;
  monthlyUsed: number;
  monthlyLimit: number | null;
  /** 限额查询错误态 */
  limitsError?: boolean;
  limitsRefetch?: () => Promise<unknown>;
  lang?: string;
}

export default function LiveCards({
  realtime: rt, realtimePending, realtimeError, realtimeRefetch,
  activeUsersFallback, activeChannelsFallback,
  today: ts, todayError, todayRefetch,
  usage: us, usageError, usageRefetch,
  monthlyUsed, monthlyLimit, limitsError, limitsRefetch, lang,
}: Props): JSX.Element {
  const { t } = useTranslation();

  /** 卡片内错误块：消息 + 重试按钮（与 PanelWrap 错误态同款交互） */
  const errorBlock = (message: string, refetch?: () => Promise<unknown>): JSX.Element => (
    <div className="error-message" style={{ marginBottom: 0 }}>
      {message}
      {refetch && (
        <button className="btn btn-outline btn-sm" style={{ marginLeft: 8 }} onClick={() => void refetch()}>
          {t('重试')}
        </button>
      )}
    </div>
  );

  return (
    <>
      {/* 实时 4 卡（30s 轮询，页面隐藏自动暂停） */}
      {realtimeError ? (
        <div style={{ marginBottom: 16 }}>
          {errorBlock(t('实时数据加载失败'), realtimeRefetch)}
        </div>
      ) : !realtimePending && (
        <div className="stats-grid" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(180px, 1fr))', marginBottom: 16 }}>
          <div className="stat-card">
            <div className="stat-card-top">
              <div className="stat-title">{t('实时 RPS')}</div>
              <div className="stat-icon-badge" style={{ background: 'rgba(34, 197, 94, 0.15)', color: '#34d399' }}>⚡</div>
            </div>
            <div className="stat-value" style={{ fontSize: '24px' }}>{fmtInt(rt?.qps ?? rt?.rps ?? 0, lang)}</div>
            <div className="stat-desc" style={{ fontSize: '11px' }}>{t('每秒请求数')}</div>
          </div>
          <div className="stat-card">
            <div className="stat-card-top">
              <div className="stat-title">{t('活跃用户')}</div>
              <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>👥</div>
            </div>
            <div className="stat-value" style={{ fontSize: '24px' }}>{fmtInt(rt?.active_users ?? activeUsersFallback, lang)}</div>
            <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟')}</div>
          </div>
          <div className="stat-card">
            <div className="stat-card-top">
              <div className="stat-title">{t('活跃渠道')}</div>
              <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>🔗</div>
            </div>
            <div className="stat-value" style={{ fontSize: '24px' }}>{fmtInt(rt?.active_channels ?? activeChannelsFallback, lang)}</div>
            <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟')}</div>
          </div>
          <div className="stat-card">
            <div className="stat-card-top">
              <div className="stat-title">{t('平均延迟')}</div>
              <div className="stat-icon-badge" style={{ background: 'rgba(251, 146, 60, 0.15)', color: '#fb923c' }}>⏱️</div>
            </div>
            <div className="stat-value" style={{ fontSize: '24px' }}>{fmtInt(rt?.avg_latency_ms ?? 0, lang)}ms</div>
            <div className="stat-desc" style={{ fontSize: '11px' }}>{t('近 5 分钟平均')}</div>
          </div>
        </div>
      )}

      {/* 全站累计三卡（今日 / 总量 / 限额） */}
      <div className="stats-grid" style={{ marginBottom: 16 }}>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('今日用量')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>📊</div>
          </div>
          {todayError ? (
            errorBlock(t('今日用量加载失败'), todayRefetch)
          ) : (
            <>
              <div className="stat-value" style={{ fontSize: '26px' }}>{fmtTokens(ts?.total_tokens, lang)}</div>
              <div className="stat-desc" style={{ display: 'flex', gap: '16px', fontSize: '11px' }}>
                <span>↑ {t('输入')} {fmtTokens(ts?.input_tokens, lang)}</span>
                <span>↓ {t('输出')} {fmtTokens(ts?.output_tokens, lang)}</span>
              </div>
              <div className="stat-sub">
                <span>{fmtInt(ts?.request_count, lang)} {t('次请求')}</span>
              </div>
            </>
          )}
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('Token 统计')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(47, 111, 237, 0.15)', color: '#7ca4f5' }}>🔤</div>
          </div>
          {usageError ? (
            errorBlock(t('Token 统计加载失败'), usageRefetch)
          ) : (
            <>
              <div className="stat-value" style={{ fontSize: '26px' }}>{fmtTokens(us?.total_tokens, lang)}</div>
              <div className="stat-desc" style={{ fontSize: '11px' }}>
                <div>{t('输入')}: {fmtTokens(us?.total_input_tokens ?? us?.total_tokens, lang)}</div>
                <div>{t('输出')}: {fmtTokens(us?.total_output_tokens, lang)}</div>
              </div>
            </>
          )}
        </div>
        <div className="stat-card">
          <div className="stat-card-top">
            <div className="stat-title">{t('本月用量限额')}</div>
            <div className="stat-icon-badge" style={{ background: 'rgba(16, 185, 129, 0.15)', color: '#34d399' }}>📅</div>
          </div>
          {limitsError ? (
            errorBlock(t('本月用量限额加载失败'), limitsRefetch)
          ) : (
            <>
              <div className="stat-value" style={{ fontSize: '26px' }}>{monthlyLimit != null ? fmtInt(monthlyLimit, lang) : '∞'}</div>
              <div className="stat-desc" style={{ fontSize: '11px' }} title={t('统计口径：token（本月累计），与「配额/余额」的货币口径不同')}>
                <span>{t('已用')} {fmtInt(monthlyUsed, lang)} tokens</span>
                {monthlyLimit != null && monthlyLimit > 0 && (
                  <span style={{ marginLeft: 12 }}>{((monthlyUsed / monthlyLimit) * 100).toFixed(1)}%</span>
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </>
  );
}
