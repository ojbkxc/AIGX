/**
 * RouteReplay — 路由回放面板（对照 Agent2API routing-replay / routing-exec-timeline）。
 *
 * 只读渲染请求**当时**保存的选路事实，修改当前路由不会改变这份记录：
 * - 快照：候选有序表（含选中高亮与逐条原因）、被跳过渠道（含状态与原因）、
 *   策略/粘性/配置快照卡
 * - 执行时间线：逐次上游尝试的渠道、阶段、耗时与错误（对得上日志 attempts 计数）
 *
 * 数据源：GET /api/routing/replay/{request_id}（be-fix 任务 #12-B）。
 * 三态降级：端点 404（后端未就绪）/ 200 无快照（旧记录或快照未启用）/ 请求失败。
 * 性能约束：挂载即单次拉取、不轮询、无首屏开销（日志详情 Tab 激活才挂载）。
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { EmptyState, Loading } from './ui';
import type {
  RouteReplayAttemptItem,
  RouteReplayCandidateItem,
  RouteReplaySnapshot,
} from '../types/routing-replay';
import './RouteReplay.css';

/** 秒/毫秒自适应（对齐账号页 expiryMillis 口径：<1e11 视为秒） */
function toMillis(v: number | undefined): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  return v < 1e11 ? v * 1000 : v;
}

