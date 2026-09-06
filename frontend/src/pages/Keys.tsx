import { useState, useEffect, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { isAdmin } from '../lib/utils';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Input, EmptyState, Select, SkeletonTable } from '../components/ui';
import './Keys.css';

interface TokenItem {
  id: string | number;
  name: string;
  group?: string;
  allowed_models?: string[] | string;
  quota_limit?: number | null;
  used_quota?: number;
  expires_at?: number | null;
  status?: string;
  is_active?: boolean;
  created_at?: number;
  plain_key?: string;
  key?: string;
  api_key?: string;
}

interface GroupItem {
  name?: string;
}

interface KeyFormState {
  name: string;
  group: string;
  allowed_models: string;
  expires_at: string;
  quota_limit: string;
  ip_limit: string;
  status: string;
}

interface GeneratedKeyState {
  key?: string;
  api_key?: string;
  [field: string]: unknown;
}

interface RotatedKeyState {
  name: string;
  key: string;
}

const EMPTY_FORM: KeyFormState = {
  name: '',
  group: 'default',
  allowed_models: '',
  expires_at: '',
  quota_limit: '',
  ip_limit: '',
  status: 'active',
};

export default function Keys(): JSX.Element {
  const [tokens, setTokens] = useState<TokenItem[]>([]);
  const [groups, setGroups] = useState<GroupItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const [showModal, setShowModal] = useState(false);
  // 明文 key 展开状态（列表脱敏；明文经「查看/复制」按需取回）
  const [revealedKeys, setRevealedKeys] = useState<Record<string | number, boolean>>({});
  // 按需取回的明文缓存（管理员列表脱敏，点击查看/复制时经 GET /api/tokens/:id/key 取回）
  const [plainKeys, setPlainKeys] = useState<Record<string | number, string>>({});
  const [editing, setEditing] = useState<TokenItem | null>(null);
  const [form, setForm] = useState<KeyFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [generatedKey, setGeneratedKey] = useState<GeneratedKeyState | null>(null);

  // 令牌轮换后展示的新密钥（一次性显示，提示用户立即保存）
  const [rotatedKey, setRotatedKey] = useState<RotatedKeyState | null>(null);

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [tokenRes, groupRes] = await Promise.all([
        api.listTokens(),
        isAdmin() ? api.listGroups().catch(() => null) : Promise.resolve(null),
      ]);
      setTokens(Array.isArray(tokenRes?.data) ? tokenRes.data : tokenRes || []);
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
    setGeneratedKey(null);
    setShowModal(true);
  };

  const openEdit = (tk: TokenItem) => {
    setEditing(tk);
    setForm({
      name: tk.name || '',
      group: tk.group || 'default',
      allowed_models: Array.isArray(tk.allowed_models)
        ? tk.allowed_models.join(', ')
        : (tk.allowed_models || ''),
      expires_at: tk.expires_at != null ? String(tk.expires_at) : '',
      quota_limit: tk.quota_limit != null ? String(tk.quota_limit) : '',
      ip_limit: '',
      status: tk.status || (tk.is_active === false ? 'disabled' : 'active'),
    });
    setGeneratedKey(null);
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditing(null);
    setGeneratedKey(null);
  };

  const handleSave = async (e: FormEvent) => {
    e.preventDefault();
    if (!form.name.trim()) {
      addToast(t('密钥名称为必填项'), 'error');
      return;
    }
    setSaving(true);
    setError('');
    try {
      const allowedModels = form.allowed_models
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      const ipLimit = form.ip_limit
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      const payload: Record<string, unknown> = {
        name: form.name.trim(),
        group: form.group || 'default',
        allowed_models: allowedModels,
        ip_limit: ipLimit,
        status: form.status,
      };
      if (form.expires_at.trim()) payload.expires_at = Number(form.expires_at);
      if (form.quota_limit.trim()) payload.quota_limit = Number(form.quota_limit);

      if (editing) {
        await api.updateToken(editing.id, payload);
        addToast(t('令牌已更新'));
      } else {
        const res = await api.addToken(payload);
        const created = (res?.data || res || {}) as GeneratedKeyState;
        // 契约：创建响应额外返回 plain_key（一次性明文），优先展示明文而非脱敏密钥
        if (created && (created.plain_key || created.key || created.api_key)) {
          setGeneratedKey({ ...created, key: String(created.plain_key || created.key || created.api_key) });
        }
        addToast(t('令牌创建成功'));
      }
      await load();
      // 编辑保存后关闭弹窗；创建路径保持弹窗以展示一次性密钥
      if (editing) closeModal();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = (id: string | number) => {
    setConfirmState({
      title: t('删除令牌'),
      message: t('确定删除此令牌？使用该令牌的调用将立即失败。'),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteToken(id);
          addToast(t('令牌已删除'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const handleToggleStatus = async (tk: TokenItem) => {
    const newStatus = tk.status === 'disabled' ? 'active' : 'disabled';
    setError('');
    try {
      await api.updateToken(tk.id, { status: newStatus });
      addToast(newStatus === 'active' ? t('已启用') : t('已禁用'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleResetUsed = (id: string | number) => {
    setConfirmState({
      title: t('重置已用'),
      message: t('确定重置已用额度为 0？'),
      confirmText: t('重置已用'),
      danger: false,
      onConfirm: async () => {
        setError('');
        try {
          await api.resetTokenUsed(id);
          addToast(t('已重置已用额度'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  // 令牌轮换：生成新密钥，旧密钥立即失效；新密钥仅此一次展示
  const handleRotate = (tk: TokenItem) => {
    setConfirmState({
      title: t('轮换令牌密钥'),
      message: t('轮换将生成新密钥，旧密钥立即失效。确定继续？'),
      confirmText: t('轮换'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          const res = await api.rotateToken(tk.id);
          const data = (res?.data || res || {}) as GeneratedKeyState;
          // 后端返回新密钥明文（plain_key / key / api_key）
          const newKey = data.plain_key || data.key || data.api_key;
          if (newKey) {
            setRotatedKey({ name: tk.name, key: String(newKey) });
          }
          addToast(t('令牌已轮换，请立即保存新密钥'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const copyToClipboard = (text: string) => {
    // navigator.clipboard 仅在安全上下文（HTTPS/localhost）可用；AIGX 常以
    // http://IP:9527 部署，需降级 execCommand 方案，否则复制静默失败。
    const fallbackCopy = (): boolean => {
      try {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        const ok = document.execCommand('copy');
        document.body.removeChild(ta);
        return ok;
      } catch {
        return false;
      }
    };
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(() => {
        addToast(t('已复制到剪贴板'));
      }).catch(() => {
        if (!fallbackCopy()) addToast(t('复制失败，请手动选择复制'), 'error');
        else addToast(t('已复制到剪贴板'));
      });
    } else if (fallbackCopy()) {
      addToast(t('已复制到剪贴板'));
    } else {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  // 取回令牌明文：本地已有则直接用；否则请求后端（失败时提示）
  const fetchPlainKey = async (tk: TokenItem): Promise<string | null> => {
    const cached = plainKeys[tk.id] || tk.plain_key;
    if (cached) return cached;
    try {
      const res = await api.getTokenKey(tk.id);
      const key = res?.data?.plain_key || res?.plain_key;
      if (key) {
        setPlainKeys((prev) => ({ ...prev, [tk.id]: String(key) }));
        return String(key);
      }
    } catch (err) {
      addToast(err instanceof Error ? err.message : t('获取密钥失败'), 'error');
      return null;
    }
    return null;
  };

  const fmtQuota = (q: number | undefined): string => {
    const n = Number(q || 0);
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(2) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(2) + 'K';
    return String(n);
  };

  const isExpired = (tk: TokenItem): boolean => {
    if (!tk.expires_at) return false;
    return Number(tk.expires_at) < Math.floor(Date.now() / 1000);
  };

  if (loading) return <SkeletonTable columns={5} rows={6} />;

  return (
    <div>
      <div className="page-header">
        <h1>{t('API 令牌')}</h1>
        <p>{t('管理 API 令牌：分组、模型白名单、额度、过期与 IP 限制')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Card
        title={`${t('所有令牌')} (${tokens.length})`}
        actions={<Button onClick={openCreate}>{t('+ 创建令牌')}</Button>}
      >
        {tokens.length === 0 ? (
          <EmptyState message={t('暂无 API 令牌')} icon="🔑" action={<Button onClick={openCreate}>{t('创建第一个令牌')}</Button>} />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>{t('名称')}</th>
                  <th>{t('密钥')}</th>
                  <th>{t('分组')}</th>
                  <th>{t('模型白名单')}</th>
                  <th>{t('额度')}</th>
                  <th>{t('过期')}</th>
                  <th>{t('状态')}</th>
                  <th>{t('创建时间')}</th>
                  <th>{t('操作')}</th>
                </tr>
              </thead>
              <tbody>
                {tokens.map((tk) => {
                  const expired = isExpired(tk);
                  const disabled = tk.status === 'disabled' || tk.is_active === false;
                  return (
                    <tr key={tk.id}>
                      <td><strong>{tk.name}</strong></td>
                      <td style={{ fontSize: 12 }}>
                        {tk.plain_key || plainKeys[tk.id] ? (
                          <span style={{ display: 'inline-flex', alignItems: 'center', gap: '6px' }}>
                            <code style={{
                              fontSize: 11,
                              maxWidth: 220,
                              overflow: 'hidden',
                              textOverflow: 'ellipsis',
                              whiteSpace: 'nowrap',
                              display: 'inline-block',
                              verticalAlign: 'middle',
                            }}>
                              {revealedKeys[tk.id] ? (plainKeys[tk.id] || tk.plain_key) : tk.key || '••••••••••••'}
                            </code>
                            <button
                              type="button"
                              className="btn btn-outline btn-sm"
                              title={revealedKeys[tk.id] ? t('隐藏密钥') : t('查看密钥')}
                              onClick={() => {
                                if (!revealedKeys[tk.id] && !plainKeys[tk.id] && !tk.plain_key) {
                                  void fetchPlainKey(tk);
                                }
                                setRevealedKeys((prev) => ({ ...prev, [tk.id]: !prev[tk.id] }));
                              }}
                              style={{ padding: '2px 6px', flexShrink: 0 }}
                            >
                              {revealedKeys[tk.id] ? t('隐藏') : t('查看')}
                            </button>
                            <button
                              type="button"
                              className="btn btn-outline btn-sm"
                              title={t('复制密钥')}
                              onClick={() => void fetchPlainKey(tk).then((key) => key && copyToClipboard(key))}
                              style={{ padding: '2px 6px', flexShrink: 0 }}
                            >
                              {t('复制')}
                            </button>
                          </span>
                        ) : (
                          <span style={{ color: 'var(--text-muted)' }}>
                            {tk.key || '••••••••••••'}
                          </span>
                        )}
                      </td>
                      <td>{tk.group || 'default'}</td>
                      <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                        {Array.isArray(tk.allowed_models) && tk.allowed_models.length > 0
                          ? tk.allowed_models.join(', ')
                          : (tk.allowed_models || t('全部'))}
                      </td>
                      <td style={{ fontSize: 12 }}>
                        {tk.quota_limit
                          ? `${fmtQuota(tk.used_quota)} / ${fmtQuota(tk.quota_limit)}`
                          : `${fmtQuota(tk.used_quota)} / ∞`}
                      </td>
                      <td style={{ fontSize: 12, color: expired ? 'rgb(239,68,68)' : 'var(--text-muted)' }}>
                        {tk.expires_at
                          ? new Date(tk.expires_at * 1000).toLocaleDateString()
                          : t('永不过期')}
                        {expired && t('(已过期)')}
                      </td>
                      <td>
                        <span className={disabled ? 'badge badge-danger' : 'badge badge-success'}>
                          {disabled ? t('禁用') : t('启用')}
                        </span>
                      </td>
                      <td style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                        {tk.created_at
                          ? new Date(tk.created_at > 1e12 ? tk.created_at : tk.created_at * 1000).toLocaleDateString()
                          : '—'}
                      </td>
                      <td>
                        <div className="actions-cell">
                          <Button variant="outline" size="sm" onClick={() => openEdit(tk)}>{t('编辑')}</Button>
                          <Button variant="outline" size="sm" onClick={() => void handleToggleStatus(tk)}>
                            {disabled ? t('启用') : t('禁用')}
                          </Button>
                          {tk.quota_limit && (
                            <Button variant="outline" size="sm" onClick={() => handleResetUsed(tk.id)}>
                              {t('重置已用')}
                            </Button>
                          )}
                          <Button variant="outline" size="sm" onClick={() => handleRotate(tk)}>{t('轮换')}</Button>
                          <Button variant="danger" size="sm" onClick={() => handleDelete(tk.id)}>{t('删除')}</Button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />

      {/* 令牌轮换后新密钥展示模态框：新密钥仅此一次显示 */}
      {rotatedKey && (
        <div className="modal-overlay" onClick={() => setRotatedKey(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('令牌已轮换')} — {rotatedKey.name}</h3>
              <button type="button" className="modal-close" onClick={() => setRotatedKey(null)}>&times;</button>
            </div>
            <div className="modal-body">
              <div className="success-message">{t('轮换成功！请立即保存新密钥，此为最后一次显示。')}</div>
              <div className="form-group">
                <label>{t('新 API 密钥')}</label>
                <div className="generated-key-box">
                  <code className="generated-key">{rotatedKey.key}</code>
                </div>
                <p className="key-warning">{t('旧密钥已立即失效，请将新密钥保存到安全位置，关闭后将无法再次查看。')}</p>
              </div>
              <Button onClick={() => copyToClipboard(rotatedKey.key)} style={{ width: '100%' }}>
                {t('复制到剪贴板')}
              </Button>
            </div>
            <div className="modal-footer">
              <Button onClick={() => setRotatedKey(null)}>{t('我已保存')}</Button>
            </div>
          </div>
        </div>
      )}

      {showModal && (
        <div className="modal-overlay" onClick={closeModal}>
          <form className="modal" onClick={(e) => e.stopPropagation()} onSubmit={handleSave}>
            <div className="modal-header">
              <h3>{editing ? t('编辑令牌') : t('创建 API 令牌')}</h3>
              <button type="button" className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              {generatedKey && !editing ? (
                <div>
                  <div className="success-message">{t('令牌创建成功！')}</div>
                  <div className="form-group">
                    <label>{t('您的 API 密钥')}</label>
                    <div className="generated-key-box">
                      <code className="generated-key">{generatedKey.key || generatedKey.api_key}</code>
                    </div>
                    <p className="key-warning">{t('请立即复制此密钥，关闭后将无法再次查看。')}</p>
                  </div>
                  <Button onClick={() => copyToClipboard(String(generatedKey.key || generatedKey.api_key))} style={{ width: '100%' }}>
                    {t('复制到剪贴板')}
                  </Button>
                </div>
              ) : (
                <>
                  <Input
                    label={`${t('名称')} *`}
                    placeholder={t('例如：开发环境令牌')}
                    value={form.name}
                    onChange={(e) => setForm({ ...form, name: e.target.value })}
                    autoFocus
                  />
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
                    label={`${t('模型白名单')} ${t('(逗号分隔，留空则允许全部)')}`}
                    placeholder={editing ? t('留空表示不修改') : 'glm-5.2, deepseek-v3, kimi-k2.6'}
                    value={form.allowed_models}
                    onChange={(e) => setForm({ ...form, allowed_models: e.target.value })}
                  />
                  <Input
                    label={`${t('过期时间')} ${t('(Unix 时间戳，留空则永不过期)')}`}
                    type="number"
                    placeholder={editing ? t('留空表示不修改') : t('keysPlaceholderExpiresAt')}
                    value={form.expires_at}
                    onChange={(e) => setForm({ ...form, expires_at: e.target.value })}
                  />
                  <Input
                    label={`${t('额度上限')} ${t('(留空则无上限)')}`}
                    type="number"
                    placeholder={editing ? t('留空表示不修改') : t('keysPlaceholderQuotaLimit')}
                    value={form.quota_limit}
                    onChange={(e) => setForm({ ...form, quota_limit: e.target.value })}
                  />
                  <Input
                    label={`${t('IP 限制')} ${t('(逗号分隔，留空则不限制)')}`}
                    placeholder={editing ? t('留空表示不修改') : t('keysPlaceholderIpLimit')}
                    value={form.ip_limit}
                    onChange={(e) => setForm({ ...form, ip_limit: e.target.value })}
                  />
                  <Select
                    label={t('状态')}
                    value={form.status}
                    onChange={(e) => setForm({ ...form, status: e.target.value })}
                  >
                    <option value="active">{t('启用')}</option>
                    <option value="disabled">{t('禁用')}</option>
                  </Select>
                </>
              )}
            </div>
            <div className="modal-footer">
              {generatedKey && !editing ? (
                <Button onClick={closeModal}>{t('完成')}</Button>
              ) : (
                <>
                  <Button variant="outline" onClick={closeModal}>{t('取消')}</Button>
                  <Button type="submit" disabled={saving}>
                    {saving ? t('保存中...') : (editing ? t('保存') : t('创建'))}
                  </Button>
                </>
              )}
            </div>
          </form>
        </div>
      )}
    </div>
  );
}
