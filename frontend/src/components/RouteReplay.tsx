/**
 * RouteReplay — 路由回放面板（对照 Agent2API routing-replay / routing-exec-timeline）。
 *
 * 只读渲染请求**当时**保存的选路事实，修改当前路由不会改变这份记录。
 *
 * 数据形状（#31 契约对齐，出处见 types/routing-replay.ts 锚点）：后端投影是
 * **扁平**形状——candidate_channels 只有 id、attempts 是计数、无逐次尝试明细。
 * 本组件在渲染前做归一化：
 *   candidate_channels → 候选序条目（{id}；有渠道名映射则显示名）
 *   filtered_channels  → 被跳过条目（{id, reason}）
 *   selected_channel   → 最终承载（与候选/过滤比对，命中则高亮）
 *   route_config       → 策略/粘性卡
 *   account_skips      → 逐账号跳过分组（折叠 + 截断计数）
 * 空态守卫 = 「有没有任何回放字段」（旧日志/开关关闭 → 全部缺省 → 降级提示）。
 * 「执行路径」时间线在无逐次明细（attempts 只是计数）时整节不渲染——明细
 * 数据缺口由 be-fix #32 补。
 *
 * 数据源：GET /api/routing/replay/{request_id}（be-fix Phase1-B）。
 * 三态降级：端点 404（未就绪）/ 200 无回放字段（旧记录）/ 请求失败。
 * 性能约束：挂载即单次拉取、不轮询、无首屏开销（日志详情 Tab 激活才挂载）。
 */
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { EmptyState, Loading } from './ui';
import type {
  AccountSkipGroup,
  RouteReplayAttemptItem,
  RouteReplayFilteredItem,
  RouteReplaySnapshot,
} from '../types/routing-replay';
import './RouteReplay.css';

/** 秒/毫秒自适应（对齐账号页 expiryMillis 口径：<1e11 视为秒） */
function toMillis(v: number | undefined): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  return v < 1e11 ? v * 1000 : v;
}

/** 耗时收短：ms → 「820ms / 1.2s / 1m04s」（#32 逐次明细落库后时间线恢复使用）
 *
 * 秒档固定 1 位小数：上游 `routing-ui.tsx:56-61 formatDuration` 也是
 * `toFixed(1)`，注释示例「1.2s」同样只有 1 位。此前写成
 * `toFixed(ms < 10_000 ? 2 : 1)`，10s 以下吐「1.00s」，与注释/上游/e2e
 * 断言（routing-replay.spec.ts:161 期望 `1.0s`）三方都不一致。
 */
export function fmtDuration(ms: number | undefined): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
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

/** 逐账号跳过分组（可折叠；形状守卫：groups 缺省/空 = 整节不渲染） */
function AccountSkipGroups({ groups, t }: {
  groups: AccountSkipGroup[] | undefined;
  t: (k: string, opts?: Record<string, unknown>) => string;
}): JSX.Element | null {
  if (!groups?.length) return null;
  return (
    <section className="rr-section">
      <h4>{t('逐账号跳过明细')}</h4>
      <div className="rr-askips">
        {groups.map((g) => (
          <details key={g.channel_id} className="rr-askip-group">
            <summary>
              <code>{g.channel_id}</code>
              <span className="rr-badge rr-badge-mute">{t('{{n}} 个账号', { n: g.skipped.length })}</span>
              {(g.truncated ?? 0) > 0 && (
                <span className="rr-badge rr-badge-bad">{t('还有 {{n}} 条未记录', { n: g.truncated ?? 0 })}</span>
              )}
            </summary>
            <div className="rr-askip-rows">
              {g.skipped.map((r, i) => (
                <div key={`${r.account_id}-${i}`} className="rr-blocked-item">
                  <b>{r.account_name || r.account_id}</b>
                  <p>{r.reason || ''}</p>
                </div>
              ))}
            </div>
          </details>
        ))}
      </div>
    </section>
  );
}

