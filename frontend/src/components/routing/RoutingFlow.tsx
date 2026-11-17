/**
 * RoutingFlow —— 请求流向面板（对照 A2A `routing-flow-types` 的 FlowSnapshot /
 * FlowPath 与 routing-request-list 的模型+时间窗筛选）。
 *
 * 形状：时间窗（5/10/30/60 分钟）→ 汇总指标 → 路径表
 * （客户端 → 模型 → 提供商/账号，含请求数、运行中、成功/失败、粘滞命中、换号数）。
 *
 * 数据源：GET /api/routing/flow?minutes=&scope=（be-routing）
 * 三态降级：`available === false`（记录库不可用）/ 404（端点未就绪）/ 请求失败。
 * 时间窗切换 = 改 query 重新拉取（不轮询；手动刷新）。
 */
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2, RefreshCw } from 'lucide-react';
import { api } from '../../api';
import { Button, Card, EmptyState } from '../ui';
import {
  FLOW_WINDOWS,
  type RoutingFlowPath,
  type RoutingFlowScope,
  type RoutingFlowSnapshot,
} from '../../types/routing-workspace';

function statusOf(err: unknown): number | undefined {
  return (err as { status?: number } | null)?.status;
}

export default function RoutingFlow({ active }: { active: boolean }): JSX.Element {
  const { t } = useTranslation();
  const [minutes, setMinutes] = useState<number>(10);
  const [scope, setScope] = useState<RoutingFlowScope>('window');
  const [data, setData] = useState<RoutingFlowSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [notReady, setNotReady] = useState(false);

  const load = useCallback(async (minutes: number, scope: RoutingFlowScope): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getRoutingFlow({ minutes, scope });
      setData(res?.data ?? null);
      setNotReady(false);
    } catch (err) {
      const st = statusOf(err);
      if (st === 404 || st === 405) {
        setNotReady(true);
        setData(null);
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setLoading(false);
    }
  }, []);

  // 首次显示 / 时间窗切换 / 范围切换 → 重新拉取（listing 的 query key 同口径）
  useEffect(() => {
    if (!active) return;
    void load(minutes, scope);
  }, [active, minutes, scope, load]);

  if (notReady) {
    return (
      <Card title={t('请求流向')}>
        <div className="rt-notice rt-notice-warn" role="status">
          {t('请求流向端点尚未就绪（后端建设中）。就绪后这里按 5/10/30/60 分钟时间窗展示请求经过的客户端 → 模型 → 供应商/账号路径。')}
        </div>
      </Card>
    );
  }

  const paths: RoutingFlowPath[] = Array.isArray(data?.paths) ? data?.paths ?? [] : [];
  const unavailable = data?.available === false;

  return (
    <Card
      title={t('请求流向')}
      actions={
        <Button variant="outline" onClick={() => void load(minutes, scope)} disabled={loading}>
          {loading ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />}
          {t('刷新')}
        </Button>
      }
    >
      <div className="rt-notice">
        {t('汇总 {{n}} 分钟内已完成的请求路径；「仅在途」只统计尚未结束的请求。配对口径与日志一致。', {
          n: minutes,
        })}
      </div>

      <div className="rt-flow-toolbar" role="group" aria-label={t('时间窗')}>
        <span className="rt-muted">{t('时间窗')}</span>
        {FLOW_WINDOWS.map((w) => (
          <button
            key={w}
            type="button"
            className={`rt-seg-btn rt-flow-win${minutes === w ? ' is-on' : ''}`}
            aria-pressed={minutes === w}
            data-window={w}
            onClick={() => setMinutes(w)}
          >
            {t('{{n}} 分钟', { n: w })}
          </button>
        ))}
        <label className="rt-special-check" style={{ marginLeft: 8 }}>
          <input
            type="checkbox"
            checked={scope === 'running'}
            onChange={(e) => setScope(e.target.checked ? 'running' : 'window')}
          />
          <span>{t('仅在途')}</span>
        </label>
      </div>

      {error ? <div className="error-message">{error}</div> : null}

      {unavailable ? (
        <EmptyState message={t('请求记录数据库不可用，无法统计流向。')} />
      ) : loading && !data ? (
        <div className="rt-special-loading">
          <Loader2 size={14} className="spin" />
          {t('统计中…')}
        </div>
      ) : (
        <>
          <div className="rt-flow-stats">
            <div>
              <b>{data?.requests ?? '—'}</b>
              <span>{t('请求数')}</span>
            </div>
            <div>
              <b>{data?.running ?? '—'}</b>
              <span>{t('在途')}</span>
            </div>
            <div>
              <b>{data?.pathCount ?? paths.length}</b>
              <span>{t('路径数')}</span>
            </div>
            <div>
              <b>{data?.sessions ?? '—'}</b>
              <span>{t('会话数')}</span>
            </div>
            {data?.unknownRequests ? (
              <div>
                <b>{data.unknownRequests}</b>
                <span>{t('未归类请求')}</span>
              </div>
            ) : null}
          </div>

          {paths.length ? (
            <div className="rt-table-wrap" style={{ marginTop: 12 }}>
              <table className="rt-table">
                <thead>
                  <tr>
                    <th>{t('客户端')}</th>
                    <th>{t('模型')}</th>
                    <th>{t('供应商 / 账号')}</th>
                    <th>{t('请求')}</th>
                    <th>{t('在途')}</th>
                    <th>{t('成功 / 失败')}</th>
                    <th>{t('粘滞命中')}</th>
                    <th>{t('换号')}</th>
                  </tr>
                </thead>
                <tbody>
                  {paths.map((p, i) => (
                    <tr key={p.id ?? `${p.model ?? ''}-${p.accountId ?? ''}-${i}`}>
                      <td>{p.clientLabel || p.client || '—'}</td>
                      <td>{p.model || '—'}</td>
                      <td>
                        {`${p.providerLabel || p.provider || '—'}`}
                        {p.accountName || p.accountId ? (
                          <span className="rt-muted">{` · ${p.accountName || p.accountId}`}</span>
                        ) : null}
                      </td>
                      <td className="rt-num">{p.requests ?? 0}</td>
                      <td className="rt-num">{p.running ?? 0}</td>
                      <td className="rt-num">
                        {`${p.successful ?? 0} / ${p.failed ?? 0}`}
                      </td>
                      <td className="rt-num">{p.stickyHits ?? 0}</td>
                      <td className="rt-num">{p.switched ?? 0}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <EmptyState
              message={
                loading
                  ? t('统计中…')
                  : t('最近 {{n}} 分钟没有匹配的请求', { n: minutes })
              }
            />
          )}

          {data?.truncated ? (
            <div className="form-hint" style={{ marginTop: 8 }}>
              {t('路径超出上限已截断，仅显示前 {{n}} 条。', { n: paths.length })}
            </div>
          ) : null}
          {data?.since ? (
            <div className="form-hint" style={{ marginTop: 4 }}>
              {t('统计起点：{{time}}', { time: new Date(data.since).toLocaleString() })}
            </div>
          ) : null}
        </>
      )}
    </Card>
  );
}