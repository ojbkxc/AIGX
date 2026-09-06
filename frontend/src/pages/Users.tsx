import { useState, useEffect, type MouseEvent, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Input, Loading, EmptyState, Select } from '../components/ui';

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

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [listRes, meRes, groupRes] = await Promise.all([
        api.listUsers(),
        api.getMe().catch(() => null),
        api.listGroups().catch(() => null),
      ]);
      setUsers(Array.isArray(listRes?.data) ? listRes.data : []);
      if (meRes) setMe(meRes.data || null);
      if (groupRes) setGroups(Array.isArray(groupRes?.data) ? groupRes.data : groupRes || []);
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

  const handleDelete = (id: string | number) => {
    setConfirmState({
      title: t('删除用户'),
      message: t('确定删除该用户？该操作不可撤销。'),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteUser(id);
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

  if (loading) return <Loading text={t('加载用户列表')} />;

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
              {me.username && <div style={{ fontSize: 13, color: 'var(--text-muted)' }}>@{me.username}</div>}
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
        title={`${t('所有用户')} (${users.length})`}
        actions={<Button onClick={openCreate}>{t('+ 新建用户')}</Button>}
      >
        {users.length === 0 ? (
          <EmptyState message={t('暂无用户')} icon="👥" />
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
                {users.map((u) => (
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
                    <td>{u.created_at ? new Date(u.created_at * 1000).toLocaleString() : '—'}</td>
                    <td>
                      <div className="actions-cell">
                        <Button variant="outline" size="sm" onClick={() => openEdit(u)}>{t('编辑')}</Button>
                        <Button variant="danger" size="sm" onClick={() => handleDelete(u.id)}>{t('删除')}</Button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

      {showModal && (
        <div className="modal-overlay" onClick={closeModal}>
          <form className="modal" onClick={(e) => e.stopPropagation()} onSubmit={handleSave}>
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
              <Button variant="outline" onClick={closeModal}>{t('取消')}</Button>
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
