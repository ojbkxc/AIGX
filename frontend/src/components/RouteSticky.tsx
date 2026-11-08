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
import { EmptyState, Loading } from './ui';
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
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const debounceRef = useRef<number | null>(null);

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
    setConfirmState({
      title: t('清除全部粘滞会话'),
      message: t('全部会话的下一条请求都会重新按策略选路（上游上下文缓存失效）。确认清除全部？'),
      confirmText: t('全部清除'),
      danger: true,
      onConfirm: async () => {
        try {
          const res = await api.clearRouteSticky({ all: true });
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
          {t('会话粘滞让同一会话跟住同一渠道（复用上游上下文缓存）。当前模式：{{mode}}；排障或想换渠道时在此清除。', {
            mode: stickyMode || t('未记录'),
          })}
        </p>
      </div>
      <div className="card-body">
        <div className="form-group">
          <div style={{ display: 'flex', gap: 8 }}>
            <div style={{ position: 'relative', flex: 1, maxWidth: 360 }}>
              <Search size={14} style={{ position: 'absolute', left: 10, top: 9, opacity: 0.5 }} />
              <input
                className="form-input"
                style={{ paddingLeft: 30 }}
                placeholder={t('按会话标识或渠道名搜索')}
                value={keyword}
                onChange={(e) => onKeyword(e.target.value)}
              />
            </div>
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
              disabled={loading || degraded || entries.length === 0}
            >
              {t('全部清除')}
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
        ) : entries.length === 0 ? (
          <EmptyState
            message={applied ? t('没有匹配的粘滞会话') : t('当前没有粘滞会话')}
          />
        ) : (
          <div className="rs-list">
            {entries.map((e) => {
              const exp = toMillis(e.expires_at);
              const { session, model } = splitStickyId(e.id);
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
                  {exp != null && (
                    <span className="rs-exp">
                      {t('到期 {{time}}', { time: new Date(exp).toLocaleTimeString() })}
                    </span>
                  )}
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

        {!loading && !degraded && !errMsg && entries.length > 0 && (
          <div className="rs-total" style={{ opacity: 0.7, fontSize: 12, marginTop: 10 }}>
            {/* 后端按 500 条截断（无分页）：超出必须显式告知，否则管理员
                会把「列出的一页」误当成全部 */}
            {`${t('总数')} ${total}`}
            {truncated ? ` · ${t('（已截断）')}` : ''}
          </div>
        )}
      </div>
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
