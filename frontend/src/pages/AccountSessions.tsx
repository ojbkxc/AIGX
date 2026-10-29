import { useEffect, useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { FlaskConical, Pencil, Plus, RotateCcw, Trash2, Upload } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, EmptyState, Input, Select, SkeletonTable, Textarea } from '../components/ui';
import type {
  AccountProviderItem,
  AccountSessionItem,
  AccountSessionStatsItem,
  ChannelItem,
} from '../types';

interface SessionFormState {
  channel_id: string;
  name: string;
  provider: string;
  base_url: string;
  api_key: string;
  proxy: string;
  status: string;
  priority: string;
  weight: string;
  max_concurrency: string;
  models: string;
  expires_at: string;
  clear_key: boolean;
}

const EMPTY_FORM: SessionFormState = {
  channel_id: '',
  name: '',
  provider: 'openai_compatible',
  base_url: '',
  api_key: '',
  proxy: '',
  status: 'enabled',
  priority: '0',
  weight: '1',
  max_concurrency: '3',
  models: '',
  expires_at: '',
  clear_key: false,
};

interface BulkFormState {
  channel_id: string;
  provider: string;
  base_url: string;
  proxy: string;
  name_prefix: string;
  priority: string;
  weight: string;
  max_concurrency: string;
  models: string;
  credentials: string;
}

const EMPTY_BULK: BulkFormState = {
  channel_id: '',
  provider: 'openai_compatible',
  base_url: '',
  proxy: '',
  name_prefix: 'account',
  priority: '0',
  weight: '1',
  max_concurrency: '3',
  models: '',
  credentials: '',
};

/** 运行态 → badge 样式 */
function statusClass(status?: string): string {
  if (status === 'enabled') return 'badge-success';
  if (status === 'expired') return 'badge-warning';
  return 'badge-neutral';
}

