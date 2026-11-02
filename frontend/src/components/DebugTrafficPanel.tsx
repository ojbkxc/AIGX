import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import type { DebugTrafficEntry, DebugTrafficView } from '../types';

/**
 * 调试模式 · 上游原始报文（系统设置 → 运维 → 网关）
 *
 * 对照 Agent2API「设置 → 网关 → 调试模式」：开启后，AIGX 会在**即将发往上游**
 * 时抓一份 envelope —— 上游 URL、**脱敏后的**请求头、实际发出的请求体
 * （已按别名映射改写 model 的最终形态）。与 `log_body`（下游客户端侧）互补：
 * 前者回答「用户要什么」，这里回答「我们到底发出去了什么」。
 *
 * 缓冲在进程内（不写库），**重启即清空**；关闭开关时后端会顺手清空。
 */
export default function DebugTrafficPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [enabled, setEnabled] = useState(false);
  const [view, setView] = useState<DebugTrafficView | null>(null);
  const [openIdx, setOpenIdx] = useState<number | null>(null);

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
      setOpenIdx(null);
      await loadView();
      addToast(t('已清空 {{n}} 条', { n: res?.data?.cleared ?? 0 }));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  const entries: DebugTrafficEntry[] = view?.entries ?? [];

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('调试模式（上游原始报文）')}</h2>
        <p className="card-subtitle">
          {t('记录 AIGX 实际发往上游的 URL、脱敏后的请求头与请求体；存在内存环形缓冲中，重启即清空。')}
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
              {t('凭据类请求头（authorization / cookie / x-api-key 等）一律脱敏为 [redacted]，不可关闭。')}
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
              {t('保留最近 {{max}} 条 · 单条请求体上限 {{kb}} KB · 已挤出 {{dropped}} 条', {
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
                const key = `${entry.ts}-${idx}`;
                const open = openIdx === idx;
                return (
                  <div
                    key={key}
                    className="card"
                    style={{ padding: 10, border: '1px solid var(--border, #333)' }}
                  >
                    <div
                      style={{ cursor: 'pointer', display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center' }}
                      onClick={() => setOpenIdx(open ? null : idx)}
                    >
                      <span style={{ fontSize: 12, opacity: 0.65 }}>
                        {new Date(entry.ts).toLocaleString()}
                      </span>
                      <span className="badge">{entry.method}</span>
                      <span style={{ wordBreak: 'break-all' }}>{entry.url}</span>
                      {entry.truncated && <span className="badge">{t('已截断')}</span>}
                      {entry.model && <span style={{ opacity: 0.7 }}>{entry.model}</span>}
                      {entry.account_id && <span style={{ opacity: 0.7 }}>#{entry.account_id}</span>}
                    </div>
                    {open && (
                      <div style={{ marginTop: 8, fontSize: 12 }}>
                        <div style={{ opacity: 0.7, marginBottom: 4 }}>
                          {t('请求 ID')}: {entry.request_id || '-'}
                        </div>
                        <div style={{ opacity: 0.7, marginBottom: 4 }}>{t('请求头（已脱敏）')}</div>
                        <pre style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-all', margin: 0 }}>
                          {JSON.stringify(entry.headers ?? {}, null, 2)}
                        </pre>
                        <div style={{ opacity: 0.7, margin: '8px 0 4px' }}>{t('请求体')}</div>
                        <pre style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-all', margin: 0 }}>
                          {entry.body ?? t('（空）')}
                        </pre>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}