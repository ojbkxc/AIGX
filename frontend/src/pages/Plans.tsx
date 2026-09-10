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
  // ── 订阅化扩展（#85）────────────────────────────────────────
  plan_type?: string;
  duration_unit?: string;
  duration_value?: number;
  custom_seconds?: number;
  total_amount?: number;
  quota_reset_period?: string;
  quota_reset_custom_seconds?: number;
  allow_balance_pay?: boolean;
  allow_wallet_overflow?: boolean;
  max_purchase_per_user?: number;
  upgrade_group?: string;
  downgrade_group?: string;
  sort_order?: number;
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
  // ── 订阅化字段（#85）────────────────────────────────────────
  plan_type: 'once' | 'subscription';
  duration_unit: string;
  duration_value: string;
  custom_seconds: string;
  total_amount: string;
  quota_reset_period: string;
  quota_reset_custom_seconds: string;
  allow_balance_pay: boolean;
  allow_wallet_overflow: boolean;
  max_purchase_per_user: string;
  upgrade_group: string;
  downgrade_group: string;
  sort_order: string;
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
  plan_type: 'once',
  duration_unit: 'month',
  duration_value: '1',
  custom_seconds: '',
  total_amount: '',
  quota_reset_period: 'never',
  quota_reset_custom_seconds: '',
  allow_balance_pay: true,
  allow_wallet_overflow: true,
  max_purchase_per_user: '',
  upgrade_group: '',
  downgrade_group: '',
  sort_order: '0',
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
      plan_type: p.plan_type === 'subscription' ? 'subscription' : 'once',
      duration_unit: p.duration_unit || 'month',
      duration_value: p.duration_value != null ? String(p.duration_value) : '1',
      custom_seconds: p.custom_seconds ? String(p.custom_seconds) : '',
      total_amount: p.total_amount ? String(p.total_amount) : '',
      quota_reset_period: p.quota_reset_period || 'never',
      quota_reset_custom_seconds: p.quota_reset_custom_seconds ? String(p.quota_reset_custom_seconds) : '',
      allow_balance_pay: p.allow_balance_pay !== false,
      allow_wallet_overflow: p.allow_wallet_overflow !== false,
      max_purchase_per_user: p.max_purchase_per_user ? String(p.max_purchase_per_user) : '',
      upgrade_group: p.upgrade_group || '',
      downgrade_group: p.downgrade_group || '',
      sort_order: p.sort_order != null ? String(p.sort_order) : '0',
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
      const isSub = form.plan_type === 'subscription';
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
        plan_type: form.plan_type,
        sort_order: Number(form.sort_order || 0),
      };
      if (isSub) {
        payload.duration_unit = form.duration_unit;
        payload.duration_value = Number(form.duration_value || 0);
        payload.custom_seconds = Number(form.custom_seconds || 0);
        payload.total_amount = Number(form.total_amount || 0);
        payload.quota_reset_period = form.quota_reset_period;
        payload.quota_reset_custom_seconds = Number(form.quota_reset_custom_seconds || 0);
        payload.allow_balance_pay = form.allow_balance_pay;
        payload.allow_wallet_overflow = form.allow_wallet_overflow;
        payload.max_purchase_per_user = Number(form.max_purchase_per_user || 0);
        payload.upgrade_group = form.upgrade_group.trim();
        payload.downgrade_group = form.downgrade_group.trim();
      }
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

  // 订阅时长展示（#85）
  const fmtSubDuration = (p: PlanItem): string => {
    if (p.duration_unit === 'custom') {
      const s = Number(p.custom_seconds || 0);
      if (s <= 0) return t('未配置');
      if (s % 86400 === 0) return `${s / 86400}${t('天')}`;
      if (s % 3600 === 0) return `${s / 3600}${t('小时')}`;
      return `${s}${t('秒')}`;
    }
    const unitMap: Record<string, string> = { year: t('年'), month: t('月'), day: t('天'), hour: t('小时') };
    const v = Number(p.duration_value || 0);
    if (v <= 0) return t('未配置');
    return `${v} ${unitMap[p.duration_unit || 'month'] || ''}`;
  };

  const fmtResetPeriod = (period: string | undefined): string => {
    const map: Record<string, string> = {
      never: t('不重置'),
      daily: t('每日'),
      weekly: t('每周'),
      monthly: t('每月'),
      custom: t('自定义'),
    };
    return map[period || 'never'] || period || t('不重置');
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
                    {p.plan_type === 'subscription' && <span className="badge badge-info">{t('订阅')}</span>}
                    {p.enabled === false && <span className="badge badge-neutral">{t('已停售')}</span>}
                  </div>
                  <div className="plan-price">
                    {p.price ? `¥${Number(p.price).toFixed(2)}` : '—'}
                  </div>
                </div>
                <div className="plan-card-body">
                  {p.plan_type === 'subscription' ? (
                    <>
                      <div className="plan-meta">
                        <span className="plan-meta-label">{t('时长')}</span>
                        <span>{fmtSubDuration(p)}</span>
                      </div>
                      <div className="plan-meta">
                        <span className="plan-meta-label">{t('订阅配额')}</span>
                        <span>{p.total_amount ? fmtQuota(p.total_amount) : t('不限')}</span>
                      </div>
                      <div className="plan-meta">
                        <span className="plan-meta-label">{t('周期重置')}</span>
                        <span>{fmtResetPeriod(p.quota_reset_period)}</span>
                      </div>
                      {p.upgrade_group && (
                        <div className="plan-meta">
                          <span className="plan-meta-label">{t('升级分组')}</span>
                          <span>{p.upgrade_group}</span>
                        </div>
                      )}
                    </>
                  ) : (
                    <>
                      <div className="plan-meta">
                        <span className="plan-meta-label">{t('额度')}</span>
                        <span>{p.quota ? fmtQuota(p.quota) : t('不限')}</span>
                      </div>
                      <div className="plan-meta">
                        <span className="plan-meta-label">{t('有效期')}</span>
                        <span>{fmtDuration(p.duration_days)}</span>
                      </div>
                    </>
                  )}
                  <div className="plan-meta">
                    <span className="plan-meta-label">{t('分组')}</span>
                    <span>{p.group || 'default'}</span>
                  </div>
                  {p.plan_type !== 'subscription' && (
                    <div className="plan-meta">
                      <span className="plan-meta-label">{t('已发放')}</span>
                      <span>{Number(p.issued_count || 0)}</span>
                    </div>
                  )}
                  {p.description && <p className="plan-desc">{p.description}</p>}
                </div>
                <div className="plan-card-actions">
                  {p.plan_type === 'subscription' ? (
                    <span className="plan-sub-hint" title={t('订阅套餐由用户在钱包页用余额购买')}>{t('余额购买制')}</span>
                  ) : (
                    <Button size="sm" onClick={() => handleIssueKey(p)} disabled={p.enabled === false || issuingId === p.id}>
                      <Gift size={14} /> {issuingId === p.id ? t('生成中...') : t('发放密钥')}
                    </Button>
                  )}
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
              <Select
                label={t('套餐类型')}
                value={form.plan_type}
                onChange={(e) => setForm({ ...form, plan_type: e.target.value as 'once' | 'subscription' })}
              >
                <option value="once">{t('按量套餐（发 API 密钥）')}</option>
                <option value="subscription">{t('时长订阅（余额购买）')}</option>
              </Select>
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
              {form.plan_type === 'once' ? (
                <>
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
                </>
              ) : (
                <>
                  <div style={{ display: 'grid', gridTemplateColumns: '2fr 1fr', gap: 12 }}>
                    <Select
                      label={t('订阅时长单位')}
                      value={form.duration_unit}
                      onChange={(e) => setForm({ ...form, duration_unit: e.target.value })}
                    >
                      <option value="year">{t('年')}</option>
                      <option value="month">{t('月')}</option>
                      <option value="day">{t('天')}</option>
                      <option value="hour">{t('小时')}</option>
                      <option value="custom">{t('自定义')}</option>
                    </Select>
                    {form.duration_unit === 'custom' ? (
                      <Input
                        label={`${t('自定义秒数')} *`}
                        type="number"
                        placeholder="2592000"
                        value={form.custom_seconds}
                        onChange={(e) => setForm({ ...form, custom_seconds: e.target.value })}
                      />
                    ) : (
                      <Input
                        label={`${t('时长数值')} *`}
                        type="number"
                        min="1"
                        placeholder="1"
                        value={form.duration_value}
                        onChange={(e) => setForm({ ...form, duration_value: e.target.value })}
                      />
                    )}
                  </div>
                  <Input
                    label={`${t('订阅总配额')} ${t('(0 = 不限)')}`}
                    type="number"
                    placeholder="500000"
                    hint={t('订阅期内可用总额度，耗尽后按下方策略处理')}
                    value={form.total_amount}
                    onChange={(e) => setForm({ ...form, total_amount: e.target.value })}
                  />
                  <Select
                    label={t('配额周期重置')}
                    value={form.quota_reset_period}
                    onChange={(e) => setForm({ ...form, quota_reset_period: e.target.value })}
                  >
                    <option value="never">{t('不重置')}</option>
                    <option value="daily">{t('每日重置')}</option>
                    <option value="weekly">{t('每周重置')}</option>
                    <option value="monthly">{t('每月重置')}</option>
                    <option value="custom">{t('自定义周期')}</option>
                  </Select>
                  {form.quota_reset_period === 'custom' && (
                    <Input
                      label={`${t('重置周期秒数')} *`}
                      type="number"
                      placeholder="604800"
                      value={form.quota_reset_custom_seconds}
                      onChange={(e) => setForm({ ...form, quota_reset_custom_seconds: e.target.value })}
                    />
                  )}
                  <Select
                    label={t('余额购买')}
                    value={form.allow_balance_pay ? 'on' : 'off'}
                    onChange={(e) => setForm({ ...form, allow_balance_pay: e.target.value === 'on' })}
                  >
                    <option value="on">{t('允许用户用余额购买')}</option>
                    <option value="off">{t('禁止余额购买')}</option>
                  </Select>
                  <Select
                    label={t('池耗尽回退钱包')}
                    value={form.allow_wallet_overflow ? 'on' : 'off'}
                    onChange={(e) => setForm({ ...form, allow_wallet_overflow: e.target.value === 'on' })}
                  >
                    <option value="on">{t('允许：订阅池耗尽后继续扣钱包余额')}</option>
                    <option value="off">{t('禁止：池耗尽即拒绝请求')}</option>
                  </Select>
                  <Input
                    label={`${t('每用户限购次数')} ${t('(0 = 不限)')}`}
                    type="number"
                    placeholder="1"
                    value={form.max_purchase_per_user}
                    onChange={(e) => setForm({ ...form, max_purchase_per_user: e.target.value })}
                  />
                  <Select
                    label={t('购买后升级分组')}
                    value={form.upgrade_group}
                    onChange={(e) => setForm({ ...form, upgrade_group: e.target.value })}
                  >
                    <option value="">{t('不变更')}</option>
                    {groups.filter((g) => g.name).map((g) => (
                      <option key={g.name} value={g.name}>{g.name}</option>
                    ))}
                  </Select>
                  <Select
                    label={t('到期回退分组')}
                    value={form.downgrade_group}
                    onChange={(e) => setForm({ ...form, downgrade_group: e.target.value })}
                  >
                    <option value="">{t('回退购买前分组')}</option>
                    {groups.filter((g) => g.name).map((g) => (
                      <option key={g.name} value={g.name}>{g.name}</option>
                    ))}
                  </Select>
                  <Input
                    label={t('展示排序（升序）')}
                    type="number"
                    value={form.sort_order}
                    onChange={(e) => setForm({ ...form, sort_order: e.target.value })}
                  />
                </>
              )}
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
