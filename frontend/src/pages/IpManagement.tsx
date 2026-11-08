import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { EmptyState, SkeletonTable } from '../components/ui';
import {
  applyTableColumns,
  readTableColumns,
  resetTableColumns,
  writeTableColumns,
  type TableColumnDef,
  type TableColumnItem,
  type TableColumnState,
} from '../lib/tableColumns';
import TableColumnSettings from '../components/ui/TableColumnSettings';
import { useColumnResize } from '../components/ui/useColumnResize';
import { Tooltip } from '../components/ui';
import { Columns3 } from 'lucide-react';
import './IpManagement.css';

type TabKey = 'whitelist' | 'blacklist';
type IpRule = string | { pattern?: string; ip?: string; note?: string; remark?: string };

interface IpFilterResponse {
  enabled?: boolean;
  whitelist?: IpRule[];
  blacklist?: IpRule[];
  data?: {
    enabled?: boolean;
    whitelist?: IpRule[];
    blacklist?: IpRule[];
  };
}

// IpManagement 页面：IP 白名单/黑名单管理。
// 参照 Groups.jsx / Keys.jsx 的 CRUD 界面模式。
// 规则支持单 IP 或 CIDR 表示法（如 192.168.0.0/24）。

const LS_TCOLS = 'aigx-tbl-cols.ip';
/** 列声明 */
const IP_COL_DEFS: TableColumnDef[] = [
  { key: 'cidr', label: 'IP / CIDR', width: 220, locked: true },
  { key: 'remark', label: '备注', width: 240 },
  { key: 'act', label: '操作', width: 120, locked: true },
];
export default function IpManagement() {
  // 列设置已迁公共层（tableColumns v2）
  const [colState, setColState] = useState<TableColumnState>(() => readTableColumns(LS_TCOLS, IP_COL_DEFS));
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);
  const commitCols = useCallback((next: TableColumnItem[], persist: boolean) => {
    setColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);
  const resetCols = useCallback(() => setColState(resetTableColumns(LS_TCOLS, IP_COL_DEFS)), []);
  const appliedCols = useMemo(() => applyTableColumns(colState, IP_COL_DEFS), [colState]);
  const visibleKeySet = useMemo(() => new Set(appliedCols.map((c) => c.key)), [appliedCols]);
  const colResize = useColumnResize({
    widths: Object.fromEntries(colState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number])),
    setWidths: (next) => {
      commitCols(
        colState.items.map((i) => {
          const w = next[i.key];
          const defW = IP_COL_DEFS.find((d) => d.key === i.key)?.width;
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(IP_COL_DEFS.map((d) => [d.key, d.width])),
    lockedKeys: new Set(IP_COL_DEFS.filter((d) => d.locked).map((d) => d.key)),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });
  const { t } = useTranslation();
  const addToast = useToast();

  // 全局开关与规则列表
  const [enabled, setEnabled] = useState(false);
  const [whitelist, setWhitelist] = useState<IpRule[]>([]);
  const [blacklist, setBlacklist] = useState<IpRule[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [savingSwitch, setSavingSwitch] = useState(false);

  // 添加规则表单（白名单 / 黑名单共用一套状态，由 activeTab 区分）
  const [activeTab, setActiveTab] = useState<TabKey>('whitelist');
  const [pattern, setPattern] = useState('');
  const [note, setNote] = useState('');
  const [adding, setAdding] = useState(false);

  // 确认弹窗
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  useEffect(() => {
    loadFilter();
  }, []);

  // 加载 IP 过滤配置
  const loadFilter = async () => {
    setLoading(true);
    setError('');
    try {
      const res = (await api.getIpFilter()) as IpFilterResponse;
      const data = res?.data || res || {};
      setEnabled(!!data.enabled);
      setWhitelist(Array.isArray(data.whitelist) ? data.whitelist : []);
      setBlacklist(Array.isArray(data.blacklist) ? data.blacklist : []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  // 切换全局开关
  const handleToggleEnabled = async () => {
    setSavingSwitch(true);
    setError('');
    try {
      const next = !enabled;
      await api.updateIpFilter({ enabled: next });
      setEnabled(next);
      addToast(next ? t('IP 过滤已启用') : t('IP 过滤已禁用'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingSwitch(false);
    }
  };

  // 简单校验 IP / CIDR 格式（宽松校验，允许 IPv4、IPv6、CIDR）
  const isValidPattern = (p: string) => {
    if (!p) return false;
    // IPv4 / IPv4 CIDR：正则 + 每段 0-255 + 掩码 ≤32
    const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(\/(\d{1,2}))?$/.exec(p);
    if (m) {
      const octets = [m[1], m[2], m[3], m[4]].map((s) => parseInt(s, 10));
      if (octets.some((n) => Number.isNaN(n) || n < 0 || n > 255)) return false;
      if (m[5] != null) {
        const mask = parseInt(m[6], 10);
        if (Number.isNaN(mask) || mask > 32) return false;
      }
      return true;
    }
    // 含冒号视为 IPv6 / IPv6 CIDR，宽松放行
    if (p.includes(':')) return true;
    return false;
  };

  // 添加规则
  const handleAdd = async (): Promise<void> => {
    const p = pattern.trim();
    if (!p) {
      setError(t('请输入 IP 或 CIDR'));
      return;
    }
    if (!isValidPattern(p)) {
      setError(t('格式不正确，请输入合法 IP 或 CIDR（如 192.168.0.0/24）'));
      return;
    }
    setAdding(true);
    setError('');
    try {
      if (activeTab === 'whitelist') {
        await api.addWhitelist(p, note.trim());
        addToast(t('白名单规则已添加'));
      } else {
        await api.addBlacklist(p, note.trim());
        addToast(t('黑名单规则已添加'));
      }
      setPattern('');
      setNote('');
      await loadFilter();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setAdding(false);
    }
  };

  // 删除规则
  const handleRemove = (item: IpRule) => {
    const p = typeof item === 'string' ? item : (item.pattern || item.ip || '');
    const isWhite = activeTab === 'whitelist';
    setConfirmState({
      title: t('删除规则'),
      message: `${t('确定删除规则')} ${p}?`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          if (isWhite) {
            await api.removeWhitelist(p);
            addToast(t('白名单规则已删除'));
          } else {
            await api.removeBlacklist(p);
            addToast(t('黑名单规则已删除'));
          }
          loadFilter();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  // 渲染规则项：兼容字符串数组与对象数组两种后端返回格式
  const renderPattern = (item: IpRule) => typeof item === 'string' ? item : (item.pattern || item.ip || '');
  const renderNote = (item: IpRule) => typeof item === 'string' ? '' : (item.note || item.remark || '');

  const currentList = activeTab === 'whitelist' ? whitelist : blacklist;

  return (
    <div className="ipm-shell">
      {error && <div className="error-message">{error}</div>}

      {/* 全局开关 */}
      <div className="card">
        <div className="card-body ipm-switch-row">
          <div className="ipm-switch-info">
            <div className="ipm-switch-title">{t('IP 过滤总开关')}</div>
            <div className="ipm-switch-hint">{t('开启后白名单/黑名单规则才会生效')}</div>
          </div>
          <label className="ipm-toggle">
            <input
              type="checkbox"
              checked={enabled}
              onChange={handleToggleEnabled}
              disabled={savingSwitch || loading}
            />
            <span className="ipm-toggle-slider"></span>
          </label>
        </div>
      </div>

      {/* 白名单 / 黑名单 Tab */}
      <div className="card">
        <div className="card-body">
          <div className="ipm-tabs">
            <button
              className={`btn ${activeTab === 'whitelist' ? 'btn-primary' : 'btn-outline'}`}
              onClick={() => { setActiveTab('whitelist'); setPattern(''); setNote(''); setError(''); }}
            >
              {t('白名单')} ({whitelist.length})
            </button>
            <button
              className={`btn ${activeTab === 'blacklist' ? 'btn-primary' : 'btn-outline'}`}
              onClick={() => { setActiveTab('blacklist'); setPattern(''); setNote(''); setError(''); }}
            >
              {t('黑名单')} ({blacklist.length})
            </button>
            {/* 列设置（公共层；白/黑名单共用一份列集） */}
            <div className="as-colset" ref={colPanelRef} style={{ position: 'relative', display: 'inline-block' }}>
              <Tooltip label={t('列设置（显隐 / 顺序 / 对齐 / 列宽拖动）')}>
                <button
                  type="button"
                  className={`btn btn-outline btn-sm${colPanelOpen ? ' as-colset-open' : ''}`}
                  onClick={() => setColPanelOpen((v) => !v)}
                  aria-label={t('列设置')}
                >
                  <Columns3 size={14} />
                </button>
              </Tooltip>
              {colPanelOpen && (
                <TableColumnSettings
                  open
                  onClose={() => setColPanelOpen(false)}
                  defs={IP_COL_DEFS}
                  items={colState.items}
                  onChange={commitCols}
                  onReset={resetCols}
                  anchorRef={colPanelRef}
                />
              )}
            </div>
          </div>

          {/* 添加规则表单 */}
          <form
            className="ipm-add-form"
            onSubmit={(e) => { e.preventDefault(); void handleAdd(); }}
          >
            <div className="form-group">
              <label>{t('IP / CIDR')} *</label>
              <input
                className="form-input"
                placeholder={t('例如：192.168.0.0/24 或 10.0.0.1')}
                value={pattern}
                onChange={(e) => setPattern(e.target.value)}
                disabled={loading || adding}
              />
            </div>
            <div className="form-group">
              <label>{t('备注')}</label>
              <input
                className="form-input"
                placeholder={t('可选备注，如：办公网段')}
                value={note}
                onChange={(e) => setNote(e.target.value)}
                disabled={loading || adding}
              />
            </div>
            <div className="ipm-add-action">
              <button
                type="submit"
                className="btn btn-primary"
                disabled={adding || loading || !pattern.trim()}
              >
                {adding ? t('添加中...') : t('添加规则')}
              </button>
            </div>
          </form>

          {/* 规则列表 */}
          {loading ? (
            <SkeletonTable columns={3} rows={4} />
          ) : currentList.length === 0 ? (
            <EmptyState
              message={activeTab === 'whitelist' ? t('暂无白名单规则') : t('暂无黑名单规则')}
              icon={activeTab === 'whitelist' ? '✅' : '🚫'}
            />
          ) : (
            <div className="table-wrapper">
              <table>
                <colgroup>
                  {appliedCols.map((c) => (
                    <col key={c.key} data-col-key={c.key} style={c.width != null ? { width: `${c.width}px` } : undefined} />
                  ))}
                </colgroup>
                <thead>
                  <tr>
                    {appliedCols.map((c, idx) => {
                      const grip = colResize.gripProps(c.key, {
                        isLast: idx === appliedCols.length - 1,
                        locked: c.locked,
                        defaultWidth: c.width,
                      });
                      return (
                        <th key={c.key} style={c.width != null ? { width: `${c.width}px` } : undefined}>
                          {t(IP_COL_DEFS.find((d) => d.key === c.key)?.label ?? c.key)}
                          {c.key !== 'cidr' && c.key !== 'act' && (
                            <span className="as-col-grip" title={t('拖动调整列宽 · 双击还原')} {...grip} />
                          )}
                        </th>
                      );
                    })}
                  </tr>
                </thead>
                <tbody>
                  {currentList.map((item, i) => {
                    const p = renderPattern(item);
                    const n = renderNote(item);
                    return (
                      <tr key={p || i}>
                        {visibleKeySet.has('cidr') && (
                        <td><code style={{ background: 'var(--card-bg)', padding: '2px 6px', borderRadius: 4 }}>{p}</code></td>
                        )}
                        {visibleKeySet.has('remark') && (
                        <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>{n || '—'}</td>
                        )}
                        {visibleKeySet.has('act') && (
                        <td>
                          <div className="actions-cell">
                            <button className="btn btn-danger btn-sm" onClick={() => handleRemove(item)}>
                              {t('删除')}
                            </button>
                          </div>
                        </td>
                        )}
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
