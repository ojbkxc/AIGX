/**
 * RouteSticky — 粘滞会话管理（对照 Agent2API「路由 → 粘性会话」routing-sticky.tsx +
 * api/routing_workspace.rs get_sticky/clear_sticky）。
 *
 * 列出当前粘住的会话（标识/渠道/过期时刻）、按标识或渠道名搜索、逐条清除、
 * 全部清除（二次确认）。粘滞让同会话跟住同一渠道以复用上游上下文缓存；
 * 排障时「换渠道试试」就用这里清掉粘滞。
 *
 * 数据源：GET /api/routing/sticky?query= / POST /api/routing/sticky/clear
 * （be-fix 后续任务提供；未就绪 404 → 空态降级，不报错不崩）。
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
      setDegraded(false);
    } catch (err) {
      const e = err as Error & { status?: number };
      if (e.status === 404) {
        setDegraded(true);
        setEntries([]);
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

  const clearOne = (id: string): void => {
    setConfirmState({
      title: t('清除粘滞会话'),
      message: t('清除后该会话的下一条请求重新按策略选路。确认清除') + `「${id.slice(0, 16)}…」？`,
      confirmText: t('清除'),
      danger: false,
      onConfirm: async () => {
        try {
          await api.clearRouteSticky({ id });
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
              return (
                <div key={e.id} className="rs-item">
                  <code className="rs-id" title={e.id}>{e.id.length > 24 ? `${e.id.slice(0, 24)}…` : e.id}</code>
                  <span className="rs-chan">
                    {e.channel_name || (e.channel_id != null ? `#${e.channel_id}` : '—')}
                    {e.provider ? ` · ${e.provider}` : ''}
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
      </div>
      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