/** 耗时收短：ms → 「820ms / 1.2s / 1m04s」 */
function fmtDuration(ms: number | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)}s`;
  const m = Math.floor(ms / 60_000);
  return `${m}m${String(Math.round((ms % 60_000) / 1000)).padStart(2, '0')}s`;
}

function strategyLabel(s: string | undefined, t: (k: string) => string): string {
  switch (s) {
    case 'weighted': return t('加权随机');
    case 'round_robin': return t('组内轮询');
    case 'ranked': return t('多因子调度');
    default: return s ? s : t('未记录');
  }
}

function stickyLabel(s: string | undefined, t: (k: string) => string): string {
  switch (s) {
    case 'auto': return t('自动（5min）');
    case 'session': return t('长会话（6h）');
    case 'turn': return t('每轮');
    case 'off': return t('关闭');
    default: return s ? s : t('未记录');
  }
}

function candidateName(c: RouteReplayCandidateItem): string {
  return c.name || (c.id != null ? String(c.id) : '—');
}

/** 时间线里的单次尝试：渠道名 + 阶段/状态徽标 + 耗时 + 错误/备注 */
function AttemptStep({ a, t }: { a: RouteReplayAttemptItem; t: (k: string) => string }): JSX.Element {
  const name = a.channel_name || (a.channel_id != null ? String(a.channel_id) : '') || a.provider || t('未知渠道');
  const failed = Boolean(a.error);
  const phase = a.phase ? a.phase : '';
  const barPct = a.duration_ms != null && a.duration_ms >= 0
    ? Math.max(4, Math.min(100, Math.round((a.duration_ms / 30_000) * 100)))
    : null;
  return (
    <li className={`rr-step ${failed ? 'rr-step-failed' : ''}`}>
      <span className="rr-step-mark" aria-hidden="true" />
      <div className="rr-step-body">
        <div className="rr-step-head">
          <b>{name}</b>
          {a.status != null && (
            <span className={`rr-badge ${failed ? 'rr-badge-bad' : 'rr-badge-ok'}`}>{a.status}</span>
          )}
          {phase && <span className="rr-badge rr-badge-mute">{phase}</span>}
          {a.duration_ms != null && <span className="rr-step-dur">{fmtDuration(a.duration_ms)}</span>}
        </div>
        {barPct != null && (
          <div className="rr-step-bar"><i style={{ width: `${barPct}%` }} /></div>
        )}
        {a.error && <p className="rr-step-err">{a.error}</p>}
        {!a.error && a.notice && <p className="rr-step-note">{a.notice}</p>}
      </div>
    </li>
  );
}

/** 单请求回放视图：懒挂载 + 单次拉取（调用方控制挂载时机即可实现按需加载） */
export function RouteReplayView({ requestId }: { requestId: string | number }): JSX.Element {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(true);
  const [errStatus, setErrStatus] = useState<number | null>(null);
  const [errMsg, setErrMsg] = useState('');
  const [snap, setSnap] = useState<RouteReplaySnapshot | null>(null);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setErrStatus(null);
    setErrMsg('');
    setSnap(null);
    void (async () => {
      try {
        const res = await api.getRouteReplay(requestId);
        if (!alive) return;
        setSnap(res?.data ?? null);
      } catch (err) {
        if (!alive) return;
        const e = err as Error & { status?: number };
        setErrStatus(typeof e.status === 'number' ? e.status : null);
        setErrMsg(e.message || String(err));
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => { alive = false; };
  }, [requestId]);

  if (loading) return <div className="rr-loading"><Loading text={t('加载路由回放')} /></div>;

  // 端点 404：后端快照能力尚未就绪（be-fix #12-B），空态降级不报错
  if (errStatus === 404) {
    return (
      <EmptyState
        message={t('路由回放快照端点尚未就绪（后端建设中）。就绪后这里会显示该请求当时的候选顺序、跳过原因与执行时间线。')}
      />
    );
  }
  if (errMsg) {
    return (
      <div className="rr-degraded">
        <p>{t('路由回放加载失败：{{msg}}', { msg: errMsg })}</p>
      </div>
    );
  }
  if (!snap || (!snap.route && !snap.attempts?.length)) {
    return (
      <div className="rr-degraded">
        <p>{t('该请求没有保存选路快照（旧记录或快照未启用），下方为日志基础字段。')}</p>
      </div>
    );
  }

  const route = snap.route ?? {};
  const ordered = Array.isArray(route.ordered) ? route.ordered : [];
  const blocked = Array.isArray(route.blocked) ? route.blocked : [];
  const attempts = Array.isArray(snap.attempts) ? snap.attempts : [];
  const selected = snap.selected;
  const selectedName = selected != null
    ? [...ordered, ...blocked].find((c) => String(c.id) === String(selected))?.name || String(selected)
    : '';
  const cs = route.config_snapshot;
  const captured = toMillis(cs?.captured_at);

  return (
    <div className="rr-root">
      <div className="rr-chips">
        <span className="rr-badge rr-badge-brand">{strategyLabel(route.strategy, t)}</span>
        <span className="rr-badge rr-badge-mute">{t('会话粘性：{{mode}}', { mode: stickyLabel(route.sticky, t) })}</span>
        {route.sticky_hit && <span className="rr-badge rr-badge-ok">{t('命中会话粘滞')}</span>}
        {selectedName && (
          <span className="rr-badge rr-badge-brand">{t('最终承载：{{name}}', { name: selectedName })}</span>
        )}
      </div>
      <p className="rr-note">{t('这是请求当时保存的选路快照，修改当前路由不会改变这份记录。')}</p>

      <div className="rr-metrics">
        <div><b>{fmtDuration(snap.duration_ms)}</b><span>{t('总耗时')}</span></div>
        <div><b>{fmtDuration(snap.first_response_ms)}</b><span>{t('首帧')}</span></div>
        <div><b>{attempts.length || '—'}</b><span>{t('尝试次数')}</span></div>
        <div><b>{blocked.length || '—'}</b><span>{t('跳过候选')}</span></div>
      </div>

      <section className="rr-section">
        <h4>{t('执行路径')}</h4>
        <ol className="rr-timeline">
          <li className="rr-step rr-step-entry">
            <span className="rr-step-mark" aria-hidden="true" />
            <div className="rr-step-body">
              <div className="rr-step-head"><b>{t('请求进入')}</b></div>
              <p>{snap.model || '—'}</p>
            </div>
          </li>
          {attempts.map((a, i) => <AttemptStep key={i} a={a} t={t} />)}
        </ol>
        {!attempts.length && <p className="rr-note">{t('没有保存调用明细')}</p>}
      </section>

      {ordered.length > 0 && (
        <section className="rr-section">
          <h4>{t('候选顺序')}</h4>
          <ol className="rr-ordered">
            {ordered.map((c, i) => {
              const picked = selected != null && String(c.id) === String(selected);
              return (
                <li key={`${String(c.id)}-${i}`} className={picked ? 'is-picked' : ''}>
                  <span className="rr-rank">{i + 1}</span>
                  <div className="rr-cand">
                    <div className="rr-cand-head">
                      <b>{candidateName(c)}</b>
                      {c.provider && <span className="rr-cand-provider">{c.provider}</span>}
                      {c.priority != null && <span className="rr-cand-meta">P{c.priority}</span>}
                      {c.weight != null && <span className="rr-cand-meta">W{c.weight}</span>}
                      {picked && <span className="rr-badge rr-badge-brand">{t('选中')}</span>}
                    </div>
                    {c.reason && <p className="rr-cand-reason">{c.reason}</p>}
                  </div>
                </li>
              );
            })}
          </ol>
        </section>
      )}

      {blocked.length > 0 && (
        <section className="rr-section">
          <h4>{t('被跳过的渠道')}</h4>
          <div className="rr-blocked">
            {blocked.map((c, i) => (
              <div key={`${String(c.id)}-${i}`} className="rr-blocked-item">
                <span className={`rr-badge ${c.state === 'open' || c.state === 'half_open' ? 'rr-badge-bad' : 'rr-badge-mute'}`}>
                  {c.state || t('过滤')}
                </span>
                <b>{candidateName(c)}</b>
                <p>{c.reason || ''}</p>
              </div>
            ))}
          </div>
        </section>
      )}

      <section className="rr-section">
        <h4>{t('请求当时的配置')}</h4>
        <div className="rr-kv">
          <span>{t('策略：{{s}}', { s: strategyLabel(route.strategy, t) })}</span>
          <span>{t('会话粘性：{{mode}}', { mode: stickyLabel(route.sticky, t) })}</span>
          {cs?.provider_count != null && (
            <span>{t('{{providers}} 个提供商 / {{accounts}} 个账号', {
              providers: cs.provider_count,
              accounts: cs.account_count ?? 0,
            })}</span>
          )}
          {route.session_id && <span className="rr-mono" title={route.session_id}>{t('会话标识 {{id}}', { id: route.session_id.slice(0, 12) })}</span>}
        </div>
        {captured != null && (
          <p className="rr-note">{t('快照保存于 {{time}}', { time: new Date(captured).toLocaleString() })}</p>
        )}
      </section>
    </div>
  );
}

/** 路由回放面板：粘贴请求 ID 直查（挂「系统设置 → 运维 → 路由」底部） */
export default function RouteReplayPanel(): JSX.Element {
  const { t } = useTranslation();
  const [input, setInput] = useState('');
  const [activeId, setActiveId] = useState<string | number | null>(null);

  const submit = (): void => {
    const v = input.trim();
    if (v) setActiveId(v);
  };

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('路由回放')}</h2>
        <p className="card-subtitle">
          {t('按请求 ID 回放当时的选路快照与执行时间线（只读历史事实，不按当前配置重算）。日志详情的「路由回放」Tab 内嵌同一视图。')}
        </p>
      </div>
      <div className="card-body">
        <div className="form-group">
          <label>{t('请求 ID')}</label>
          <div style={{ display: 'flex', gap: 8 }}>
            <input
              className="form-input"
              placeholder={t('粘贴请求日志里的请求 ID')}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') submit(); }}
            />
            <button className="btn btn-secondary" onClick={submit} disabled={!input.trim()}>
              {t('回放')}
            </button>
          </div>
        </div>
        {activeId != null && <RouteReplayView requestId={activeId} />}
      </div>
    </div>
  );
}
