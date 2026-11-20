import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import type {
  DebugTrafficDetail,
  DebugTrafficEntry,
  DebugTrafficView,
} from '../types';

/**
 * 调试模式 · 上游原始报文（系统设置 → 网关 → 调试模式）
 *
 * 对照 Agent2API v3.5.0-beta.3：开启后，AIGX 会在**即将发往上游**时抓一份
 * envelope —— 上游 URL、**脱敏后的**请求头/请求体（已按别名映射改写 model
 * 的最终形态），以及错误路径的上游响应状态/头/体。与 `log_body`（下游客户端
 * 侧）互补：前者回答「用户要什么」，这里回答「我们到底发出去了什么」。
 *
 * 列表只给摘要；点条目打开**详情弹层**，按请求日志 id 调
 * `GET /api/debug/traffic?id=` 取单条（数百 KB 的 SSE 体不塞进列表响应）。
 * 缓冲在进程内（FileStore 持久化最近条目），关闭开关时后端会顺手清空。
 */
export default function DebugTrafficPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [enabled, setEnabled] = useState(false);
  const [view, setView] = useState<DebugTrafficView | null>(null);
  const [detail, setDetail] = useState<DebugTrafficDetail | null>(null);
  const [detailError, setDetailError] = useState('');
  const [detailLoading, setDetailLoading] = useState(false);
  const [openId, setOpenId] = useState<string | null>(null);

  const loadView = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getDebugTraffic();
      setView(res?.data ?? null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void (async () => {
      try {
        const [limits, traffic] = await Promise.all([api.getLimits(), api.getDebugTraffic()]);
        const d = limits?.data as Record<string, unknown> | undefined;
        setEnabled(Boolean(d?.debug_traffic));
        setView(traffic?.data ?? null);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  // 弹层 Esc 关闭（直接内联状态清理，避免把每次渲染新建的 closeDetail 放进依赖）
  useEffect(() => {
    if (!openId) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') {
        setOpenId(null);
        setDetail(null);
        setDetailError('');
        setDetailLoading(false);
      }
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [openId]);

  const toggle = async (next: boolean): Promise<void> => {
    setError('');
    try {
      await api.updateLimits({ debug_traffic: next });
      setEnabled(next);
      // 关闭时后端已清空；开启时也拉一次（避免展示开关开启前的旧数据）
      await loadView();
      addToast(next ? t('调试模式已开启，抓取最近的上游请求') : t('调试模式已关闭，缓冲已清空'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const clear = async (): Promise<void> => {
    setError('');
    try {
      const res = await api.clearDebugTraffic();
      closeDetail();
      await loadView();
      addToast(t('已清空 {{n}} 条', { n: res?.data?.cleared ?? 0 }));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const openDetail = async (entry: DebugTrafficEntry): Promise<void> => {
    const id = entry.id || entry.request_id;
    if (!id) {
      addToast(t('该记录缺少请求 ID，无法打开详情'), 'error');
      return;
    }
    setOpenId(id);
    setDetail(null);
    setDetailError('');
    setDetailLoading(true);
    try {
      const res = await api.getDebugTrafficDetail(id);
      setDetail(res?.data ?? null);
    } catch (err) {
      // 后端 404 = 调试模式当时未开启/记录已超出保留条数
      setDetailError(err instanceof Error ? err.message : String(err));
    } finally {
      setDetailLoading(false);
    }
  };

  function closeDetail(): void {
    setOpenId(null);
    setDetail(null);
    setDetailError('');
    setDetailLoading(false);
  }

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  // Array.isArray 守卫：桩兜底 {data:[]} 时 view 是数组，view.entries 是
  // Array.prototype.entries（native 方法）而非列表——?? [] 不拦函数。
  const entries: DebugTrafficEntry[] = Array.isArray(view?.entries) ? view!.entries : [];

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('调试模式（上游原始报文）')}</h2>
        <p className="card-subtitle">
          {t('记录 AIGX 实际发往上游的 URL、脱敏后的请求头与请求体；凭据类字段一律脱敏，不可关闭。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={enabled}
                onChange={(e) => void toggle(e.target.checked)}
              />
              <span>{t('开启调试模式（抓取上游原始报文）')}</span>
            </label>
            <div className="form-hint">
              {t('凭据类请求头（authorization / cookie / x-api-key 等）与 URL/正文中的凭据字段一律脱敏为 [redacted]，不可关闭。')}
            </div>
          </div>

          <div className="settings-actions" style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <button className="btn" onClick={() => void loadView()}>
              {t('刷新')}
            </button>
            <button className="btn" onClick={() => void clear()} disabled={entries.length === 0}>
              {t('清空')}
            </button>
            <span style={{ marginLeft: 'auto', opacity: 0.7, fontSize: 12 }}>
              {t('保留最近 {{max}} 条 · 单条正文上限 {{kb}} KB · 已挤出 {{dropped}} 条', {
                max: view?.maxEntries ?? 0,
                kb: Math.round((view?.maxBodyBytes ?? 0) / 1024),
                dropped: view?.dropped ?? 0,
              })}
            </span>
          </div>

          {!enabled && (
            <div className="form-hint">
              {t('调试模式未开启：不会记录任何上游报文。')}
            </div>
          )}

          {enabled && entries.length === 0 && (
            <div className="form-hint">{t('暂无记录 —— 发起一次请求后点「刷新」。')}</div>
          )}

          {entries.length > 0 && (
            <div className="debug-traffic-list" style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
              {entries.map((entry, idx) => {
                const key = `${entry.ts}-${entry.id ?? idx}`;
                return (
                  <div
                    key={key}
                    className="card"
                    style={{ padding: 10, border: '1px solid var(--border, #333)', cursor: 'pointer' }}
                    onClick={() => void openDetail(entry)}
                    title={t('点击查看详情')}
                  >
                    <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center' }}>
                      <span style={{ fontSize: 12, opacity: 0.65 }}>
                        {new Date(entry.ts).toLocaleString()}
                      </span>
                      <span className="badge">{entry.method}</span>
                      <span style={{ wordBreak: 'break-all' }}>{entry.url}</span>
                      {entry.response_status != null && (
                        <span className="badge">{entry.response_status}</span>
                      )}
                      {entry.truncated && <span className="badge">{t('已截断')}</span>}
                      {entry.model && <span style={{ opacity: 0.7 }}>{entry.model}</span>}
                      {entry.account_id && <span style={{ opacity: 0.7 }}>#{entry.account_id}</span>}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>

      {/* 详情弹层：按请求日志 id 单条拉取（列表不夹带原始报文） */}
      {openId && (
        <div
          className="modal-overlay"
          onClick={(e) => {
            if (e.target === e.currentTarget) closeDetail();
          }}
        >
          <div className="modal log-detail-modal" role="dialog" aria-label={t('原始报文详情')}>
            <div className="modal-header">
              <h3>{t('原始报文详情')}</h3>
              <button type="button" className="modal-close" onClick={closeDetail} aria-label="×">
                ×
              </button>
            </div>
            <div className="modal-body">
              {detailLoading && <div className="loading">{t('加载中...')}</div>}
              {detailError && <div className="error-message">{detailError}</div>}
              {detail && (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 8, fontSize: 12 }}>
                  <div className="log-detail-grid">
                    <div className="log-detail-row">
                      <span className="log-detail-label">{t('请求 ID')}</span>
                      <span className="log-detail-value log-detail-mono">{detail.id || '—'}</span>
                    </div>
                    <div className="log-detail-row">
                      <span className="log-detail-label">{t('时间')}</span>
                      <span className="log-detail-value">
                        {detail.ts ? new Date(detail.ts).toLocaleString() : '—'}
                      </span>
                    </div>
                    <div className="log-detail-row">
                      <span className="log-detail-label">{t('方法')}</span>
                      <span className="log-detail-value">{detail.method || '—'}</span>
                    </div>
                    <div className="log-detail-row">
                      <span className="log-detail-label">{t('状态码')}</span>
                      <span className="log-detail-value">{detail.status ?? '—'}</span>
                    </div>
                    <div className="log-detail-row">
                      <span className="log-detail-label">{t('模型')}</span>
                      <span className="log-detail-value log-detail-mono">{detail.model || '—'}</span>
                    </div>
                    {detail.accountId && (
                      <div className="log-detail-row">
                        <span className="log-detail-label">{t('账号')}</span>
                        <span className="log-detail-value log-detail-mono">#{detail.accountId}</span>
                      </div>
                    )}
                  </div>
                  <div>
                    <div className="log-detail-label">{t('上游 URL（已脱敏）')}</div>
                    <pre className="log-detail-pre">{detail.url || '—'}</pre>
                  </div>
                  <div>
                    <div className="log-detail-label">{t('请求头（已脱敏）')}</div>
                    <pre className="log-detail-pre">
                      {JSON.stringify(detail.requestHeaders ?? {}, null, 2)}
                    </pre>
                  </div>
                  <div>
                    <div className="log-detail-label">
                      {t('请求体')}
                      {detail.truncated ? ` ${t('（已截断）')}` : ''}
                    </div>
                    <pre className="log-detail-pre">{detail.requestBody ?? t('（空）')}</pre>
                  </div>
                  {detail.responseHeaders && (
                    <div>
                      <div className="log-detail-label">{t('响应头（已脱敏）')}</div>
                      <pre className="log-detail-pre">
                        {JSON.stringify(detail.responseHeaders, null, 2)}
                      </pre>
                    </div>
                  )}
                  {detail.responseBody != null && (
                    <div>
                      <div className="log-detail-label">{t('响应体（已脱敏）')}</div>
                      <pre className="log-detail-pre">{detail.responseBody}</pre>
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
