/**
 * AI 运维页右侧「运行信息」栏——给「用 AI 运维网关」这条主链路配的**只读观测面**。
 *
 * 为什么放在这里：`/agent` 右侧此前被 A2A 客户端检测面板占着（检测本机装了哪些
 * AI 编码客户端并帮它们接网关），那件事与「用 AI 运维网关」无关，已下架。空出来的
 * 右栏改成运维自己要看的东西——同理，这里**只读**，不承载任何写入口（写操作仍然
 * 只能由 AI 通过工具+审批去做，人在这里点按钮改网关是另一条链路）。
 *
 * 取数全部走**已经存在的管理员接口**，不臆造后端：
 *   · `GET /api/purge`                    → `runtime.{uptime_secs,inflight,upstream}` +
 *                                            `stats_24h.responses`（`src/api/admin/purge.rs`）
 *   · `GET /api/channels`                 → 渠道清单，数 `status === 'enabled'` 得活跃数
 *   · `GET /api/logs/requests?size=N`     → 最近请求（含 `status_code`/`latency_ms`/`error_msg`）
 * 四个请求各自 `allSettled`：任何一段失败只是那一段显示空态，**不牵连**整栏
 * 与页面（运维面板取不到数不该让对话主流程看起来坏了）。
 */
import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Activity, AlertTriangle, ChevronRight, RefreshCw } from 'lucide-react';
import { api } from '../api';

/** 最近请求一行（只取本栏展示需要的字段，其余不猜） */
interface RecentRow {
  id: string;
  created_at: number;
  model: string;
  status_code: number;
  latency_ms: number;
  error_msg: string | null;
}

/** 网关运行摘要（缺失字段一律按「未知」渲染，不补 0 冒充真实值） */
interface GateSummary {
  uptimeSecs: number | null;
  inflight: number | null;
  upstream: string;
  requests24h: number | null;
}

const RECENT_LIMIT = 5;
const REFRESH_MS = 30_000;

/** 秒 → 「2 天 3 小时」/「5 分钟」/「12 秒」；null = 未知 */
export function humanUptime(secs: number | null, t: (k: string) => string): string {
  if (secs == null || !Number.isFinite(secs) || secs < 0) return '—';
  const days = Math.floor(secs / 86_400);
  const hours = Math.floor((secs % 86_400) / 3_600);
  const mins = Math.floor((secs % 3_600) / 60);
  if (days > 0) return `${days} ${t('天')} ${hours} ${t('小时')}`;
  if (hours > 0) return `${hours} ${t('小时')} ${mins} ${t('分钟')}`;
  if (mins > 0) return `${mins} ${t('分钟')}`;
  return `${secs} ${t('秒')}`;
}

