import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Loading, EmptyState } from '../components/ui';
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

export default function Groups(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [groups, setGroups] = useState<GroupItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<GroupItem | null>(null);
  const [form, setForm] = useState<GroupForm>({ name: '', ratio: '1', allowed_models: '', description: '' });
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    void loadGroups();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadGroups = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listGroups();
      setGroups(res?.data || res || []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const openAdd = () => {
    setEditing(null);
    setForm({ name: '', ratio: '1', allowed_models: '', description: '' });
    setShowModal(true);
  };

  const openEdit = (g: GroupItem) => {
    setEditing(g);
    setForm({
      name: g.name || '',
      ratio: String(g.ratio != null ? g.ratio : 1),
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

  // payload 与后端 GroupRequest 对齐
  const handleSave = async () => {
    if (!form.name.trim()) {
      setError(t('分组名称为必填项'));
      return;
    }
    setSaving(true);
    setError('');
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
      addToast(t('用户分组已保存'));
      closeModal();
      void loadGroups();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
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
      {/* PageIntro 标题区 */}
      <div className="page-header">
        <div>
          <h1>{t('用户分组')}</h1>
          <p>{t('管理用户分组与计费倍率，控制不同分组的模型访问权限与费率')}</p>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="groups-content">
        {loading ? (
          <Loading text={t('加载用户分组')} />
        ) : (
          <Card
            title={`${t('所有分组')} (${groups.length})`}
            actions={<Button onClick={openAdd}>{t('+ 新建分组')}</Button>}
          >
            {groups.length === 0 ? (
              <EmptyState
                message={t('暂无自定义分组，使用默认分组 (倍率 1.0)')}
                action={<Button onClick={openAdd}>{t('新建分组')}</Button>}
              />
            ) : (
              <div className="table-wrapper">
                <table>
                  <thead>
                    <tr>
                      <th>{t('分组名称')}</th>
                      <th>{t('倍率')}</th>
                      <th>{t('允许模型')}</th>
                      <th>{t('描述')}</th>
                      <th>{t('操作')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {groups.map((g) => (
                      <tr key={g.name}>
                        <td>
                          <strong>{g.name}</strong>
                          {g.name === 'default' && (
                            <span className="default-tag">{t('默认')}</span>
                          )}
                        </td>
                        <td>
                          <span className="ratio-badge" data-high={(g.ratio || 1) > 1}>
                            ×{g.ratio != null ? g.ratio : 1}
                          </span>
                        </td>
                        <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                          {Array.isArray(g.allowed_models) && g.allowed_models.length > 0
                            ? g.allowed_models.join(', ')
                            : t('全部')}
                        </td>
                        <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                          {g.description || '—'}
                        </td>
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
        <div className="modal-overlay" onClick={closeModal}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{editing ? t('编辑分组') : t('新建分组')}</h3>
              <button className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
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
              <button className="btn btn-outline" onClick={closeModal}>{t('取消')}</button>
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