/** 执行时间线的单次尝试（#32 AttemptDetail：渠道/状态/耗时/错误/粘滞命中） */
function AttemptStep({ a, t }: { a: RouteReplayAttemptItem; t: (k: string, opts?: Record<string, unknown>) => string }): JSX.Element {
  const failed = a.error != null;
  const durMs = a.startedAt > 0 && a.finishedAt != null && a.finishedAt >= a.startedAt
    ? (a.finishedAt - a.startedAt) * 1000
    : undefined;
  const barPct = durMs != null ? Math.max(4, Math.min(100, Math.round((durMs / 30_000) * 100))) : null;
  return (
    <li className={`rr-step ${failed ? 'rr-step-failed' : ''}`}>
      <span className="rr-step-mark" aria-hidden="true" />
      <div className="rr-step-body">
        <div className="rr-step-head">
          <b>{a.channel_name || a.channel_id || t('未知供应商')}</b>
          {a.status != null && (
            <span className={`rr-badge ${failed ? 'rr-badge-bad' : 'rr-badge-ok'}`}>{a.status}</span>
          )}
          {a.stickyHit && <span className="rr-badge rr-badge-ok">{t('粘滞命中')}</span>}
          {a.provider && <span className="rr-badge rr-badge-mute">{a.provider}</span>}
          {durMs != null && <span className="rr-step-dur">{fmtDuration(durMs)}</span>}
        </div>
        {a.model && <p className="rr-step-model">{t('上游模型 {{m}}', { m: a.model })}</p>}
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

  /* 归一化：扁平投影 → 渲染模型（字段出处见 types 锚点；全 Array.isArray 守卫，
   * 「数组身上的同名方法」教训见 6fa14e6a） */
  const view = useMemo(() => {
    if (!snap) return null;
    const candidates = (Array.isArray(snap.candidate_channels) ? snap.candidate_channels : [])
      .map((id) => ({ id: String(id) }));
    const filtered: RouteReplayFilteredItem[] = (Array.isArray(snap.filtered_channels)
      ? snap.filtered_channels : [])
      .filter((f) => f && f.channel_id != null)
      .map((f) => ({ channel_id: String(f.channel_id), reason: f.reason || '' }));
    const selected = snap.selected_channel != null ? String(snap.selected_channel) : '';
    const skipsGroups = Array.isArray(snap.account_skips) ? snap.account_skips : [];
    // 逐次尝试明细（#32）：缺省无此字段 = 整节不渲染；index 连续 1..=n
    const details = Array.isArray(snap.attempt_details)
      ? snap.attempt_details.filter((a) => a && a.index != null)
      : [];
    return { candidates, filtered, selected, skipsGroups, details };
  }, [snap]);

  if (loading) return <div className="rr-loading"><Loading text={t('加载路由回放')} /></div>;

  // 端点 404：后端快照能力尚未就绪，空态降级不报错
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
  // 无任何回放字段 = 旧记录或 route_replay 开关关闭（#31：判据是「有没有回放
  // 字段」，不再是 route 对象/attempts 数组——后端 attempts 是计数）
  if (!view || (
    !view.candidates.length && !view.filtered.length
    && !view.selected && !view.skipsGroups.length && !view.details.length && !snap?.route_config
  )) {
    return (
      <div className="rr-degraded">
        <p>{t('该请求没有保存选路快照（旧记录或快照未启用），下方为日志基础字段。')}</p>
      </div>
    );
  }

  const rc = snap?.route_config;
  const captured = toMillis(snap?.created_at);
  const { candidates, filtered, selected, skipsGroups, details } = view;
  const attemptsTruncated = snap?.attempt_details_truncated ?? 0;

  return (
    <div className="rr-root">
      <div className="rr-chips">
        <span className="rr-badge rr-badge-brand">{strategyLabel(rc?.strategy, t)}</span>
        <span className="rr-badge rr-badge-mute">
          {t('会话粘性：{{mode}}', { mode: rc?.sticky_enabled ? t('启用') : t('关闭') })}
        </span>
        {selected && (
          <span className="rr-badge rr-badge-brand">{t('最终承载：{{name}}', { name: selected })}</span>
        )}
      </div>
      <p className="rr-note">{t('这是请求当时保存的选路快照，修改当前路由不会改变这份记录。')}</p>

      <div className="rr-metrics">
        <div><b>{snap?.status_code ?? '—'}</b><span>{t('状态码')}</span></div>
        <div><b>{snap?.attempts ?? '—'}</b><span>{t('尝试次数')}</span></div>
        <div><b>{candidates.length || '—'}</b><span>{t('候选供应商')}</span></div>
        <div><b>{filtered.length || '—'}</b><span>{t('跳过候选')}</span></div>
      </div>

      {/* 执行路径时间线（#32 attempt_details 落库后启用）：缺省无字段 = 整节
          不渲染（旧日志）；断言口径 details.length ≤ attempts 且 index 连续 */}
      {details.length > 0 && (
        <section className="rr-section">
          <h4>{t('执行路径')}</h4>
          <ol className="rr-timeline">
            <li className="rr-step rr-step-entry">
              <span className="rr-step-mark" aria-hidden="true" />
              <div className="rr-step-body">
                <div className="rr-step-head"><b>{t('请求进入')}</b></div>
                <p>
                  {snap?.origin_model && snap.model && snap.origin_model !== snap.model
                    ? t('客户端模型 {{client}} → 实际模型 {{model}}', { client: snap.origin_model, model: snap.model })
                    : (snap?.model || '—')}
                </p>
              </div>
            </li>
            {details.map((a, i) => <AttemptStep key={`${a.index}-${i}`} a={a} t={t} />)}
          </ol>
          {attemptsTruncated > 0 && (
            <p className="rr-note">
              {t('尝试明细超过记录上限，还有 {{n}} 条未记录。', { n: attemptsTruncated })}
            </p>
          )}
        </section>
      )}

      {candidates.length > 0 && (
        <section className="rr-section">
          <h4>{t('候选顺序')}</h4>
          <ol className="rr-ordered">
            {candidates.map((c, i) => {
              const picked = selected && c.id === selected;
              return (
                <li key={`${c.id}-${i}`} className={picked ? 'is-picked' : ''}>
                  <span className="rr-rank">{i + 1}</span>
                  <div className="rr-cand">
                    <div className="rr-cand-head">
                      <b className="rr-mono">{c.id}</b>
                      {picked && <span className="rr-badge rr-badge-brand">{t('选中')}</span>}
                    </div>
                  </div>
                </li>
              );
            })}
          </ol>
        </section>
      )}

      {filtered.length > 0 && (
        <section className="rr-section">
          <h4>{t('被跳过的供应商')}</h4>
          <div className="rr-blocked">
            {filtered.map((f, i) => (
              <div key={`${f.channel_id}-${i}`} className="rr-blocked-item">
                <code>{f.channel_id}</code>
                <p>{f.reason}</p>
              </div>
            ))}
          </div>
          {(snap?.route_truncated ?? 0) > 0 && (
            <p className="rr-note">
              {t('候选明细超过记录上限，还有 {{n}} 条未记录。', { n: snap?.route_truncated ?? 0 })}
            </p>
          )}
        </section>
      )}

      <AccountSkipGroups groups={skipsGroups} t={t} />

      <section className="rr-section">
        <h4>{t('请求当时的配置')}</h4>
        <div className="rr-kv">
          <span>{t('策略：{{s}}', { s: strategyLabel(rc?.strategy, t) })}</span>
          <span>{t('会话粘性：{{mode}}', { mode: rc?.sticky_enabled ? t('启用') : t('关闭') })}</span>
        </div>
        {captured != null && (
          <p className="rr-note">{t('请求时间 {{time}}', { time: new Date(captured).toLocaleString() })}</p>
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
    <div className="card rr-panel">
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
