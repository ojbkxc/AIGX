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
import { RouteReplaySnapshotDialog, SnapshotMembers, type RouteConfigExt } from './RouteReplaySnapshotDialog';
import './RouteReplay.css';

/* ── 上游对齐扩展字段（本地形状，不落 types/routing-replay.ts——主控统一收口） ──
 * 以下字段后端 RequestLog 已落库，但 handle_get_route_replay 投影暂未带出；
 * 前端按「缺省 = 对应块不渲染」守卫，后端补投影后即自动出现。
 *   special_route     → src/log/mod.rs SpecialRouteTrace（compact/title/vision 改写留痕）
 *   first_response_ms → RequestLog.first_response_ms（首帧 TTFB）
 *   output_tokens     → RequestLog.output_tokens（输出 tokens）
 *   route_config.members → 上游 configSnapshot.members（当时的成员模型快照，AIGX 未落库）
 */
export interface SpecialRouteTrace {
  kind?: string;
  from?: string | null;
  to?: string | null;
  mode?: string | null;
}
export interface RouteReplaySnapshotExt extends RouteReplaySnapshot {
  special_route?: SpecialRouteTrace | null;
  first_response_ms?: number | null;
  output_tokens?: number | null;
}

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

/** 改写类型徽标（上游 routing-exec-rewrite：compact/title/vision 三态）。
 *  数据 = RequestLog.special_route（后端已落库，回放投影带出后即显示）；
 *  缺省 = 不渲染徽标，只保留改写前后模型对照。 */
function rewriteBadge(special: SpecialRouteTrace | null | undefined): { label: string; tone: 'brand' | 'warn' | 'mute' } | null {
  if (!special?.kind) return null;
  switch (special.kind) {
    case 'compact': return { label: '压缩上下文', tone: 'brand' };
    case 'title': return { label: '标题生成', tone: 'mute' };
    case 'vision': return { label: '图片兼容', tone: 'warn' };
    default: return { label: special.kind, tone: 'mute' };
  }
}

/** vision 改写的说明行（上游：辅助识图 vs 整次接管；mode 仅 vision 有值） */
function rewriteDetail(special: SpecialRouteTrace | null | undefined): string | null {
  if (!special || special.kind !== 'vision') return null;
  const assist = special.mode === 'assist_call' || special.mode === 'assist';
  return assist ? '辅助识图，读图结果交回原模型继续回答' : '整次请求交给视觉模型';
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

/** 执行时间线的单次尝试（#32 AttemptDetail：渠道/状态/耗时/错误/粘滞命中）。
 *
 *  segment 色条（上游 routing-exec-sections:34-37 对齐位）：在**全体尝试共享
 *  的时间尺**上定位本次起止——多次尝试时一眼看出谁先谁后、谁占了多长时间。
 *  AIGX AttemptDetail 只有 startedAt/finishedAt（无 responseAt / 首帧读数），
 *  所以色条是单段（成功=主色 / 失败=红）；上游的「等待响应→等待首帧→输出」
 *  三段拆解需后端补 AttemptDetail.responseAt + 逐次 firstFrame。 */
function AttemptStep({ a, t, scale, origin }: {
  a: RouteReplayAttemptItem;
  t: (k: string, opts?: Record<string, unknown>) => string;
  /** 共享时间尺（ms，全体尝试里最晚结束 - 最早开始）；0 = 不画色条 */
  scale: number;
  /** 时间尺原点（unix 秒，全体尝试里最早开始） */
  origin: number;
}): JSX.Element {
  const failed = a.error != null;
  const durMs = a.startedAt > 0 && a.finishedAt != null && a.finishedAt >= a.startedAt
    ? (a.finishedAt - a.startedAt) * 1000
    : undefined;
  const seg = ((): { left: number; width: number } | null => {
    if (!scale || !(a.startedAt > 0) || a.finishedAt == null || a.finishedAt < a.startedAt) return null;
    const start = (a.startedAt - origin) * 1000;
    const end = (a.finishedAt - origin) * 1000;
    if (end < 0 || start > scale) return null;
    const left = Math.max(0, Math.min(100, (start / scale) * 100));
    const right = Math.max(0, Math.min(100, (end / scale) * 100));
    return { left, width: Math.max(1.5, right - left) };
  })();
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
        {seg != null && (
          <div
            className="rr-step-segtrack"
            role="img"
            aria-label={`${t('本次用时')} ${fmtDuration(durMs)}`}
            style={{ position: 'relative', marginTop: 5, height: 4, borderRadius: 2, background: 'var(--border, #ececec)', maxWidth: 280, overflow: 'hidden' }}
          >
            <i
              title={`${a.channel_name || a.channel_id} · ${fmtDuration(durMs)}`}
              style={{
                position: 'absolute', top: 0, bottom: 0,
                left: `${seg.left}%`, width: `${seg.width}%`,
                borderRadius: 2,
                background: failed ? 'var(--danger-color, #dc2626)' : 'var(--primary, #2563eb)',
              }}
            />
          </div>
        )}
        {a.error && <p className="rr-step-err">{a.error}</p>}
        {!a.error && a.notice && <p className="rr-step-note">{a.notice}</p>}
      </div>
    </li>
  );
}

