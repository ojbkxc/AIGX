import { useState, useEffect, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Copy, Check, Pencil, Plus, Trash2, Gift } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Input, EmptyState, Select, SkeletonTable } from '../components/ui';
import './Plans.css';

interface PlanItem {
  id: string;
  name: string;
  price?: number;
  quota?: number;
  duration_days?: number;
  group?: string;
  allowed_models?: string[] | null;
  description?: string;
  enabled?: boolean;
  issued_count?: number;
  created_at?: number;
  [key: string]: unknown;
}

interface PlanFormState {
  name: string;
  price: string;
  quota: string;
  duration_days: string;
  group: string;
  allowed_models: string;
  description: string;
  enabled: boolean;
}

const EMPTY_FORM: PlanFormState = {
  name: '',
  price: '',
  quota: '',
  duration_days: '30',
  group: 'default',
  allowed_models: '',
  description: '',
  enabled: true,
};

interface IssuedKeyState {
  name: string;
  key: string;
  planName: string;
}

export default function Plans(): JSX.Element {
  const [plans, setPlans] = useState<PlanItem[]>([]);
  const [groups, setGroups] = useState<{ name?: string }[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const addToast = useToast();
  const { t } = useTranslation();

  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<PlanItem | null>(null);
  const [form, setForm] = useState<PlanFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);

  // 发 key 后一次性明文展示（关闭后仅可经令牌页「查看」取回）
  const [issuedKey, setIssuedKey] = useState<IssuedKeyState | null>(null);
  const [issuingId, setIssuingId] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    void load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const [planRes, groupRes] = await Promise.all([
        api.listPlans(),
        api.listGroups().catch(() => null),
      ]);
      setPlans(Array.isArray(planRes?.data) ? (planRes.data as unknown as PlanItem[]) : []);
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

  const openEdit = (p: PlanItem) => {
    setEditing(p);
    setForm({
      name: p.name || '',
      price: p.price != null ? String(p.price) : '',
      quota: p.quota != null ? String(p.quota) : '',
      duration_days: p.duration_days != null ? String(p.duration_days) : '0',
      group: p.group || 'default',
      allowed_models: Array.isArray(p.allowed_models) ? p.allowed_models.join(', ') : '',
      description: p.description || '',
      enabled: p.enabled !== false,
    });
    setShowModal(true);
  };

  const closeModal = () => {
    setShowModal(false);
    setEditing(null);
  };

  const handleSave = async (e: FormEvent) => {
    e.preventDefault();
    if (!form.name.trim()) {
      addToast(t('套餐名称为必填项'), 'error');
      return;
    }
    setSaving(true);
    setError('');
    try {
      const payload: Record<string, unknown> = {
        name: form.name.trim(),
        price: Number(form.price || 0),
        quota: Number(form.quota || 0),
        duration_days: Number(form.duration_days || 0),
        group: form.group || 'default',
        allowed_models: form.allowed_models
          .split(',')
          .map((s) => s.trim())
          .filter(Boolean),
        description: form.description.trim(),
        enabled: form.enabled,
      };
      if (editing) payload.id = editing.id;
      await api.upsertPlan(payload);
      addToast(editing ? t('套餐已更新') : t('套餐已创建'));
      closeModal();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = (p: PlanItem) => {
    setConfirmState({
      title: t('删除套餐'),
      message: t('确定删除套餐「{{name}}」？已发放的密钥不受影响。', { name: p.name }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deletePlan(p.id);
          addToast(t('套餐已删除'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  // 按套餐发 key：核心交付动作（响应一次性返回明文密钥）
  const handleIssueKey = async (p: PlanItem) => {
    setIssuingId(p.id);
    setError('');
    try {
      const res = await api.issuePlanKey(p.id, {});
      const data = (res?.data ?? {}) as { plain_key?: string; key?: string; name?: string };
      const key = data.plain_key || data.key;
      if (key) {
        setIssuedKey({ name: data.name || p.name, key: String(key), planName: p.name });
      }
      addToast(t('密钥已生成，请立即复制交付买家'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setIssuingId(null);
    }
  };

  const handleToggleEnabled = async (p: PlanItem) => {
    setError('');
    try {
      await api.upsertPlan({
        ...p,
        id: p.id,
        enabled: !(p.enabled !== false),
      });
      addToast(p.enabled !== false ? t('已停售') : t('已上架'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const copyToClipboard = (text: string) => {
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
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      }).catch(() => {
        if (!fallbackCopy()) addToast(t('复制失败，请手动选择复制'), 'error');
        else { addToast(t('已复制到剪贴板')); setCopied(true); window.setTimeout(() => setCopied(false), 1500); }
      });
    } else if (fallbackCopy()) {
      addToast(t('已复制到剪贴板'));
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } else {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  const fmtQuota = (n: number | undefined): string => {
    const v = Number(n || 0);
    if (v >= 1_000_000) return (v / 1_000_000).toFixed(2) + 'M';
    if (v >= 1_000) return (v / 1_000).toFixed(2) + 'K';
    return String(v);
  };

  const fmtDuration = (d: number | undefined): string => {
    const v = Number(d || 0);
    if (v <= 0) return t('永不过期');
    if (v % 365 === 0) return `${v / 365}${t('年')}`;
    if (v % 30 === 0) return `${v / 30}${t('个月')}`;
    return `${v}${t('天')}`;
  };

  if (loading) return <SkeletonTable columns={6} rows={5} />;

  return (
    <div>
      <div className="page-header">
        <h1>{t('套餐管理')}</h1>
        <p>{t('定义按量套餐模板：买家付款后按套餐发放带额度与有效期的 API 密钥')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Card
        title={`${t('套餐列表')} (${plans.length})`}
        actions={<Button onClick={openCreate}><Plus size={14} /> {t('+ 创建套餐')}</Button>}
      >
        {plans.length === 0 ? (
          <EmptyState
            message={t('暂无套餐，创建第一个套餐模板')}
            icon="📦"
            action={<Button onClick={openCreate}>{t('创建套餐')}</Button>}
          />
        ) : (
          <div className="plans-grid">
            {plans.map((p) => (
              <div key={p.id} className={`plan-card ${p.enabled === false ? 'plan-disabled' : ''}`}>
                <div className="plan-card-head">
                  <div className="plan-card-title">
                    <span className="plan-name">{p.name}</span>
                    {p.enabled === false && <span className="badge badge-neutral">{t('已停售')}</span>}
                  </div>
                  <div className="plan-price">
                    {p.price ? `¥${Number(p.price).toFixed(2)}` : '—'}
                  </div>
                </div>
                <div className="plan-card-body">
                  <div className="plan-meta">
                    <span className="plan-meta-label">{t('额度')}</span>
                    <span>{p.quota ? fmtQuota(p.quota) : t('不限')}</span>
                  </div>
                  <div className="plan-meta">
                    <span className="plan-meta-label">{t('有效期')}</span>
                    <span>{fmtDuration(p.duration_days)}</span>
                  </div>
                  <div className="plan-meta">
                    <span className="plan-meta-label">{t('分组')}</span>
                    <span>{p.group || 'default'}</span>
                  </div>
                  <div className="plan-meta">
                    <span className="plan-meta-label">{t('已发放')}</span>
                    <span>{Number(p.issued_count || 0)}</span>
                  </div>
                  {p.description && <p className="plan-desc">{p.description}</p>}
                </div>
                <div className="plan-card-actions">
                  <Button size="sm" onClick={() => handleIssueKey(p)} disabled={p.enabled === false || issuingId === p.id}>
                    <Gift size={14} /> {issuingId === p.id ? t('生成中...') : t('发放密钥')}
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => handleToggleEnabled(p)}>
                    {p.enabled !== false ? t('停售') : t('上架')}
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => openEdit(p)}>
                    <Pencil size={14} />
                  </Button>
                  <Button variant="danger" size="sm" onClick={() => handleDelete(p)}>
                    <Trash2 size={14} />
                  </Button>
                </div>
              </div>
            ))}
          </div>
        )}
      </Card>

      {/* 套餐编辑弹窗 */}
      {showModal && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={handleSave}>
            <div className="modal-header">
              <h3>{editing ? t('编辑套餐') : t('创建套餐')}</h3>
              <button type="button" className="modal-close" onClick={closeModal}>&times;</button>
            </div>
            <div className="modal-body">
              <Input
                label={`${t('名称')} *`}
                placeholder={t('例如：入门包 100K tokens')}
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                autoFocus
              />
              <Input
                label={`${t('售价')} ${t('(元，展示用)')}`}
                type="number"
                step="0.01"
                placeholder="9.9"
                value={form.price}
                onChange={(e) => setForm({ ...form, price: e.target.value })}
              />
              <Input
                label={`${t('额度上限')} ${t('(0 = 不限额度)')}`}
                type="number"
                placeholder="100000"
                value={form.quota}
                onChange={(e) => setForm({ ...form, quota: e.target.value })}
              />
              <Input
                label={`${t('有效期天数')} ${t('(0 = 永不过期)')}`}
                type="number"
                placeholder="30"
                value={form.duration_days}
                onChange={(e) => setForm({ ...form, duration_days: e.target.value })}
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
                placeholder="glm-5.2, deepseek-v3"
                value={form.allowed_models}
                onChange={(e) => setForm({ ...form, allowed_models: e.target.value })}
              />
              <Input
                label={t('描述')}
                placeholder={t('可选')}
                value={form.description}
                onChange={(e) => setForm({ ...form, description: e.target.value })}
              />
              <Select
                label={t('状态')}
                value={form.enabled ? 'on' : 'off'}
                onChange={(e) => setForm({ ...form, enabled: e.target.value === 'on' })}
              >
                <option value="on">{t('上架')}</option>
                <option value="off">{t('停售')}</option>
              </Select>
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={closeModal} disabled={saving}>{t('取消')}</Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('保存中...') : (editing ? t('保存') : t('创建'))}
              </Button>
            </div>
          </form>
        </div>
      )}

      {/* 发 key 成功：一次性明文展示（对齐 Keys 页创建成功契约） */}
      {issuedKey && (
        <div className="modal-overlay" onClick={() => setIssuedKey(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('密钥已生成')} — {issuedKey.planName}</h3>
              <button type="button" className="modal-close" onClick={() => setIssuedKey(null)}>&times;</button>
            </div>
            <div className="modal-body">
              <div className="success-message">{t('套餐密钥发放成功！')}</div>
              <div className="form-group">
                <label>{t('API 密钥')}</label>
                <div className="generated-key-box">
                  <code className="generated-key">{issuedKey.key}</code>
                </div>
                <p className="key-warning">{t('请立即复制交付买家，关闭后可在 API 密钥页通过「查看」取回。')}</p>
              </div>
              <Button onClick={() => copyToClipboard(issuedKey.key)} style={{ width: '100%' }}>
                {copied ? <Check size={14} /> : <Copy size={14} />} {copied ? t('已复制') : t('复制到剪贴板')}
              </Button>
            </div>
            <div className="modal-footer">
              <Button onClick={() => setIssuedKey(null)}>{t('完成')}</Button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
