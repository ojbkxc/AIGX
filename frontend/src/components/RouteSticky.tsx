/**
 * RouteSticky — 粘滞会话管理（对照 Agent2API「路由 → 粘性会话」routing-sticky.tsx +
 * api/routing_workspace.rs get_sticky/clear_sticky）。
 *
 * 列出当前粘住的会话（标识/模型/渠道/到期时刻）、按标识或模型搜索、逐条清除、
 * 全部清除（二次确认）。粘滞让同会话跟住同一渠道以复用上游上下文缓存；
 * 排障时「换渠道试试」就用这里清掉粘滞。
 *
 * 数据源：GET /api/routing/sticky?query= / POST /api/routing/sticky/clear
 * （后端 be-fix #38 已入库，契约锚点 src/api/admin/routing.rs
 * handle_get_route_sticky）：
 * - 行 id = 内部编码 `session_id\u{1f}model`，清除时**原样回传**；
 *   展示时按 U+001F 拆分（控制字符不可见，整串截断会把模型名切掉）
 * - `expires_at` 是 unix 秒（toMillis 的 <1e11 口径自适应）
 * - `channel_name` 优先显示（渠道已删则 null → 回落 `#channel_id`）
 * - 后端按 500 条截断、无分页参数：超出由 `total` / `truncated` 回执
 * 性能约束：手动刷新 + 搜索触发拉取，不轮询（搜索去抖 300ms）。
 */