/** 单请求回放视图：懒挂载 + 单次拉取（调用方控制挂载时机即可实现按需加载） */
export function RouteReplayView({
  requestId,
  onReset,
}: {
  requestId: string | number;
  /** 清空当前选中，回到「粘贴请求 ID」的回放面板（默认无入口，由页面传入）。 */
  onReset?: () => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(true);
  const [errStatus, setErrStatus] = useState<number | null>(null);
  const [errMsg, setErrMsg] = useState('');
  const [snap, setSnap] = useState<RouteReplaySnapshotExt | null>(null);
  const [cfgDialogOpen, setCfgDialogOpen] = useState(false);
  // 换一条请求时收起对比弹窗（上游 RoutingExecSnapshot 同口径：row.id/ts 变即复位）
  useEffect(() => { setCfgDialogOpen(false); }, [requestId]);

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

  if (loading) {
    return (
      <div className="rr-loading">
        <Loading text={t('加载路由回放')} />
        {onReset ? (
          <button type="button" className="rr-exec-reset" onClick={onReset}>
            {t('重新选择请求')}
          </button>
        ) : null}
      </div>
    );
  }

  // 端点 404：后端快照能力尚未就绪，空态降级不报错
  if (errStatus === 404) {
    return (
      <div>
        <EmptyState
          message={t('路由回放快照端点尚未就绪（后端建设中）。就绪后这里会显示该请求当时的候选顺序、跳过原因与执行时间线。')}
        />
        {onReset ? (
          <button type="button" className="rr-exec-reset" onClick={onReset} style={{ marginTop: 8 }}>
            {t('重新选择请求')}
          </button>
        ) : null}
      </div>
    );
  }
  if (errMsg) {
    return (
      <div className="rr-degraded">
        <p>{t('路由回放加载失败：{{msg}}', { msg: errMsg })}</p>
        {onReset ? (
          <button type="button" className="rr-exec-reset" onClick={onReset} style={{ marginTop: 8 }}>
            {t('重新选择请求')}
          </button>
        ) : null}
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
        {onReset ? (
          <button type="button" className="rr-exec-reset" onClick={onReset} style={{ marginTop: 8 }}>
            {t('重新选择请求')}
          </button>
        ) : null}
      </div>
    );
  }

  const rc = snap?.route_config as RouteConfigExt | null | undefined;
  const captured = toMillis(snap?.created_at);
  const { candidates, filtered, selected, skipsGroups, details } = view;
  const attemptsTruncated = snap?.attempt_details_truncated ?? 0;

  /**
   * 承载段的「原因」：只在同一渠道既出现在候选又被记录过错误时才有话说。
   * 没有就留空 —— 不猜（与「provenance 不许猜」同一条纪律）。
   */
  const carrierReason = ((): string | null => {
    if (!selected) return null;
    const hit = filtered.find((f) => f.channel_id === selected);
    if (hit?.reason) return hit.reason;
    const step = details.find((a) => (a.channel_id || '') === selected && a.error);
    return step?.error ?? null;
  })();

  /** 执行耗时：逐次明细里最后一条的耗时（A2A 的「总耗时 / 首帧 / 输出」口径） */
  const totalMs = ((): number | undefined => {
    const last = details[details.length - 1];
    if (!last) return undefined;
    if (last.startedAt > 0 && last.finishedAt != null && last.finishedAt >= last.startedAt) {
      return (last.finishedAt - last.startedAt) * 1000;
    }
    return undefined;
  })();

  /** 首帧（TTFB）：后端 RequestLog.first_response_ms 已落库，回放投影带出后即显示 */
  const firstFrameMs = typeof snap?.first_response_ms === 'number' && Number.isFinite(snap.first_response_ms)
    ? snap.first_response_ms : undefined;
  /** 输出 tokens：后端 RequestLog.output_tokens 已落库，回放投影带出后即显示 */
  const outputTokens = typeof snap?.output_tokens === 'number' && Number.isFinite(snap.output_tokens)
    ? snap.output_tokens : undefined;

  /** segment 色条的共享时间尺：全体尝试的 [最早开始, 最晚结束]（unix 秒 → ms 跨度）。
   *  只有 1 次尝试时尺子退化为本次时长（条占满），多次才体现先后错位。 */
  const segScale = ((): { origin: number; scale: number } => {
    const spans = details
      .filter((a) => a.startedAt > 0 && a.finishedAt != null && a.finishedAt >= a.startedAt)
      .map((a) => ({ s: a.startedAt, e: a.finishedAt as number }));
    if (!spans.length) return { origin: 0, scale: 0 };
    const origin = Math.min(...spans.map((p) => p.s));
    const end = Math.max(...spans.map((p) => p.e));
    return { origin, scale: Math.max(1, (end - origin) * 1000) };
  })();

  /** 选路情况表：候选（已尝试/待尝试）+ 被跳过（未参与），一行一个账号/渠道 */
  const routeRows = ((): Array<{ key: string; name: string; state: string; reason: string }> => {
    const triedIndex = new Map<string, RouteReplayAttemptItem>();
    details.forEach((a) => {
      const key = a.channel_id || a.channel_name || '';
      if (key) triedIndex.set(key, a);
    });
    const rows = candidates.map((c) => {
      const tried = triedIndex.get(c.id);
      const failed = Boolean(tried?.error);
      return {
        key: `c-${c.id}`,
        name: c.id,
        state: tried ? (failed ? t('尝试失败') : t('已尝试')) : t('待尝试'),
        reason: tried?.error || tried?.notice || '',
      };
    });
    filtered.forEach((f) => {
      rows.push({ key: `f-${f.channel_id}`, name: f.channel_id, state: t('未参与'), reason: f.reason || '' });
    });
    return rows;
  })();

  /** 折叠的「其他候选」：A2A 只先露 2 条 + 折叠其余 */
  const ROUTE_ROWS_VISIBLE = 6;

  return (
    <div className="rr-root">
      {/* ① 头部（A2A routing-exec-head）：模型 + 状态 + 时间 + HTTP 状态码 */}
      <header className="rr-exec-head">
        <div>
          <h3 className="rr-exec-title">{snap?.origin_model || snap?.model || t('（未记录模型）')}</h3>
          <div className="rr-exec-meta">
            <span className={`rr-exec-state${(snap?.status_code ?? 0) >= 400 ? ' is-bad' : ''}`}>
              {(snap?.status_code ?? 0) >= 400 ? t('失败') : t('成功')}
            </span>
            {captured != null ? (
              <time title={new Date(captured).toLocaleString()}>
                {new Date(captured).toLocaleTimeString()}
              </time>
            ) : null}
            {snap?.status_code != null ? <span>{`HTTP ${snap.status_code}`}</span> : null}
            {snap?.request_id || snap?.log_id != null ? (
              <span className="rr-exec-reqid">{String(snap?.request_id ?? snap?.log_id)}</span>
            ) : null}
          </div>
        </div>
        {onReset ? (
          <button type="button" className="rr-exec-reset" onClick={onReset}>
            {t('重新选择请求')}
          </button>
        ) : null}
      </header>

      {/* ② 模型改写（A2A routing-exec-rewrite）：客户端模型 → 实际使用；无改写不渲染。
          类型徽标 = special_route.kind（压缩/标题/图片兼容），回放投影带出后即显示。 */}
      {snap?.origin_model && snap?.model && snap.origin_model !== snap.model ? (
        <section className="rr-section rr-exec-rewrite">
          <header className="rr-exec-rewrite-head" style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
            <b>{t('模型改写')}</b>
            {(() => {
              const badge = rewriteBadge(snap.special_route);
              if (!badge) return null;
              return (
                <span className={`rr-badge ${badge.tone === 'brand' ? 'rr-badge-brand' : badge.tone === 'warn' ? 'rr-badge-bad' : 'rr-badge-mute'}`}>
                  {t(badge.label)}
                </span>
              );
            })()}
            {(() => {
              const detail = rewriteDetail(snap.special_route);
              return detail ? <span className="rr-note" style={{ margin: 0 }}>{t(detail)}</span> : null;
            })()}
          </header>
          <div className="rr-exec-rewrite-flow">
            <div className="rr-exec-node">
              <span>{t('客户端模型')}</span>
              <b>{snap.special_route?.kind === 'vision' && snap.special_route.from ? snap.special_route.from : snap.origin_model}</b>
            </div>
            <span className="rr-exec-arrow" aria-hidden="true">→</span>
            <div className="rr-exec-node is-accent">
              <span>{t('实际使用')}</span>
              <b>{snap.special_route?.to || snap.model}</b>
            </div>
          </div>
          {!snap.special_route?.kind ? (
            <p className="rr-note">
              {t('改写类型（压缩上下文 / 标题生成 / 图片兼容）由特殊请求路由记录，本页快照未带该字段，故只显示改写前后模型。')}
            </p>
          ) : null}
        </section>
      ) : null}

      {/* ③ 承载（A2A routing-exec-carrier）：最终承载 + 原因 */}
      <section className="rr-section rr-exec-carrier" data-failed={carrierReason ? '' : undefined}>
        <p className="rr-exec-label">{t('最终承载')}</p>
        <p className="rr-exec-carrier-name">{selected || t('未记录')}</p>
        {carrierReason ? <p className="rr-exec-carrier-reason">{carrierReason}</p> : null}
      </section>

      {/* ④ 执行耗时（A2A routing-exec-duration）：总耗时 / 首帧 / 输出 + 逐次 segment 色条。
          首帧 = RequestLog.first_response_ms、输出 = output_tokens（均已落库，回放投影带出即显示） */}
      <section className="rr-section">
        <div className="rr-exec-section-head">
          <h4>{t('执行耗时')}</h4>
          <span className="rr-exec-section-note">
            {[
              `${t('总耗时')} ${fmtDuration(totalMs)}`,
              `${t('首帧')} ${fmtDuration(firstFrameMs)}`,
              `${t('输出')} ${outputTokens != null ? `${outputTokens} tokens` : '—'}`,
            ].join(' · ')}
          </span>
        </div>
        {details.length ? (
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
            {details.map((a, i) => (
              <AttemptStep key={`${a.index}-${i}`} a={a} t={t} scale={segScale.scale} origin={segScale.origin} />
            ))}
          </ol>
        ) : (
          <p className="rr-exec-note">{t('没有保存调用明细')}</p>
        )}
        {attemptsTruncated > 0 ? (
          <p className="rr-note">
            {t('尝试明细超过记录上限，还有 {{n}} 条未记录。', { n: attemptsTruncated })}
          </p>
        ) : null}
      </section>

      {/* ⑤ 选路情况（A2A routing-exec-candidates）：账号 / 本次状态 / 原因 */}
      <section className="rr-section">
        <div className="rr-exec-section-head">
          <h4>{t('选路情况')}</h4>
          <span className="rr-exec-section-note">
            {t('已保存 {{n}} 个候选', { n: candidates.length + filtered.length })}
          </span>
        </div>
        {routeRows.length ? (
          <>
            <div className="rr-exec-table" role="table" aria-label={t('选路情况')}>
              <div className="rr-exec-row is-heading" role="row">
                <span role="columnheader">{t('账号')}</span>
                <span role="columnheader">{t('本次状态')}</span>
                <span role="columnheader">{t('原因')}</span>
              </div>
              {routeRows.slice(0, ROUTE_ROWS_VISIBLE).map((row) => (
                <div className="rr-exec-row" role="row" key={row.key}>
                  <span role="cell" className="rr-mono">{row.name}</span>
                  <span role="cell" data-tone={row.state === t('尝试失败') ? 'bad' : row.state === t('未参与') ? 'mute' : 'ok'}>
                    {row.state}
                  </span>
                  <span role="cell" className="rr-exec-reason">{row.reason || '—'}</span>
                </div>
              ))}
            </div>
            {routeRows.length > ROUTE_ROWS_VISIBLE ? (
              <details className="rr-exec-more">
                <summary>{t('查看其他 {{n}} 个候选', { n: routeRows.length - ROUTE_ROWS_VISIBLE })}</summary>
                <div className="rr-exec-table" role="table">
                  {routeRows.slice(ROUTE_ROWS_VISIBLE).map((row) => (
                    <div className="rr-exec-row" role="row" key={row.key}>
                      <span role="cell" className="rr-mono">{row.name}</span>
                      <span role="cell" data-tone={row.state === t('尝试失败') ? 'bad' : row.state === t('未参与') ? 'mute' : 'ok'}>
                        {row.state}
                      </span>
                      <span role="cell" className="rr-exec-reason">{row.reason || '—'}</span>
                    </div>
                  ))}
                </div>
              </details>
            ) : null}
          </>
        ) : (
          <p className="rr-exec-note">{t('没有保存其他候选信息')}</p>
        )}
        {(snap?.route_truncated ?? 0) > 0 ? (
          <p className="rr-note">
            {t('候选明细超过记录上限，还有 {{n}} 条未记录。', { n: snap?.route_truncated ?? 0 })}
          </p>
        ) : null}
      </section>

      {/* ⑥ 当时配置（A2A routing-exec-snapshot）：配置范围 / 选路方式 / 会话粘滞 / 上游模型
          + 「查看配置详情」对比弹窗（当时快照 vs 当前配置，D1）
          + 成员模型快照展开（上游 SnapshotMembers，D5；members 缺省 = 不渲染） */}
      <section className="rr-section">
        <div className="rr-exec-section-head">
          <h4>{t('当时配置')}</h4>
          <button
            type="button"
            className="rr-exec-reset"
            style={{ marginLeft: 0, alignSelf: 'auto' }}
            onClick={() => setCfgDialogOpen(true)}
          >
            {t('查看配置详情')}
          </button>
        </div>
        <dl className="rr-exec-definitions">
          <div>
            <dt>{t('配置范围')}</dt>
            <dd>{t('未记录')}</dd>
          </div>
          <div>
            <dt>{t('选路方式')}</dt>
            <dd>{rc?.strategy ? strategyLabel(rc.strategy, t) : t('未记录')}</dd>
          </div>
          <div>
            <dt>{t('会话粘滞')}</dt>
            <dd>{rc ? (rc.sticky_enabled ? t('启用') : t('关闭')) : t('未记录')}</dd>
          </div>
          <div className="is-wide">
            <dt>{t('上游模型')}</dt>
            <dd className="rr-mono">{snap?.model || '—'}</dd>
          </div>
        </dl>
        {(() => {
          const members = (rc as RouteConfigExt | undefined)?.members;
          return Array.isArray(members) && members.length ? (
            <details className="rr-askip-group" style={{ marginTop: 8 }}>
              <summary>
                <span>{t('成员模型')}</span>
                <span className="rr-badge rr-badge-mute">{t('{{n}} 个', { n: members.length })}</span>
              </summary>
              <SnapshotMembers members={members} />
            </details>
          ) : null;
        })()}
        {captured != null ? (
          <p className="rr-note">{t('请求时间 {{time}}', { time: new Date(captured).toLocaleString() })}</p>
        ) : null}
      </section>
      {cfgDialogOpen ? (
        <RouteReplaySnapshotDialog
          model={snap?.origin_model || snap?.model || ''}
          strategy={rc?.strategy}
          stickyEnabled={rc?.sticky_enabled}
          capturedAt={captured}
          onClose={() => setCfgDialogOpen(false)}
        />
      ) : null}

      {/* AIGX 独有：逐账号跳过明细（A2A 把它并进「选路情况」的原因列） */}
      <AccountSkipGroups groups={skipsGroups} t={t} />
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
