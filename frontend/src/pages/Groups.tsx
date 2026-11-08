import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Loading, EmptyState } from '../components/ui';
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
import './Groups.css';

interface GroupItem {
  name: string;
  ratio?: number;
  allowed_models?: string[] | string;
  description?: string;
}

interface GroupForm {
  name: string;
  ratio: string;
  allowed_models: string;
  description: string;
}

/** 弹窗内的表单错误（区别于页面级 error，直接渲染在表单顶部，不被遮罩挡住） */
type ModalError = string;

/** 计费实际读取的分组倍率表（RatioConfig.group_ratio） */
type GroupRatioMap = Record<string, number>;


const LS_TCOLS = 'aigx-tbl-cols.groups';
/** 列声明 */
const GRP_COL_DEFS: TableColumnDef[] = [
  { key: 'name', label: '分组名称', width: 160, locked: true },
  { key: 'ratio', label: '倍率', align: 'right', width: 100 },
  { key: 'models', label: '允许模型', width: 220 },
  { key: 'desc', label: '描述', width: 260 },
  { key: 'act', label: '操作', width: 140, locked: true },
];

export default function Groups(): JSX.Element {
  // 列设置已迁公共层（tableColumns v2）
  const [colState, setColState] = useState<TableColumnState>(() => readTableColumns(LS_TCOLS, GRP_COL_DEFS));
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);
  const commitCols = useCallback((next: TableColumnItem[], persist: boolean) => {
    setColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);
  const resetCols = useCallback(() => setColState(resetTableColumns(LS_TCOLS, GRP_COL_DEFS)), []);
  const appliedCols = useMemo(() => applyTableColumns(colState, GRP_COL_DEFS), [colState]);
  const visibleKeySet = useMemo(() => new Set(appliedCols.map((c) => c.key)), [appliedCols]);
  const colResize = useColumnResize({
    widths: Object.fromEntries(colState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number])),
    setWidths: (next) => {
      commitCols(
        colState.items.map((i) => {
          const w = next[i.key];
          const defW = GRP_COL_DEFS.find((d) => d.key === i.key)?.width;
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(GRP_COL_DEFS.map((d) => [d.key, d.width])),
    lockedKeys: new Set(GRP_COL_DEFS.filter((d) => d.locked).map((d) => d.key)),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });
  const { t } = useTranslation();
  const addToast = useToast();
  const [groups, setGroups] = useState<GroupItem[]>([]);
  const [groupRatios, setGroupRatios] = useState<GroupRatioMap>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<GroupItem | null>(null);
  const [form, setForm] = useState<GroupForm>({ name: '', ratio: '1', allowed_models: '', description: '' });
  const [saving, setSaving] = useState(false);
  // 弹窗内错误：渲染在表单顶部，避免页面顶部错误被遮罩挡住看不见
  const [modalError, setModalError] = useState<ModalError>('');

  useEffect(() => {
    void loadGroups();
  }, []);

  const loadGroups = async () => {
    setLoading(true);
    setError('');
    try {
      const [groupRes, ratioRes] = await Promise.all([
        api.listGroups(),
        api.getRatios().catch(() => null),
      ]);
      setGroups(groupRes?.data ?? []);
      const ratios = (ratioRes?.data ?? {}) as { group_ratio?: GroupRatioMap };
      setGroupRatios(ratios.group_ratio ?? {});
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const openAdd = () => {
    setEditing(null);
    setForm({ name: '', ratio: '1', allowed_models: '', description: '' });
    setModalError('');
    setShowModal(true);
  };

  const openEdit = (g: GroupItem) => {
    setEditing(g);
    setModalError('');
    setForm({
      name: g.name || '',
      // 展示计费口径的倍率（RatioConfig），不回退到分组条目上那份不生效的旧值
      ratio: String(groupRatios[g.name] != null ? groupRatios[g.name] : 1),
      allowed_models: Array.isArray(g.allowed_models)
        ? g.allowed_models.join(', ')
        : (g.allowed_models || ''),
      description: g.description || '',
    });
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditing(null);
  };

  /** 把分组倍率写进计费实际读取的 RatioConfig（分组条目上的 ratio 字段仅为兼容保留） */
  const syncGroupRatio = async (name: string, ratio: number | null) => {
    const res = await api.getRatios();
    const current = (res?.data ?? {}) as {
      model_ratio?: Record<string, number>;
      group_ratio?: GroupRatioMap;
    };
    const nextGroup = { ...(current.group_ratio ?? {}) };
    if (ratio == null || ratio === 1) delete nextGroup[name];
    else nextGroup[name] = ratio;
    await api.updateRatios({
      model_ratio: current.model_ratio ?? {},
      group_ratio: nextGroup,
    });
  };

  // payload 与后端 GroupRequest 对齐
  const handleSave = async () => {
    if (!form.name.trim()) {
      setModalError(t('分组名称为必填项'));
      return;
    }
    setSaving(true);
    setModalError('');
    try {
      const allowedModels = form.allowed_models
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      const payload = {
        name: form.name.trim(),
        ratio: Number(form.ratio) || 1,
        allowed_models: allowedModels,
        description: form.description || '',
      };
      await api.upsertGroup(payload);
      await syncGroupRatio(form.name.trim(), Number(form.ratio) || 1);
      addToast(t('用户分组已保存'));
      closeModal();
      void loadGroups();
    } catch (err) {
      setModalError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = (name: string) => {
    if (name === 'default') {
      setError(t('不能删除默认分组'));
      return;
    }
    setConfirmState({
      title: t('删除分组'),
      message: `${t('确定删除分组')} ${name}?`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteGroup(name);
          // 连带清掉计费倍率，避免留下孤儿倍率作用到同名新分组
          await syncGroupRatio(name, null).catch(() => { /* 清理失败不阻塞删除 */ });
          addToast(t('用户分组已删除'));
          void loadGroups();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  return (
    <div className="groups-shell">
      {/* 嵌入 Settings billing 子标签使用，无独立路由；标题由外层 Tabs 提供 */}
      {error && <div className="error-message">{error}</div>}

      <div className="groups-content">
        {loading ? (
          <Loading text={t('加载用户分组')} />
        ) : (
          <Card
            title={`${t('所有分组')} (${groups.length})`}
            actions={
              <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
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
                      defs={GRP_COL_DEFS}
                      items={colState.items}
                      onChange={commitCols}
                      onReset={resetCols}
                      anchorRef={colPanelRef}
                    />
                  )}
                </div>
                <Button onClick={openAdd}>{t('+ 新建分组')}</Button>
              </div>
            }
          >
            {groups.length === 0 ? (
              <EmptyState
                message={t('暂无自定义分组，使用默认分组 (倍率 1.0)')}
                action={<Button onClick={openAdd}>{t('新建分组')}</Button>}
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
                          <th
                            key={c.key}
                            style={c.width != null ? { width: `${c.width}px` } : undefined}
                          >
                            {t(GRP_COL_DEFS.find((d) => d.key === c.key)?.label ?? c.key)}
                            {c.key !== 'act' && (
                              <span className="as-col-grip" title={t('拖动调整列宽 · 双击还原')} {...grip} />
                            )}
                          </th>
                        );
                      })}
                    </tr>
                  </thead>
                  <tbody>
                    {groups.map((g) => (
                      <tr key={g.name}>
{visibleKeySet.has('name') && (
                        <td>
                          <strong>{g.name}</strong>
                          {g.name === 'default' && (
                            <span className="default-tag">{t('默认')}</span>
                          )}
                        </td>
)}
{visibleKeySet.has('ratio') && (
                        <td>
                          <span
                            className="ratio-badge"
                            data-high={(groupRatios[g.name] ?? 1) > 1}
                            title={t('计费实际生效的分组倍率（与「定价倍率」页同一份配置）')}
                          >
                            ×{groupRatios[g.name] != null ? groupRatios[g.name] : 1}
                          </span>
                        </td>
)}
{visibleKeySet.has('models') && (
                        <td
                          style={{ fontSize: 12, color: 'var(--text-muted)', maxWidth: 300, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                          title={Array.isArray(g.allowed_models) && g.allowed_models.length > 0 ? g.allowed_models.join(', ') : t('全部')}
                        >
                          {Array.isArray(g.allowed_models) && g.allowed_models.length > 0
                            ? g.allowed_models.join(', ')
                            : t('全部')}
                        </td>
)}
{visibleKeySet.has('desc') && (
                        <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                          {g.description || '—'}
                        </td>
)}
                        {visibleKeySet.has('act') && (
                        <td>
                          <div className="actions-cell">
                            <button className="btn btn-outline btn-sm" onClick={() => openEdit(g)}>
                              {t('编辑')}
                            </button>
                            {g.name !== 'default' && (
                              <button className="btn btn-danger btn-sm" onClick={() => handleDelete(g.name)}>
                                {t('删除')}
                              </button>
                            )}
                          </div>
                        </td>
                      )}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Card>
        )}
      </div>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

      {showModal && (
        <div className="modal-overlay">
          <div className="modal">
            <div className="modal-header">
              <h3>{editing ? t('编辑分组') : t('新建分组')}</h3>
              <button className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              {modalError && <div className="error-message">{modalError}</div>}
              <div className="form-group">
                <label>{t('分组名称')} *</label>
                <input
                  className="form-input"
                  placeholder={t('例如：vip')}
                  value={form.name}
                  disabled={!!(editing && editing.name === 'default')}
                  onChange={(e) => setForm({ ...form, name: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('计费倍率')}</label>
                <input
                  className="form-input"
                  type="number"
                  step="0.01"
                  min="0"
                  placeholder="1.0"
                  value={form.ratio}
                  onChange={(e) => setForm({ ...form, ratio: e.target.value })}
                />
                <span className="form-hint">{t('最终费用 = 基础费用 × 模型倍率 × 分组倍率')}</span>
              </div>
              <div className="form-group">
                <label>{t('允许模型')}</label>
                <input
                  className="form-input"
                  placeholder={t('(逗号分隔，留空则允许全部)')}
                  value={form.allowed_models}
                  onChange={(e) => setForm({ ...form, allowed_models: e.target.value })}
                />
              </div>
              <div className="form-group">
                <label>{t('分组描述')}</label>
                <input
                  className="form-input"
                  placeholder={t('分组描述')}
                  value={form.description}
                  onChange={(e) => setForm({ ...form, description: e.target.value })}
                />
              </div>
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={closeModal} disabled={saving}>{t('取消')}</button>
              <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
                {saving ? t('保存中...') : t('保存')}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