import { useCallback, useEffect, useReducer, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { RefreshCw, Search } from 'lucide-react';
import { api } from '../api';
import { useToast } from './Toast';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';
import { Badge, EmptyState, Loading } from './ui';
import type { RouteStickyList } from '../types/routing-replay';

/** 秒/毫秒自适应（<1e11 视为秒，对齐账号页口径） */
function toMillis(v: number | undefined): number | null {
  if (v == null || !Number.isFinite(v)) return null;
  return v < 1e11 ? v * 1000 : v;
}

/** 后端 id 的内部编码分隔符（src/api/admin/routing.rs ROUTE_STICKY_ID_SEP） */
const ID_SEP = '\u001f';

/**
 * 拆分展示用 id：`session\u{1f}model` → 两段。
 * 无分隔符（旧桩/异常形状）时整串当会话标识，不抛错。
 */
function splitStickyId(id: string): { session: string; model: string } {
  const sep = id.indexOf(ID_SEP);
  if (sep < 0) return { session: id, model: '' };
  return { session: id.slice(0, sep), model: id.slice(sep + 1) };
}

/**
 * 剩余时长人话化（对齐上游 routing-ui `formatDuration` 的口径）：
 * 秒 → `x 秒` / `x 分 y 秒` / `x 时 y 分`。用于「后过期」那一格。
 */
function formatDuration(ms: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return '—';
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} 秒`;
  if (s < 3600) return `${Math.floor(s / 60)} 分 ${s % 60} 秒`;
  return `${Math.floor(s / 3600)} 时 ${Math.floor((s % 3600) / 60)} 分`;
}

/**
 * 时间戳 → 本地可读串（对齐上游 `timeText`）。缺值返回空串（调用处再回落占位）。
 * 秒/毫秒自适应：`ts < 1e11` 视为 unix 秒。
 */
function timeText(ts: number | undefined | null): string {
  if (ts == null || !Number.isFinite(ts)) return '';
  return new Date(ts < 1e11 ? ts * 1000 : ts).toLocaleString();
}

/**
 * 会话来源人话标签（对齐上游 routing-sticky 行）。
 * 上游有 `content-hash` 这一档；AIGX 以 `session_id` 为粘性键、后端当前
 * **不返回 `source` 字段**（结构性缺口，需后端补来源维度），故无值时显式标
 * 「后端未记录」，绝不按 session 形状臆测成「内容哈希」。
 */
function stickySourceLabel(source: string | undefined): string {
  if (source === 'content-hash') return '内容哈希';
  if (source) return source;
  return '会话来源：后端未记录';
}

/**
 * entries 状态用 useReducer 而非 useState（CI e2e 崩溃防御）。
 *
 * 现象：/settings 路由 Tab 曾 100% 崩 `Object.entries(null) ← basicStateReducer
 * ← useState`——有函数 action 进了本组件首个 useState 的更新队列（React 把
 * `setState(fn)` 当惰性更新，fn 拿上一 state 当参执行）。源码三处 setEntries
 * 全部是值更新（`res.data.entries ?? []` / `[]`），产物级排查也无法定位派发源。
 *
 * 防御：reducer 显式丢弃函数 action——无论谁误派函数，reducer 都不会执行它，
 * 只按「数组则取值、否则保持原值」收敛。行为与 setState 完全一致（值更新路径），
 * 但把整类「函数进队列」崩溃封死。
 */
type EntriesAction = RouteStickyList['entries'] | ((prev: RouteStickyList['entries']) => RouteStickyList['entries']);
function entriesReducer(
  prev: RouteStickyList['entries'],
  action: EntriesAction,
): RouteStickyList['entries'] {
  if (typeof action === 'function') return prev;
  return Array.isArray(action) ? action : prev;
}

export default function RouteSticky(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [entries, setEntries] = useReducer(entriesReducer, [] as RouteStickyList['entries']);
  const [stickyMode, setStickyMode] = useState('');
  const [total, setTotal] = useState(0);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [degraded, setDegraded] = useState(false); // 端点 404：后端未就绪
  const [errMsg, setErrMsg] = useState('');
  const [keyword, setKeyword] = useState('');
  const [applied, setApplied] = useState('');
  /**
   * 只看某个模型的粘滞（对齐 A2A `RoutingStickyDialog` 的 `model` 入参）：
   * 有模型上下文时（从回放/模型配置进来）清除退化为「清这个模型的全部」，
   * 不会误清别的模型 —— A2A 的确认文案与按钮也随 model 变化。
   */
  const [modelScope, setModelScope] = useState('');
  /** 每个模型的粘滞条数（筛选下拉的候选，取自当前快照） */
  const modelsInScope = Array.from(new Set(entries.map((e) => splitStickyId(e.id).model).filter(Boolean))).sort();
  const visible = modelScope ? entries.filter((e) => splitStickyId(e.id).model === modelScope) : entries;
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const debounceRef = useRef<number | null>(null);

  /**
   * 活跃绑定数（对齐 A2A 的「{n} 条活跃绑定」）：`expires_at` 在未来的才计入，
   * 且**随模型作用域收窄**（A2A 弹窗带 model 时它的 `active` 也是该模型那一份）。
   */
  const nowMs = Date.now();
  const activeCount = (modelScope
    ? entries.filter((e) => splitStickyId(e.id).model === modelScope)
    : entries
  ).filter((e) => {
    const exp = toMillis(e.expires_at);
    return exp == null || exp > nowMs;
  }).length;

  const load = useCallback(async (q: string): Promise<void> => {
    setLoading(true);
    setErrMsg('');
    try {
      const res = await api.getRouteSticky(q);
      // 根因修复（CI e2e Object.entries(null) 崩溃）：旧桩把 /api/routing/sticky
      // 兜底成 {data: []}——data 是**数组**，data.entries 即 Array.prototype.entries
      // （native 方法，name='entries'），`?? []` 只拦 null/undefined 不拦函数 →
      // 函数被当 updater 派发进 useState 队列 → basicStateReducer 执行
      // fn(prev=null) → Object.entries(null) 崩。Array.isArray 守卫既挡住
      // 「数组身上的 entries 方法」，也挡住任何非数组形状；useReducer 防御
      // （d52f3a15）继续作为第二道保险保留。
      const raw = res?.data?.entries;
      setEntries(Array.isArray(raw) ? raw : []);
      setStickyMode(res?.data?.sticky ?? '');
      setTotal(typeof res?.data?.total === 'number' ? res.data.total : 0);
      setTruncated(res?.data?.truncated === true);
      setDegraded(false);
    } catch (err) {
      const e = err as Error & { status?: number };
      if (e.status === 404) {
        setDegraded(true);
        setEntries([]);
        setTotal(0);
        setTruncated(false);
      } else {
        setErrMsg(e.message || String(err));
      }
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load('');
  }, [load]);

  // 搜索去抖 300ms：输入停顿才发请求（大列表友好，不逐键打后端）
  const onKeyword = (v: string): void => {
    setKeyword(v);
    if (debounceRef.current !== null) window.clearTimeout(debounceRef.current);
    debounceRef.current = window.setTimeout(() => {
      debounceRef.current = null;
      const q = v.trim();
      setApplied(q);
      void load(q);
    }, 300);
  };

  // 逐条清除：id 是后端内部编码（session\u{1f}model），**原样回传**
  const clearOne = (entryId: string): void => {
    const { session, model } = splitStickyId(entryId);
    const label = model ? `${session} / ${model}` : session;
    setConfirmState({
      title: t('清除粘滞会话'),
      message:
        t('清除后该会话的下一条请求重新按策略选路。确认清除') +
        `「${label.length > 32 ? `${label.slice(0, 32)}…` : label}」？`,
      confirmText: t('清除'),
      danger: false,
      onConfirm: async () => {
        try {
          await api.clearRouteSticky({ id: entryId });
          addToast(t('已清除粘滞会话'));
          void load(applied);
        } catch (err) {
          addToast(err instanceof Error ? err.message : String(err), 'error');
        }
      },
    });
  };

  const clearAll = (): void => {
    // 有模型作用域时退化为「清这个模型的全部」（对齐 A2A：带 model 进弹窗时
    // 底部按钮是「解除当前模型全部粘滞」，文案也随它变）。不这样会把别的
    // 模型的粘滞一起清掉 —— 那正是「只想换这个模型试试」时最不想要的副作用。
    const scoped = modelScope;
    setConfirmState({
      title: scoped ? t('清除该模型的全部粘滞会话') : t('清除全部粘滞会话'),
      message: scoped
        ? t('「{{model}}」的下一条请求都会重新按策略选路（上游上下文缓存失效）。确认清除？', {
            model: scoped,
          })
        : t('全部会话的下一条请求都会重新按策略选路（上游上下文缓存失效）。确认清除全部？'),
      confirmText: scoped ? t('清除该模型全部') : t('全部清除'),
      danger: true,
      onConfirm: async () => {
        try {
          const res = await api.clearRouteSticky(scoped ? { model: scoped } : { all: true });
          addToast(t('已清除 {{n}} 条粘滞会话', { n: res?.data?.cleared ?? 0 }));
          void load(applied);
        } catch (err) {
          addToast(err instanceof Error ? err.message : String(err), 'error');
        }
      },
    });
  };

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('粘滞会话')}</h2>
        <p className="card-subtitle">
          {t('会话粘滞让同一会话跟住同一供应商（复用上游上下文缓存）。当前模式：{{mode}}；排障或想换供应商时在此清除。', {
            mode: stickyMode || t('未记录'),
          })}
        </p>
      </div>
      <div className="card-body">
        {/* 状态摘要（对齐 A2A RoutingStickyDialog：粘性档位徽标 + 活跃绑定条数）。
            AIGX 的条目按 expires_at 判活跃；count 只用于展示，不参与清除判据。 */}
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap', marginBottom: 10 }}>
          <Badge tone="info">{t('粘性：{{mode}}', { mode: stickyMode || t('未记录') })}</Badge>
          <span className="form-hint" style={{ margin: 0 }}>
            {t('{{n}} 条活跃绑定', { n: activeCount })}
          </span>
        </div>
        <div className="form-group">
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <div style={{ position: 'relative', flex: 1, maxWidth: 360 }}>
              <Search size={14} style={{ position: 'absolute', left: 10, top: 9, opacity: 0.5 }} />
              <input
                className="form-input"
                style={{ paddingLeft: 30 }}
                placeholder={t('搜索模型、账号或会话来源')}
                value={keyword}
                onChange={(e) => onKeyword(e.target.value)}
                aria-label={t('搜索模型、账号或会话来源')}
              />
            </div>
            {/* 模型作用域（对齐 A2A 弹窗的 model 入参）：候选取自当前快照里
                真的出现过的模型 —— 空清单时整块不渲染，不留一个没得选的空下拉。 */}
            {modelsInScope.length > 0 ? (
              <select
                className="form-input"
                style={{ maxWidth: 220 }}
                value={modelScope}
                aria-label={t('粘滞模型')}
                onChange={(e) => setModelScope(e.target.value)}
              >
                <option value="">{t('全部模型')}</option>
                {modelsInScope.map((m) => (
                  <option key={m} value={m}>{m}</option>
                ))}
              </select>
            ) : null}
            <button
              className="btn btn-secondary"
              onClick={() => void load(applied)}
              disabled={loading}
              title={t('刷新')}
            >
              <RefreshCw size={14} />
            </button>
            <button
              className="btn btn-secondary"
              onClick={clearAll}
              disabled={loading || degraded || (modelScope ? (visible.length === 0) : (entries.length === 0))}
            >
              {modelScope ? t('清除该模型全部') : t('全部清除')}
            </button>
          </div>
        </div>

        {loading ? (
          <Loading text={t('加载粘滞会话')} />
        ) : degraded ? (
          <EmptyState
            message={t('粘滞会话管理端点尚未就绪（后端建设中）。就绪后这里列出当前粘住的会话，支持搜索与逐条/全部清除。')}
          />
        ) : errMsg ? (
          <div className="error-message">{errMsg}</div>
        ) : visible.length === 0 ? (
          <EmptyState
            message={
              modelScope
                ? t('该模型当前没有粘滞会话')
                : applied
                  ? t('没有匹配的粘滞会话')
                  : t('当前没有粘滞会话')
            }
          />
        ) : (
          <div className="rs-list">
            {visible.map((e) => {
              const exp = toMillis(e.expires_at);
              const { session, model } = splitStickyId(e.id);
              // G1「最近使用」：上游用 entry.lastSeen；AIGX 后端未单列 last_seen 时
              // 回落 updated_at（粘滞条目在最后成功请求时写入，updated_at≈最近使用）。
              const lastUsed = timeText(e.last_seen ?? e.updated_at);
              // 剩余时长：优先后端 hard_remaining_secs，否则用 expires_at - now 推算。
              const remainMs =
                e.hard_remaining_secs != null
                  ? e.hard_remaining_secs * 1000
                  : exp != null
                    ? exp - nowMs
                    : null;
              return (
                <div key={e.id} className="rs-item">
                  {/* title 必须存**原样内部编码 id**（与清除时 POST body 数据同源）：
                      e2e 单条清除用例从 title 读完整 id 做回传断言；人读格式
                      （session / model）只放行内可见文本，不进 title。 */}
                  <code className="rs-id" title={e.id}>
                    {session.length > 24 ? `${session.slice(0, 24)}…` : session}
                  </code>
                  {model ? (
                    <span className="rs-model" style={{ opacity: 0.8 }}>
                      {model}
                    </span>
                  ) : null}
                  <span className="rs-chan">
                    {e.channel_name || (e.channel_id ? `#${e.channel_id}` : '—')}
                  </span>
                  {/* ── G1：会话来源 / 键预览 / 命中次数（对齐上游 sticky 行字段集） ──
                      后端当前不返回 source/key_preview/hits，故命中这些分支会落到
                      「后端未记录」标注，不编造数值。后端补齐后这些分支自动生效。 */}
                  <Badge tone="neutral">{stickySourceLabel(e.source)}</Badge>
                  {e.key_preview ? (
                    <code className="rs-key" title={e.key_preview}>
                      {e.key_preview}
                    </code>
                  ) : null}
                  {typeof e.hits === 'number' ? (
                    <span className="rs-hits">{t('命中 {{n}} 次', { n: e.hits })}</span>
                  ) : (
                    <span className="rs-hints" title={t('后端待补：命中次数维度')}>
                      {t('命中次数：后端未记录')}
                    </span>
                  )}
                  {lastUsed ? (
                    <span className="rs-lastused">
                      {t('最近使用：{{time}}', { time: lastUsed })}
                    </span>
                  ) : null}
                  {exp != null && (
                    <span className="rs-exp">
                      {t('到期 {{time}}', { time: new Date(exp).toLocaleTimeString() })}
                    </span>
                  )}
                  {remainMs != null ? (
                    <span className="rs-remain" title={t('距硬过期剩余')}>
                      {formatDuration(remainMs)}
                    </span>
                  ) : null}
                  <button
                    type="button"
                    className="btn btn-secondary btn-sm rs-clear"
                    onClick={() => clearOne(e.id)}
                  >
                    {t('清除')}
                  </button>
                </div>
              );
            })}
          </div>
        )}

        {!loading && !degraded && !errMsg && visible.length > 0 && (
          <div className="rs-total" style={{ opacity: 0.7, fontSize: 12, marginTop: 10 }}>
            {/* 后端按 500 条截断（无分页）：超出必须显式告知，否则管理员
                会把「列出的一页」误当成全部。有模型作用域时同时给出筛选口径，
                否则「总数」与「行数」对不上会被读成丢数据。 */}
            {`${t('总数')} ${total}`}
            {truncated ? ` · ${t('（已截断）')}` : ''}
            {modelScope ? ` · ${t('当前只显示 {{model}}', { model: modelScope })}` : ''}
          </div>
        )}
      </div>
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