/** unix 秒 → 本地 `MM-DD HH:mm:ss`（后端 `created_at` 是秒，不是毫秒） */
export function fmtStamp(sec: number): string {
  const d = new Date(sec * 1000);
  if (Number.isNaN(d.getTime())) return '—';
  const p = (n: number): string => String(n).padStart(2, '0');
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/** 后端日志行 → 本栏行；缺 `created_at` 的行直接丢弃（排不出先后就不排） */
export function toRecentRows(rows: unknown): RecentRow[] {
  if (!Array.isArray(rows)) return [];
  const out: RecentRow[] = [];
  for (const raw of rows) {
    if (!raw || typeof raw !== 'object') continue;
    const r = raw as Record<string, unknown>;
    const createdAt = Number(r.created_at);
    if (!Number.isFinite(createdAt) || createdAt <= 0) continue;
    out.push({
      id: String(r.id ?? createdAt),
      created_at: createdAt,
      model: typeof r.model === 'string' ? r.model : '',
      status_code: Number(r.status_code) || 0,
      latency_ms: Number(r.latency_ms) || 0,
      error_msg: typeof r.error_msg === 'string' && r.error_msg ? r.error_msg : null,
    });
  }
  return out.slice(0, RECENT_LIMIT);
}

/** 一行算不算「错误」：非 2xx，或带错误文本（与 /logs 的状态口径一致） */
function isFailed(row: RecentRow): boolean {
  return (row.status_code !== 0 && (row.status_code < 200 || row.status_code >= 300)) || !!row.error_msg;
}

/** 右栏运行信息面板。挂载即取数、每 30s 自刷新；卸载即停（窄屏下由抽屉挂载）。 */
export default function AgentOpsPanel({ onClose }: { onClose?: () => void } = {}): JSX.Element {
  const { t } = useTranslation();
  const [summary, setSummary] = useState<GateSummary | null>(null);
  const [channels, setChannels] = useState<{ enabled: number; total: number } | null>(null);
  const [recent, setRecent] = useState<RecentRow[] | null>(null);
  const [failed, setFailed] = useState<RecentRow[] | null>(null);
  const [busy, setBusy] = useState(false);
  /** 本次刷新是否整栏失败（四段全挂 = 后端/鉴权有问题，值得显式说一句） */
  const [allFailed, setAllFailed] = useState(false);
  const aliveRef = useRef(true);

  const load = useCallback(async (): Promise<void> => {
    setBusy(true);
    const [purge, chs, logs] = await Promise.allSettled([
      api.getPurge(),
      api.listChannels(),
      api.listRequestLogs({ size: RECENT_LIMIT }),
    ]);
    if (!aliveRef.current) return;

    // ── 网关状态（runtime + 24h 计数）──
    if (purge.status === 'fulfilled') {
      const data = (purge.value as unknown as {
        data?: {
          runtime?: { uptime_secs?: number; inflight?: number; upstream?: string };
          stats_24h?: { responses?: number };
        };
      }).data;
      const rt = data?.runtime;
      setSummary({
        uptimeSecs: Number.isFinite(Number(rt?.uptime_secs)) ? Number(rt?.uptime_secs) : null,
        // `inflight` 旧后端不下发 → 明确是「未知」而不是 0
        inflight: Number.isFinite(Number(rt?.inflight)) ? Number(rt?.inflight) : null,
        upstream: typeof rt?.upstream === 'string' ? rt.upstream : '',
        requests24h: Number.isFinite(Number(data?.stats_24h?.responses))
          ? Number(data?.stats_24h?.responses)
          : null,
      });
    } else {
      setSummary(null);
    }

    // ── 活跃渠道 ──
    if (chs.status === 'fulfilled') {
      const list = (chs.value as unknown as { data?: Array<{ status?: string }> }).data;
      const arr = Array.isArray(list) ? list : [];
      setChannels({
        enabled: arr.filter((c) => (c?.status ?? '') === 'enabled').length,
        total: arr.length,
      });
    } else {
      setChannels(null);
    }

    // ── 最近请求 / 最近错误（同一次取数的两个视图，错误从同一批里筛）──
    if (logs.status === 'fulfilled') {
      const rows = toRecentRows((logs.value as unknown as { data?: unknown }).data);
      setRecent(rows);
      setFailed(rows.filter(isFailed));
    } else {
      setRecent(null);
      setFailed(null);
    }

    setAllFailed(purge.status === 'rejected' && chs.status === 'rejected' && logs.status === 'rejected');
    setBusy(false);
  }, []);

  useEffect(() => {
    aliveRef.current = true;
    void load();
    const timer = window.setInterval(() => { void load(); }, REFRESH_MS);
    return () => {
      aliveRef.current = false;
      window.clearInterval(timer);
    };
  }, [load]);

  /** 小标题行（右侧留刷新/收起位） */
  const head = (title: string): JSX.Element => (
    <div className="agent-ops-head">
      <span className="agent-ops-title">{title}</span>
    </div>
  );

  const loading = busy && summary === null && channels === null && recent === null;

  return (
    <aside className="agent-ops" aria-label={t('运行信息')}>
      <div className="agent-ops-bar">
        <span className="agent-ops-brand">
          <Activity size={13} />
          {t('运行信息')}
        </span>
        <span className="agent-ops-bar-actions">
          <button
            type="button"
            className="agent-ops-icon-btn"
            title={t('刷新')}
            aria-label={t('刷新')}
            disabled={busy}
            onClick={() => { void load(); }}
          >
            <RefreshCw size={13} className={busy ? 'is-spinning' : ''} />
          </button>
          {onClose && (
            <button
              type="button"
              className="agent-ops-icon-btn"
              title={t('收起')}
              aria-label={t('收起')}
              onClick={onClose}
            >
              <ChevronRight size={14} />
            </button>
          )}
        </span>
      </div>

      {loading && <div className="agent-ops-note">{t('加载中…')}</div>}

      {allFailed && (
        <div className="agent-ops-note error">
          {t('加载失败')}
          <button type="button" className="agent-ops-link" onClick={() => { void load(); }}>{t('重试')}</button>
        </div>
      )}

      <div className="agent-ops-body">
        {/* ── 网关运行状态 ── */}
        <section className="agent-ops-sec">
          {head(t('网关状态'))}
          {summary ? (
            <dl className="agent-ops-kv">
              <div>
                <dt>{t('运行时长')}</dt>
                <dd>{humanUptime(summary.uptimeSecs, t)}</dd>
              </div>
              <div>
                <dt>{t('在途请求')}</dt>
                <dd>{summary.inflight == null ? '—' : summary.inflight}</dd>
              </div>
              <div>
                <dt>{`${t('请求数')} · 24${t('小时')}`}</dt>
                <dd>{summary.requests24h == null ? '—' : summary.requests24h}</dd>
              </div>
              <div>
                <dt>{t('上游')}</dt>
                <dd title={summary.upstream}>{summary.upstream || '—'}</dd>
              </div>
            </dl>
          ) : (
            !loading && <div className="agent-ops-empty">{t('暂无数据')}</div>
          )}
        </section>

        {/* ── 供应商活跃数 ── */}
        <section className="agent-ops-sec">
          {head(t('供应商'))}
          {channels ? (
            <div className="agent-ops-count">
              <span className="agent-ops-count-main">{channels.enabled}</span>
              <span className="agent-ops-count-sub">
                {`${t('已启用')} / ${t('共')} ${channels.total} ${t('个')}`}
              </span>
            </div>
          ) : (
            !loading && <div className="agent-ops-empty">{t('暂无数据')}</div>
          )}
        </section>

        {/* ── 最近请求 ── */}
        <section className="agent-ops-sec">
          {head(t('最近请求'))}
          {recent && recent.length > 0 ? (
            <ul className="agent-ops-list">
              {recent.map((row) => (
                <li key={row.id} className={`agent-ops-row${isFailed(row) ? ' is-bad' : ''}`}>
                  <span className="agent-ops-row-top">
                    <code className="agent-ops-model">{row.model || '—'}</code>
                    <span className="agent-ops-code">{row.status_code || '—'}</span>
                  </span>
                  <span className="agent-ops-row-bottom">
                    <span className="agent-ops-time">{fmtStamp(row.created_at)}</span>
                    <span className="agent-ops-lat">{`${row.latency_ms} ms`}</span>
                  </span>
                </li>
              ))}
            </ul>
          ) : (
            !loading && <div className="agent-ops-empty">{t('暂无数据')}</div>
          )}
        </section>

        {/* ── 最近错误（同一批里筛出来的；空 = 好事，说「正常」而不是「暂无数据」）── */}
        <section className="agent-ops-sec">
          {head(t('最近错误'))}
          {failed && failed.length > 0 ? (
            <ul className="agent-ops-list">
              {failed.map((row) => (
                <li key={`err-${row.id}`} className="agent-ops-row is-bad">
                  <span className="agent-ops-row-top">
                    <AlertTriangle size={11} />
                    <code className="agent-ops-model">{row.model || '—'}</code>
                    <span className="agent-ops-code">{row.status_code || '—'}</span>
                  </span>
                  <span className="agent-ops-err" title={row.error_msg ?? ''}>
                    {row.error_msg ?? t('错误')}
                  </span>
                </li>
              ))}
            </ul>
          ) : (
            !loading && failed !== null && <div className="agent-ops-empty ok">{t('正常')}</div>
          )}
          {failed === null && !loading && <div className="agent-ops-empty">{t('暂无数据')}</div>}
        </section>
      </div>
    </aside>
  );
}