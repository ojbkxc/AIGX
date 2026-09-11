import { useState, useEffect, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Pencil, Power, PowerOff, MoreHorizontal, KeyRound, Trash2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Input, EmptyState, Select, SkeletonTable, Pagination } from '../components/ui';
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
}

const EMPTY_FORM: UserFormState = {
  email: '',
  username: '',
  password: '',
  role: 'user',
  quota: '0',
  status: 'active',
  group: 'default',
};

export default function Users(): JSX.Element {
  const [users, setUsers] = useState<UserItem[]>([]);
  const [groups, setGroups] = useState<GroupItem[]>([]);
  const [me, setMe] = useState<UserItem | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<UserItem | null>(null);
  const [form, setForm] = useState<UserFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  // 搜索过滤（邮箱/昵称本地匹配）
  const [query, setQuery] = useState('');
  // 分页（对齐 Logs/Channels）
  const [page, setPage] = useState(1);
  const size = 20;
  const [total, setTotal] = useState(0);

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
  }, [page]);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [listRes, meRes, groupRes] = await Promise.all([
        api.listUsers({ page, size }),
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

  const fmtQuota = (q: number | undefined): string => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  if (loading) return <SkeletonTable columns={6} rows={7} />;

  const q = query.trim().toLowerCase();
  const visibleUsers = q
    ? users.filter((u) =>
        (u.email || '').toLowerCase().includes(q) || (u.username || '').toLowerCase().includes(q))
    : users;

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
        title={`${t('所有用户')} (${q ? visibleUsers.length : total}${q ? `/${total}` : ''})`}
        actions={
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <Input
              placeholder={t('搜索邮箱 / 昵称…')}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              style={{ width: 200 }}
            />
            <Button onClick={openCreate}>{t('+ 新建用户')}</Button>
          </div>
        }
      >
        {visibleUsers.length === 0 ? (
          <EmptyState message={q ? t('没有匹配的用户') : t('暂无用户')} icon="👥" />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>{t('邮箱')}</th>
                  <th>{t('昵称')}</th>
                  <th>{t('角色')}</th>
                  <th>{t('分组')}</th>
                  <th>{t('总配额')}</th>
                  <th>{t('已用')}</th>
                  <th>{t('剩余配额')}</th>
                  <th>{t('状态')}</th>
                  <th>{t('创建时间')}</th>
                  <th>{t('操作')}</th>
                </tr>
              </thead>
              <tbody>
                {visibleUsers.map((u) => (
                  <tr key={u.id}>
                    <td><strong>{u.email}</strong></td>
                    <td>{u.username || '—'}</td>
                    <td>{u.role === 'admin' ? t('管理员') : t('普通用户')}</td>
                    <td>{u.group || 'default'}</td>
                    <td>{fmtQuota(u.quota)}</td>
                    <td>{fmtQuota(u.used_quota)}</td>
                    <td>{fmtQuota((u.quota || 0) - (u.used_quota || 0))}</td>
                    <td>
                      <span className={u.status === 'active' ? 'badge badge-success' : 'badge badge-danger'}>
                        {u.status === 'active' ? t('启用') : t('禁用')}
                      </span>
                    </td>
                    <td>{u.created_at ? new Date(u.created_at > 1e12 ? u.created_at : u.created_at * 1000).toLocaleString() : '—'}</td>
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

      {(() => {
        const totalPages = Math.ceil(total / size);
        return totalPages > 1 && !q ? (
          <Pagination page={page} totalPages={totalPages} onChange={setPage} />
        ) : null;
      })()}

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
