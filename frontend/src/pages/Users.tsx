import { useState, useEffect, useRef, useCallback, useMemo, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Pencil, Power, PowerOff, MoreHorizontal, KeyRound, Trash2, Columns3 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
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
import { TablePaginationFooter, useServerPaging, SERVER_PAGE_SIZES } from '../components/ui/TableShell';
import { Tooltip } from '../components/ui';
import { Button, Card, Input, EmptyState, Select, SkeletonTable } from '../components/ui';
import { fmtQuota } from '../lib/utils';
import './Users.css';

interface UserItem {
  id: string | number;
  email: string;
  username?: string;
  role?: string;
  group?: string;
  quota?: number;
  used_quota?: number;
  status?: string;
  created_at?: number;
  /** 管理员备注 */
  remark?: string;
}

interface GroupItem {
  name?: string;
}

interface UserFormState {
  email: string;
  username: string;
  password: string;
  role: string;
  quota: string;
  status: string;
  group: string;
  remark: string;
}

const EMPTY_FORM: UserFormState = {
  email: '',
  username: '',
  password: '',
  role: 'user',
  quota: '0',
  status: 'active',
  group: 'default',
  remark: '',
};

// 公共键与列声明（模块级；[LS] 行操作菜单注释位置原属组件内）
const LS_PSZ = 'aigx-tbl-pagesize.users';
const LS_TCOLS = 'aigx-tbl-cols.users';
/** 用户表列声明（email/操作锁定；配额三列右对齐） */
const USER_COL_DEFS: TableColumnDef[] = [
  { key: 'email', label: '邮箱', width: 220, locked: true },
  { key: 'username', label: '昵称', width: 140 },
  { key: 'remark', label: '备注', width: 140 },
  { key: 'role', label: '角色', align: 'center', width: 90 },
  { key: 'group', label: '分组', width: 100 },
  { key: 'quota', label: '总配额', align: 'right', width: 110 },
  { key: 'used', label: '已用', align: 'right', width: 100 },
  { key: 'remain', label: '剩余配额', align: 'right', width: 110 },
  { key: 'status', label: '状态', align: 'center', width: 80 },
  { key: 'created', label: '创建时间', width: 160 },
  { key: 'act', label: '操作', width: 120, locked: true },
];

export default function Users(): JSX.Element {
  const [users, setUsers] = useState<UserItem[]>([]);
  const [groups, setGroups] = useState<GroupItem[]>([]);
  const [me, setMe] = useState<UserItem | null>(null);
  const [loading, setLoading] = useState(true);
  // 仅首次加载显示整页骨架。搜索/翻页时若也整页早退，工具栏（含搜索框）
  // 会被卸载，用户输入到一半焦点就丢了 —— 改为首屏后只做局部 loading。
  const firstLoadRef = useRef(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<UserItem | null>(null);
  const [form, setForm] = useState<UserFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  // 搜索关键字（邮箱/昵称/备注，走后端 keyword 过滤以覆盖全量数据）
  const [query, setQuery] = useState('');
  const [keyword, setKeyword] = useState('');
  // 分页（服务端真分页）：公共 hook 统一承担越界钳位 / size 变更回第 1 页 /
  // 关键词变化回第 1 页；page/size 发后端，total 由响应回带。
  const { page, setPage, pageSize, size, setSize, total, setTotal, totalPages } = useServerPaging({
    key: LS_PSZ,
    sizes: SERVER_PAGE_SIZES,
    fallback: 20,
    resetDeps: [keyword],
  });

  // 列设置已迁公共层（tableColumns v2）
  const [colState, setColState] = useState<TableColumnState>(() => readTableColumns(LS_TCOLS, USER_COL_DEFS));
  const [colPanelOpen, setColPanelOpen] = useState(false);
  const colPanelRef = useRef<HTMLDivElement | null>(null);
  const commitCols = useCallback((next: TableColumnItem[], persist: boolean) => {
    setColState((prev) => {
      const nextState: TableColumnState = { ...prev, items: next };
      if (persist) writeTableColumns(LS_TCOLS, nextState);
      return nextState;
    });
  }, []);
  const resetCols = useCallback(() => setColState(resetTableColumns(LS_TCOLS, USER_COL_DEFS)), []);
  const appliedCols = useMemo(() => applyTableColumns(colState, USER_COL_DEFS), [colState]);
  const visibleKeySet = useMemo(() => new Set(appliedCols.map((c) => c.key)), [appliedCols]);
  const colAlignMap = useMemo(() => {
    const map = {} as Record<string, string>;
    for (const item of colState.items) map[item.key] = item.align;
    return map;
  }, [colState]);
  const colResize = useColumnResize({
    widths: Object.fromEntries(colState.items.filter((i) => i.width != null).map((i) => [i.key, i.width as number])),
    setWidths: (next) => {
      commitCols(
        colState.items.map((i) => {
          const w = next[i.key];
          const defW = USER_COL_DEFS.find((d) => d.key === i.key)?.width;
          if (w == null || (defW != null && w === defW)) {
            const { width: _drop, ...rest } = i;
            return rest;
          }
          return { ...i, width: w };
        }),
        true,
      );
    },
    defaultWidths: Object.fromEntries(USER_COL_DEFS.map((d) => [d.key, d.width])),
    lockedKeys: new Set(USER_COL_DEFS.filter((d) => d.locked).map((d) => d.key)),
    onCommit: () => { /* 落盘已在 setWidths 内做 */ },
  });

  // 行操作菜单（fixed 挂在 body 层级，逃出 Card/table 的 overflow 裁剪；同渠道/密钥页）
  const [rowMenuId, setRowMenuId] = useState<string | number | null>(null);
  const [rowMenuPos, setRowMenuPos] = useState<{ top: number; left: number }>({ top: 0, left: 0 });
  useEffect(() => {
    if (rowMenuId === null) return;
    const handler = (): void => setRowMenuId(null);
    document.addEventListener('mousedown', handler);
    const scrollHandler = (): void => setRowMenuId(null);
    window.addEventListener('scroll', scrollHandler, true);
    return () => {
      document.removeEventListener('mousedown', handler);
      window.removeEventListener('scroll', scrollHandler, true);
    };
  }, [rowMenuId]);

  const openRowMenu = (e: React.MouseEvent<HTMLButtonElement>, id: string | number): void => {
    e.stopPropagation();
    if (rowMenuId === id) { setRowMenuId(null); return; }
    const rect = e.currentTarget.getBoundingClientRect();
    const menuH = 160; // 菜单预估高度（2 项 + 分隔线）
    const below = window.innerHeight - rect.bottom;
    const top = below < menuH && rect.top > menuH
      ? rect.top - menuH + rect.height
      : rect.bottom + 4;
    setRowMenuPos({ top, left: rect.right });
    setRowMenuId(id);
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [page, size, keyword]);

  // 搜索输入防抖：本地 query 立即响应，停顿 300ms 后触发后端 keyword 查询
  // （回第 1 页由 useServerPaging 的 resetDeps:[keyword] 统一处理，无需手动 reset）
  useEffect(() => {
    const timer = window.setTimeout(() => {
      setKeyword(query.trim());
    }, 300);
    return () => window.clearTimeout(timer);
  }, [query]);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [listRes, meRes, groupRes] = await Promise.all([
        api.listUsers({ page, size, keyword }),
        api.getMe().catch(() => null),
        api.listGroups().catch(() => null),
      ]);
      setUsers(Array.isArray(listRes?.data) ? (listRes.data as unknown as UserItem[]) : []);
      setTotal(typeof listRes?.total === 'number' ? (listRes.total as number) : 0);
      if (meRes) setMe(meRes.data as UserItem | null);
      if (groupRes) setGroups(Array.isArray(groupRes?.data) ? groupRes.data : []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
      firstLoadRef.current = false;
    }
  };

  const openCreate = () => {
    setEditing(null);
    setForm(EMPTY_FORM);
    setShowModal(true);
  };

  const openEdit = (u: UserItem) => {
    setEditing(u);
    setForm({
      email: u.email || '',
      username: u.username || '',
      password: '',
      role: u.role || 'user',
      quota: String(u.quota ?? 0),
      status: u.status || 'active',
      group: u.group || 'default',
      remark: u.remark || '',
    });
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditing(null);
  };

  const isValidEmail = (email: string): boolean => /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email);

  const handleSave = async (e: FormEvent) => {
    e.preventDefault();
    if (!form.email.trim()) {
      addToast(t('邮箱为必填项'), 'error');
      return;
    }
    if (!isValidEmail(form.email.trim())) {
      addToast(t('邮箱格式不正确'), 'error');
      return;
    }
    if (!editing && !form.password) {
      addToast(t('密码为必填项'), 'error');
      return;
    }
    // 配额必须为有效数字（空串/非数字输入不提交）
    if (form.quota.trim() !== '' && !Number.isFinite(Number(form.quota))) {
      addToast(t('配额必须为数字'), 'error');
      return;
    }
    setSaving(true);
    setError('');
    try {
      if (editing) {
        const payload: Record<string, string | number> = {
          role: form.role,
          quota: Number(form.quota),
          status: form.status,
          group: form.group,
          remark: form.remark.trim(),
        };
        if (form.email.trim() !== editing.email) payload.email = form.email.trim();
        if (form.username.trim()) payload.username = form.username.trim();
        if (form.password) payload.password = form.password;
        await api.updateUser(editing.id, payload);
        addToast(t('用户已更新'));
      } else {
        await api.createUser({
          email: form.email.trim(),
          username: form.username.trim() || undefined,
          password: form.password,
          role: form.role,
          quota: Number(form.quota),
          group: form.group,
          remark: form.remark.trim() || undefined,
        });
        addToast(t('用户已创建'));
      }
      closeModal();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  /** 启用/禁用用户（对齐 new-api ManageUser：禁用即时踢下线） */
  const handleToggleStatus = async (u: UserItem) => {
    if (me && u.id === me.id) {
      addToast(t('不能禁用当前登录的账号'), 'error');
      return;
    }
    const disabling = u.status !== 'disabled';
    setConfirmState({
      title: disabling ? t('禁用用户') : t('启用用户'),
      message: (
        <>
          {disabling ? t('确定禁用用户') : t('确定启用用户')} <strong>{u.email}</strong>？
          {disabling && t('该用户的全部会话将被立即撤销。')}
        </>
      ),
      confirmText: disabling ? t('禁用') : t('启用'),
      danger: disabling,
      onConfirm: async () => {
        setError('');
        try {
          await api.manageUser(String(u.id), disabling ? 'disable' : 'enable');
          addToast(disabling ? t('用户已禁用') : t('用户已启用'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  /** 强制禁用用户 2FA（用户丢失验证器时管理员重置） */
  const handleDisable2FA = (u: UserItem) => {
    setConfirmState({
      title: t('重置两步验证'),
      message: (
        <>
          {t('确定强制禁用用户')} <strong>{u.email}</strong> {t('的两步验证？')}
          {t('该用户的全部会话将被撤销，需重新登录。')}
        </>
      ),
      confirmText: t('强制禁用'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.adminDisable2FA(String(u.id));
          addToast(t('两步验证已重置'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const handleDelete = (u: UserItem) => {
    // 自我保护：不能删除当前登录账号自己
    if (me && u.id === me.id) {
      addToast(t('不能删除当前登录的账号'), 'error');
      return;
    }
    setConfirmState({
      title: t('删除用户'),
      message: (
        <>
          {t('确定删除用户')} <strong>{u.email}</strong>？{t('该操作不可撤销。')}
        </>
      ),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteUser(u.id);
          addToast(t('用户已删除'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  if (loading && firstLoadRef.current) {
    return <SkeletonTable columns={6} rows={7} />;
  }

  const q = query.trim().toLowerCase();
  const visibleUsers = users;

  return (
    <div>
      <div className="page-header">
        <h1>{t('用户管理')}</h1>
        <p>{t('管理系统用户、角色与配额')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {me && (
        <Card bodyClassName="">
          <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', flexWrap: 'wrap', gap: 12 }}>
            <div>
              <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{t('当前登录')}</div>
              <div style={{ fontSize: 18, fontWeight: 600 }}>{me.email}</div>
              {me.username && <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>{me.username}</div>}
            </div>
            <div style={{ display: 'flex', gap: 24, flexWrap: 'wrap' }}>
              <div>
                <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>{t('角色')}</div>
                <div style={{ fontWeight: 600 }}>{me.role === 'admin' ? t('管理员') : t('普通用户')}</div>
              </div>
              <div>
                <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>{t('剩余配额')}</div>
                <div style={{ fontWeight: 600 }}>{fmtQuota((me.quota || 0) - (me.used_quota || 0))}</div>
              </div>
            </div>
          </div>
        </Card>
      )}

      <Card
        title={`${t('所有用户')} (${total})`}
        actions={
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <Input
              placeholder={t('搜索邮箱 / 昵称 / 备注…')}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              style={{ width: 220 }}
            />
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
                  defs={USER_COL_DEFS}
                  items={colState.items}
                  onChange={commitCols}
                  onReset={resetCols}
                  anchorRef={colPanelRef}
                />
              )}
            </div>
            <Button onClick={openCreate}>{t('+ 新建用户')}</Button>
          </div>
        }
      >
        {loading ? (
          /* 局部 loading：只替换表格内容，工具栏/搜索框保持挂载与焦点 */
          <SkeletonTable columns={6} rows={5} />
        ) : visibleUsers.length === 0 ? (
          <EmptyState message={q ? t('没有匹配的用户') : t('暂无用户')} icon="👥" />
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
                        className={colAlignMap[c.key] === 'right' ? 'ta-right' : colAlignMap[c.key] === 'center' ? 'ta-center' : undefined}
                      >
                        {t(USER_COL_DEFS.find((d) => d.key === c.key)?.label ?? c.key)}
                        {c.key !== 'act' && c.key !== 'email' && (
                          <span className="as-col-grip" title={t('拖动调整列宽 · 双击还原')} {...grip} />
                        )}
                      </th>
                    );
                  })}
                </tr>
              </thead>
              <tbody>
                {visibleUsers.map((u) => (
                  <tr key={u.id}>
                    {visibleKeySet.has('email') && (
                    <td><strong>{u.email}</strong></td>
                    )}
                    {visibleKeySet.has('username') && (
                    <td>{u.username || '—'}</td>
                    )}
                    {visibleKeySet.has('remark') && (
                    <td>{u.remark || '—'}</td>
                    )}
                    {visibleKeySet.has('role') && (
                    <td>{u.role === 'admin' ? t('管理员') : t('普通用户')}</td>
                    )}
                    {visibleKeySet.has('group') && (
                    <td>{u.group || 'default'}</td>
                    )}
                    {visibleKeySet.has('quota') && (
                    <td>{fmtQuota(u.quota)}</td>
                    )}
                    {visibleKeySet.has('used') && (
                    <td>{fmtQuota(u.used_quota)}</td>
                    )}
                    {visibleKeySet.has('remain') && (
                    <td>{fmtQuota((u.quota || 0) - (u.used_quota || 0))}</td>
                    )}
                    {visibleKeySet.has('status') && (
                    <td>
                      <span className={u.status === 'active' ? 'badge badge-success' : 'badge badge-danger'}>
                        {u.status === 'active' ? t('启用') : t('禁用')}
                      </span>
                    </td>
                    )}
                    {visibleKeySet.has('created') && (
                    <td>{u.created_at ? new Date(u.created_at > 1e12 ? u.created_at : u.created_at * 1000).toLocaleString() : '—'}</td>
                    )}
                    {visibleKeySet.has('act') && (
                    <td>
                      <div className="us-actions">
                        <button
                          type="button"
                          className="us-icon-btn"
                          title={t('编辑')}
                          onClick={() => openEdit(u)}
                        >
                          <Pencil size={15} />
                        </button>
                        <button
                          type="button"
                          className={`us-icon-btn ${u.status !== 'disabled' ? 'us-danger-hover' : ''}`}
                          title={u.status === 'disabled' ? t('启用') : t('禁用')}
                          onClick={() => void handleToggleStatus(u)}
                          disabled={me != null && u.id === me.id}
                        >
                          {u.status === 'disabled' ? <PowerOff size={15} /> : <Power size={15} />}
                        </button>
                        <div className="us-row-menu">
                          <button
                            type="button"
                            className="us-icon-btn"
                            title={t('更多操作')}
                            onClick={(e) => openRowMenu(e, u.id)}
                          >
                            <MoreHorizontal size={15} />
                          </button>
                        </div>
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

      {/* 行操作菜单（fixed 挂在 body 层级，逃出 Card/table 的 overflow 裁剪） */}
      {rowMenuId !== null && (() => {
        const u = visibleUsers.find((row) => row.id === rowMenuId);
        if (!u) return null;
        const selfProtect = me != null && u.id === me.id;
        return (
          <div
            className="us-row-menu-panel us-row-menu-fixed"
            style={{ top: rowMenuPos.top, left: rowMenuPos.left }}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
          >
            <button type="button" onClick={() => { setRowMenuId(null); handleDisable2FA(u); }}>
              <KeyRound size={14} />
              {t('重置2FA')}
            </button>
            <div className="us-row-menu-sep" />
            <button
              type="button"
              className="us-row-menu-danger"
              onClick={() => { setRowMenuId(null); handleDelete(u); }}
              disabled={selfProtect}
              title={selfProtect ? t('不能删除当前登录的账号') : undefined}
            >
              <Trash2 size={14} />
              {t('删除')}
            </button>
          </div>
        );
      })()}

      <TablePaginationFooter
        total={total}
        page={page}
        totalPages={totalPages}
        size={pageSize}
        sizes={SERVER_PAGE_SIZES}
        onSizeChange={setSize}
        onPageChange={setPage}
      />

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

      {showModal && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={handleSave}>
            <div className="modal-header">
              <h3>{editing ? t('编辑用户') : t('新建用户')}</h3>
              <button type="button" className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              <Input
                label={`${t('邮箱')} *`}
                placeholder="user@example.com"
                value={form.email}
                onChange={(e) => setForm({ ...form, email: e.target.value })}
                autoFocus
              />
              <Input
                label={t('昵称')}
                hint={t('(可选)')}
                placeholder={t('显示名称')}
                value={form.username}
                onChange={(e) => setForm({ ...form, username: e.target.value })}
              />
              <Input
                label={editing ? `${t('密码')} (${t('留空则不修改')})` : `${t('密码')} *`}
                type="password"
                value={form.password}
                onChange={(e) => setForm({ ...form, password: e.target.value })}
              />
              <Select
                label={t('角色')}
                value={form.role}
                onChange={(e) => setForm({ ...form, role: e.target.value })}
              >
                <option value="user">{t('普通用户')}</option>
                <option value="admin">{t('管理员')}</option>
              </Select>
              <Select
                label={t('分组')}
                value={form.group}
                onChange={(e) => setForm({ ...form, group: e.target.value })}
              >
                <option value="default">default</option>
                {groups.filter((g) => g.name && g.name !== 'default').map((g) => (
                  <option key={g.name} value={g.name}>{g.name}</option>
                ))}
              </Select>
              <Input
                label={t('配额')}
                type="number"
                value={form.quota}
                onChange={(e) => setForm({ ...form, quota: e.target.value })}
              />
              <Input
                label={t('用户备注')}
                hint={t('(可选)')}
                placeholder={t('备注')}
                value={form.remark}
                onChange={(e) => setForm({ ...form, remark: e.target.value })}
              />
              {editing && (
                <Select
                  label={t('状态')}
                  value={form.status}
                  onChange={(e) => setForm({ ...form, status: e.target.value })}
                >
                  <option value="active">{t('启用')}</option>
                  <option value="disabled">{t('禁用')}</option>
                </Select>
              )}
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={closeModal} disabled={saving}>{t('取消')}</Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('保存中...') : t('保存')}
              </Button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