export default function AccountSessions(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [sessions, setSessions] = useState<AccountSessionItem[]>([]);
  const [accountChannels, setAccountChannels] = useState<ChannelItem[]>([]);
  const [providers, setProviders] = useState<AccountProviderItem[]>([]);
  const [stats, setStats] = useState<AccountSessionStatsItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  const [channelFilter, setChannelFilter] = useState('');
  const [search, setSearch] = useState('');

  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<AccountSessionItem | null>(null);
  const [form, setForm] = useState<SessionFormState>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);

  const [showBulk, setShowBulk] = useState(false);
  const [bulk, setBulk] = useState<BulkFormState>(EMPTY_BULK);

  const [testingId, setTestingId] = useState<string | null>(null);
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);

  const load = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const [sessionsRes, channelsRes, providersRes, statsRes] = await Promise.all([
        api.listAccountSessions(),
        api.listChannels(),
        api.accountProviders(),
        api.accountSessionStats(),
      ]);
      setSessions(sessionsRes.data ?? []);
      const channels = channelsRes.data ?? [];
      setAccountChannels(channels.filter((c) => c.channel_type === 'account'));
      setProviders(providersRes.data ?? []);
      setStats(statsRes.data?.channels ?? []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const openCreate = (): void => {
    setEditing(null);
    setForm({
      ...EMPTY_FORM,
      channel_id: channelFilter || accountChannels[0]?.id || '',
    });
    setShowModal(true);
  };

  const openEdit = (session: AccountSessionItem): void => {
    setEditing(session);
    setForm({
      channel_id: session.channel_id,
      name: session.name,
      provider: session.provider || 'openai_compatible',
      base_url: session.base_url || '',
      api_key: '',
      proxy: session.proxy || '',
      status: session.configured_status || (session.status === 'enabled' ? 'enabled' : 'disabled'),
      priority: String(session.priority ?? 0),
      weight: String(session.weight ?? 1),
      max_concurrency: String(session.max_concurrency ?? 0),
      models: (session.models || []).join(', '),
      expires_at: session.expires_at ? String(session.expires_at) : '',
      clear_key: false,
    });
    setShowModal(true);
  };

  const closeModal = (): void => {
    setShowModal(false);
    setEditing(null);
    setSaving(false);
  };

  const openBulk = (): void => {
    setBulk({
      ...EMPTY_BULK,
      channel_id: channelFilter || accountChannels[0]?.id || '',
    });
    setShowBulk(true);
  };

  const splitModels = (raw: string): string[] =>
    raw
      .split(/[,\s]+/)
      .map((m) => m.trim())
      .filter((m) => m.length > 0);

  const handleSave = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setError('');
    if (!form.channel_id) {
      setError(t('请选择渠道'));
      return;
    }
    setSaving(true);
    try {
      const payload: Record<string, unknown> = {
        channel_id: form.channel_id,
        name: form.name.trim(),
        provider: form.provider,
        base_url: form.base_url.trim(),
        proxy: form.proxy.trim(),
        status: form.status,
        priority: Number(form.priority || 0),
        weight: Number(form.weight || 1),
        max_concurrency: Number(form.max_concurrency || 0),
        models: splitModels(form.models),
        expires_at: form.expires_at.trim() ? Number(form.expires_at) : null,
      };
      if (editing) {
        if (form.clear_key) {
          payload.api_key = '';
        } else if (form.api_key.trim()) {
          payload.api_key = form.api_key.trim();
        }
        await api.updateAccountSession(editing.id, payload);
        addToast(t('账号已保存'));
      } else {
        payload.api_key = form.api_key.trim();
        await api.createAccountSession(payload);
        addToast(t('账号已创建'));
      }
      closeModal();
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleBulk = async (e: FormEvent): Promise<void> => {
    e.preventDefault();
    setError('');
    if (!bulk.channel_id) {
      setError(t('请选择渠道'));
      return;
    }
    if (!bulk.credentials.trim()) {
      setError(t('请粘贴至少一条凭证'));
      return;
    }
    setSaving(true);
    try {
      const res = await api.bulkImportAccountSessions({
        channel_id: bulk.channel_id,
        provider: bulk.provider,
        base_url: bulk.base_url.trim(),
        proxy: bulk.proxy.trim(),
        name_prefix: bulk.name_prefix.trim(),
        priority: Number(bulk.priority || 0),
        weight: Number(bulk.weight || 1),
        max_concurrency: Number(bulk.max_concurrency || 0),
        models: splitModels(bulk.models),
        credentials: bulk.credentials,
      });
      addToast(
        t('导入完成：成功 {{created}} 个，跳过 {{skipped}} 个', {
          created: res.data?.created ?? 0,
          skipped: res.data?.skipped ?? 0,
        })
      );
      setShowBulk(false);
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleTest = async (session: AccountSessionItem): Promise<void> => {
    setTestingId(session.id);
    setError('');
    try {
      const res = await api.testAccountSession(session.id);
      const data = res.data;
      if (data?.success) {
        addToast(`${t('测试成功')} · ${data.model || ''} · ${data.latency_ms ?? 0}ms`);
      } else {
        addToast(`${t('测试失败')}: ${data?.message || ''}`, 'error');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setTestingId(null);
    }
  };

  const handleResetCooldowns = async (session: AccountSessionItem): Promise<void> => {
    setError('');
    try {
      await api.resetAccountSessionCooldowns(session.id);
      addToast(t('冷却已清除'));
      await load();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleDelete = (session: AccountSessionItem): void => {
    setConfirmState({
      title: t('删除账号'),
      message: t('确定删除账号「{{name}}」？该账号的凭证与冷却记录会一并删除。', {
        name: session.name,
      }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        setError('');
        try {
          await api.deleteAccountSession(session.id);
          addToast(t('账号已删除'));
          await load();
        } catch (err) {
          setError(err instanceof Error ? err.message : String(err));
        }
      },
    });
  };

  const cooldownSummary = (session: AccountSessionItem): string => {
    const entries = Object.entries(session.cooldowns || {});
    if (entries.length === 0) return '—';
    return entries
      .slice(0, 3)
      .map(([model, secs]) => `${model} ${secs}s`)
      .join(' · ');
  };

  const successRate = (session: AccountSessionItem): string => {
    const ok = Number(session.success_count || 0);
    const failed = Number(session.error_count || 0);
    const total = ok + failed;
    if (total === 0) return '—';
    return `${((ok / total) * 100).toFixed(0)}%`;
  };

  const needle = search.trim().toLowerCase();
  const visible = sessions.filter((s) => {
    if (channelFilter && s.channel_id !== channelFilter) return false;
    if (!needle) return true;
    return (
      s.name.toLowerCase().includes(needle) ||
      (s.provider || '').toLowerCase().includes(needle) ||
      (s.base_url || '').toLowerCase().includes(needle)
    );
  });

  if (loading) return <SkeletonTable columns={7} rows={5} />;

  return (
    <div>
      <div className="page-header">
        <h1>{t('账号池管理')}</h1>
        <p>
          {t(
            '为账号型渠道管理多把上游凭证：账号级优先级/权重/并发上限与按上游模型的冷却，失败自动轮换'
          )}
        </p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {accountChannels.length === 0 && (
        <div className="error-message">
          {t('还没有账号型渠道：请先在渠道管理里创建 channel_type = account 的渠道')}
        </div>
      )}

      {stats.length > 0 && (
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8, marginBottom: 12 }}>
          {stats.map((st) => (
            <span key={st.channel_id} className="badge badge-info">
              {st.channel_name || st.channel_id}: {st.enabled}/{st.total}
              {st.cooling > 0 ? ` · ${t('冷却中')} ${st.cooling}` : ''}
              {st.in_flight > 0 ? ` · ${t('并发')} ${st.in_flight}` : ''}
            </span>
          ))}
        </div>
      )}

      <Card
        title={`${t('账号池')} (${visible.length})`}
        actions={
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <Button variant="outline" onClick={openBulk} disabled={accountChannels.length === 0}>
              <Upload size={14} /> {t('批量导入')}
            </Button>
            <Button onClick={openCreate} disabled={accountChannels.length === 0}>
              <Plus size={14} /> {t('添加账号')}
            </Button>
          </div>
        }
      >
        <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', marginBottom: 12 }}>
          <Select
            value={channelFilter}
            onChange={(e) => setChannelFilter(e.target.value)}
            aria-label={t('选择渠道')}
          >
            <option value="">{t('全部渠道')}</option>
            {accountChannels.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </Select>
          <Input
            placeholder={t('搜索名称 / provider / 地址')}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            aria-label={t('搜索')}
          />
        </div>

        {visible.length === 0 ? (
          <EmptyState
            message={
              accountChannels.length === 0
                ? t('先创建账号型渠道，再来添加凭证')
                : t('暂无账号，点击「添加账号」或「批量导入」开始')
            }
            icon="🔑"
            action={
              accountChannels.length > 0 ? (
                <Button onClick={openCreate}>{t('添加账号')}</Button>
              ) : undefined
            }
          />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  <th>{t('名称')}</th>
                  <th>{t('渠道')}</th>
                  <th>{t('Provider')}</th>
                  <th>{t('状态')}</th>
                  <th>{t('优先级')}</th>
                  <th>{t('并发')}</th>
                  <th>{t('成功率')}</th>
                  <th>{t('冷却')}</th>
                  <th>{t('操作')}</th>
                </tr>
              </thead>
              <tbody>
                {visible.map((session) => (
                  <tr key={session.id}>
                    <td>
                      <div>{session.name}</div>
                      <div className="form-hint">{session.api_key || '—'}</div>
                      {session.proxy && <div className="form-hint">{session.proxy}</div>}
                    </td>
                    <td>{session.channel_name || session.channel_id}</td>
                    <td>
                      <div>{session.provider}</div>
                      {session.base_url && <div className="form-hint">{session.base_url}</div>}
                    </td>
                    <td>
                      <span className={`badge ${statusClass(session.status)}`}>
                        {session.status}
                      </span>
                      {(session.in_flight ?? 0) > 0 && (
                        <div className="form-hint">
                          {t('进行中')} {session.in_flight}
                        </div>
                      )}
                    </td>
                    <td>
                      {session.priority ?? 0} / w{session.weight ?? 1}
                    </td>
                    <td>
                      {session.max_concurrency
                        ? `${session.in_flight ?? 0}/${session.max_concurrency}`
                        : t('不限')}
                    </td>
                    <td>{successRate(session)}</td>
                    <td>{cooldownSummary(session)}</td>
                    <td>
                      <div className="actions-cell">
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => handleTest(session)}
                          disabled={testingId === session.id}
                        >
                          <FlaskConical size={14} />
                          {testingId === session.id ? t('测试中…') : t('测试')}
                        </Button>
                        <Button variant="outline" size="sm" onClick={() => openEdit(session)}>
                          <Pencil size={14} />
                        </Button>
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => handleResetCooldowns(session)}
                          disabled={Object.keys(session.cooldowns || {}).length === 0}
                        >
                          <RotateCcw size={14} />
                        </Button>
                        <Button variant="danger" size="sm" onClick={() => handleDelete(session)}>
                          <Trash2 size={14} />
                        </Button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      {showModal && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={handleSave}>
            <div className="modal-header">
              <h3>{editing ? t('编辑账号') : t('添加账号')}</h3>
              <button type="button" className="modal-close" onClick={closeModal}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <Select
                label={t('渠道')}
                value={form.channel_id}
                onChange={(e) => setForm({ ...form, channel_id: e.target.value })}
                disabled={Boolean(editing)}
              >
                {accountChannels.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </Select>
              <Input
                label={t('账号名（可选）')}
                placeholder="account-1"
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
              />
              <Select
                label={t('Provider')}
                value={form.provider}
                onChange={(e) => setForm({ ...form, provider: e.target.value })}
              >
                {providers.map((p) => (
                  <option key={p.kind} value={p.kind}>
                    {p.label}
                  </option>
                ))}
              </Select>
              <Input
                label={t('上游地址（可选，留空用 provider 默认）')}
                placeholder="https://api.deepseek.com"
                value={form.base_url}
                onChange={(e) => setForm({ ...form, base_url: e.target.value })}
              />
              <Input
                label={t('凭证')}
                type="password"
                placeholder={editing ? t('留空表示不修改') : 'sk-...'}
                hint={editing ? t('留空保持原凭证；勾选下方可清除') : undefined}
                value={form.api_key}
                onChange={(e) => setForm({ ...form, api_key: e.target.value })}
              />
              {editing && (
                <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <input
                    type="checkbox"
                    checked={form.clear_key}
                    onChange={(e) => setForm({ ...form, clear_key: e.target.checked })}
                  />
                  <span>{t('清除现有凭证')}</span>
                </label>
              )}
              <Input
                label={t('出口代理（可选）')}
                placeholder="socks5://127.0.0.1:1080"
                value={form.proxy}
                onChange={(e) => setForm({ ...form, proxy: e.target.value })}
              />
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
                <Input
                  label={t('优先级')}
                  type="number"
                  value={form.priority}
                  onChange={(e) => setForm({ ...form, priority: e.target.value })}
                />
                <Input
                  label={t('权重')}
                  type="number"
                  min="1"
                  value={form.weight}
                  onChange={(e) => setForm({ ...form, weight: e.target.value })}
                />
              </div>
              <Input
                label={t('并发上限')}
                type="number"
                min="0"
                hint={t('(0 = 不限)')}
                value={form.max_concurrency}
                onChange={(e) => setForm({ ...form, max_concurrency: e.target.value })}
              />
              <Input
                label={t('模型白名单（可选，逗号分隔）')}
                placeholder="deepseek-chat, deepseek-reasoner"
                value={form.models}
                onChange={(e) => setForm({ ...form, models: e.target.value })}
              />
              <Input
                label={t('凭证到期时间（可选）')}
                type="number"
                hint={t('(留空则永不过期)')}
                value={form.expires_at}
                onChange={(e) => setForm({ ...form, expires_at: e.target.value })}
              />
              <Select
                label={t('状态')}
                value={form.status}
                onChange={(e) => setForm({ ...form, status: e.target.value })}
              >
                <option value="enabled">{t('启用')}</option>
                <option value="disabled">{t('停用')}</option>
              </Select>
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={closeModal}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('保存中…') : t('保存修改')}
              </Button>
            </div>
          </form>
        </div>
      )}

      {showBulk && (
        <div className="modal-overlay">
          <form className="modal" onSubmit={handleBulk}>
            <div className="modal-header">
              <h3>{t('批量导入')}</h3>
              <button type="button" className="modal-close" onClick={() => setShowBulk(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <Select
                label={t('渠道')}
                value={bulk.channel_id}
                onChange={(e) => setBulk({ ...bulk, channel_id: e.target.value })}
              >
                {accountChannels.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </Select>
              <Select
                label={t('Provider')}
                value={bulk.provider}
                onChange={(e) => setBulk({ ...bulk, provider: e.target.value })}
              >
                {providers.map((p) => (
                  <option key={p.kind} value={p.kind}>
                    {p.label}
                  </option>
                ))}
              </Select>
              <Input
                label={t('上游地址（可选）')}
                placeholder="https://api.deepseek.com"
                value={bulk.base_url}
                onChange={(e) => setBulk({ ...bulk, base_url: e.target.value })}
              />
              <Input
                label={t('名称前缀')}
                value={bulk.name_prefix}
                onChange={(e) => setBulk({ ...bulk, name_prefix: e.target.value })}
              />
              <Input
                label={t('出口代理（可选）')}
                placeholder="socks5://127.0.0.1:1080"
                value={bulk.proxy}
                onChange={(e) => setBulk({ ...bulk, proxy: e.target.value })}
              />
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 12 }}>
                <Input
                  label={t('优先级')}
                  type="number"
                  value={bulk.priority}
                  onChange={(e) => setBulk({ ...bulk, priority: e.target.value })}
                />
                <Input
                  label={t('权重')}
                  type="number"
                  min="1"
                  value={bulk.weight}
                  onChange={(e) => setBulk({ ...bulk, weight: e.target.value })}
                />
              </div>
              <Input
                label={t('并发上限')}
                type="number"
                min="0"
                hint={t('(0 = 不限)')}
                value={bulk.max_concurrency}
                onChange={(e) => setBulk({ ...bulk, max_concurrency: e.target.value })}
              />
              <Input
                label={t('模型白名单（可选）')}
                placeholder="deepseek-chat"
                value={bulk.models}
                onChange={(e) => setBulk({ ...bulk, models: e.target.value })}
              />
              <Textarea
                label={t('凭证列表')}
                rows={8}
                placeholder={'sk-aaa\nname2,sk-bbb\n# 注释行会被忽略'}
                hint={t('每行一条：Name,Key 或仅 Key；# 开头为注释')}
                value={bulk.credentials}
                onChange={(e) => setBulk({ ...bulk, credentials: e.target.value })}
              />
            </div>
            <div className="modal-footer">
              <Button variant="secondary" onClick={() => setShowBulk(false)}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={saving}>
                {saving ? t('导入中…') : t('开始导入')}
              </Button>
            </div>
          </form>
        </div>
      )}

      <ConfirmDialog
        state={confirmState}
        onClose={() => setConfirmState(null)}
      />
    </div>
  );
}
