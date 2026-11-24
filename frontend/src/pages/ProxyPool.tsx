/**
 * ProxyPool — 网络代理页（对照 Agent2API「网络代理」页 + ds2api 代理管理）。
 *
 * 集中管理一组命名代理（http/https/socks5），账号池的 proxy 字段用
 * `pool:<id>` / `rotate` / `rotate:<id>,<id>` 编码引用它们（改一处、
 * 多账号生效）；每条可独立做连通性测试（结果记入条目，重启后仍可见）。
 */
import { useCallback, useEffect, useMemo, useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Globe, Loader2, Pencil, Plus, RefreshCw, Search, SlidersHorizontal, Trash2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, EmptyState, Input, Loading, Select, Switch, Tabs } from '../components/ui';
import type { ProxyPoolItem, ProxyTestResult } from '../types';
import './ProxyPool.css';

/** 新建 / 编辑表单形状 */
interface EntryForm {
  id: string;
  name: string;
  protocol: string;
  host: string;
  port: string;
  username: string;
  /** 编辑时留空 = 保留旧密码（后端口径） */
  password: string;
}

const EMPTY_FORM: EntryForm = {
  id: '',
  name: '',
  protocol: 'http',
  host: '',
  port: '',
  username: '',
  password: '',
};

/** 状态筛选口径（对齐 Agent2API proxies-page.tsx 的 `stateOf`） */
type ProxyStateFilter = 'all' | 'ok' | 'fail' | 'none' | 'off';

function proxyState(item: ProxyPoolItem): Exclude<ProxyStateFilter, 'all'> {
  if (!item.enabled) return 'off';
  const last = item.lastTest ?? null;
  if (!last || !last.tested_at) return 'none';
  return last.success ? 'ok' : 'fail';
}

function fmtTestedAt(value: number | null | undefined): string {
  if (!value || !Number.isFinite(value)) return '—';
  const ms = value < 1e11 ? value * 1000 : value;
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return '—';
  }
}

export default function ProxyPool(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [items, setItems] = useState<ProxyPoolItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [showForm, setShowForm] = useState(false);
  const [form, setForm] = useState<EntryForm>(EMPTY_FORM);
  const [saving, setSaving] = useState(false);
  const [testingIds, setTestingIds] = useState<Set<string>>(new Set());
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  // 连通性测试设置（A2A GET/POST /api/proxies/settings）
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsUrl, setSettingsUrl] = useState('');
  const [settingsDefault, setSettingsDefault] = useState('');
  const [settingsSaving, setSettingsSaving] = useState(false);
  const [settingsError, setSettingsError] = useState('');
  // 筛选（A2A 代理池页的 搜索 / 协议 / 状态 三件套）：纯前端派生，不改接口
  const [keyword, setKeyword] = useState('');
  const [protocolFilter, setProtocolFilter] = useState('all');
  const [stateFilter, setStateFilter] = useState<ProxyStateFilter>('all');
  const [syncingClash, setSyncingClash] = useState(false);

  const load = useCallback(async () => {
    try {
      const res = await api.listProxyPool();
      setItems((res as { items?: ProxyPoolItem[] })?.items ?? []);
    } catch (err) {
      addToast((err as Error).message || t('加载代理池失败'), 'error');
    } finally {
      setLoading(false);
    }
  }, [addToast, t]);

  useEffect(() => {
    load();
  }, [load]);

  const openCreate = () => {
    setForm(EMPTY_FORM);
    setShowForm(true);
  };

  const openSettings = async () => {
    setSettingsOpen(true);
    setSettingsError('');
    try {
      const res = await api.proxySettings();
      const data = res?.data;
      setSettingsUrl(data?.testUrl ?? '');
      setSettingsDefault(data?.defaults?.testUrl ?? '');
    } catch (err) {
      setSettingsError((err as Error).message || t('加载代理设置失败'));
    }
  };

  const saveSettings = async (e: FormEvent) => {
    e.preventDefault();
    const url = settingsUrl.trim();
    if (url && !/^https?:\/\//i.test(url)) {
      setSettingsError(t('测试网址要以 http:// 或 https:// 开头'));
      return;
    }
    setSettingsSaving(true);
    setSettingsError('');
    try {
      const res = await api.saveProxySettings({ testUrl: url });
      setSettingsUrl(res?.data?.testUrl ?? '');
      addToast(t('连通性测试设置已保存'), 'success');
      setSettingsOpen(false);
    } catch (err) {
      setSettingsError((err as Error).message || t('保存失败'));
    } finally {
      setSettingsSaving(false);
    }
  };

  const openEdit = (entry: ProxyPoolItem) => {
    setForm({
      id: entry.id,
      name: entry.name,
      protocol: entry.protocol || 'http',
      host: entry.host,
      port: String(entry.port ?? ''),
      username: entry.username ?? '',
      // 密码不回传：留空提交 = 保留旧值（表单提示已说明）
      password: '',
    });
    setShowForm(true);
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setSaving(true);
    try {
      const payload: Record<string, unknown> = {
        id: form.id || undefined,
        name: form.name.trim(),
        protocol: form.protocol,
        host: form.host.trim(),
        port: Number(form.port),
        username: form.username.trim(),
        password: form.password,
      };
      const res = form.id ? await api.updateProxyEntry(payload) : await api.createProxyEntry(payload);
      const data = res as unknown as { items?: ProxyPoolItem[] };
      setItems(data?.items ?? []);
      setShowForm(false);
      addToast(form.id ? t('代理已更新') : t('代理已创建'), 'success');
    } catch (err) {
      addToast((err as Error).message || t('保存失败'), 'error');
    } finally {
      setSaving(false);
    }
  };

  const toggleEnabled = async (entry: ProxyPoolItem) => {
    try {
      const res = await api.enableProxyEntry(entry.id, !entry.enabled);
      const data = res as unknown as { items?: ProxyPoolItem[] };
      setItems(data?.items ?? []);
    } catch (err) {
      addToast((err as Error).message || t('操作失败'), 'error');
    }
  };

  const testEntry = async (entry: ProxyPoolItem) => {
    setTestingIds((prev) => new Set(prev).add(entry.id));
    try {
      const res = await api.testProxyEntry(entry.id);
      const result = res as unknown as ProxyTestResult & { items?: ProxyPoolItem[] };
      if (result.items) setItems(result.items);
      if (result.success) {
        addToast(t('出口可用') + (result.ip ? `（IP ${result.ip}）` : ''), 'success');
      } else {
        addToast(`${t('测试失败')}：${result.error ?? ''}`, 'error');
      }
    } catch (err) {
      addToast((err as Error).message || t('测试失败'), 'error');
    } finally {
      setTestingIds((prev) => {
        const next = new Set(prev);
        next.delete(entry.id);
        return next;
      });
    }
  };

  const removeEntry = (entry: ProxyPoolItem) => {
    setConfirmState({
      title: t('删除代理'),
      message: t('确认删除代理「{{name}}」？引用它的账号会解析失败并回退报错。', { name: entry.name }),
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        try {
          const res = await api.removeProxyEntry(entry.id);
          const data = res as unknown as { items?: ProxyPoolItem[] };
          setItems(data?.items ?? []);
          addToast(t('已删除'), 'success');
        } catch (err) {
          addToast((err as Error).message || t('删除失败'), 'error');
        } finally {
          setConfirmState(null);
        }
      },
    });
  };

  const copyReference = async (entry: ProxyPoolItem) => {
    try {
      await navigator.clipboard.writeText(`pool:${entry.id}`);
      addToast(t('已复制引用编码 pool:{{id}}', { id: entry.id }), 'success');
    } catch {
      addToast(`pool:${entry.id}`, 'warning');
    }
  };

  const testing = (id: string) => testingIds.has(id);

  /**
   * 手动同步 Clash Verge 出口（对齐 A2A「同步 EasyClash」）。
   *
   * 后端 `POST /api/proxy-pool/sync-clash` 回整份池 + `changes`（本次变更条数）
   * + `syncError`（Clash 没装 / 配置解析失败的说明，null = 成功）。同步失败时
   * 池本身不变，所以只提示原因、不改列表。
   */
  const syncClash = async (): Promise<void> => {
    setSyncingClash(true);
    try {
      const res = await api.syncClashProxyPool();
      setItems(res?.items ?? []);
      if (res?.syncError) addToast(`${t('同步失败')}：${res.syncError}`, 'error');
      else addToast(t('已同步 {{n}} 条变更', { n: res?.changes ?? 0 }), 'success');
    } catch (err) {
      addToast((err as Error).message || t('同步失败'), 'error');
    } finally {
      setSyncingClash(false);
    }
  };

  /** 协议候选：只列池里真出现过的协议，避免筛出一个恒空的选项 */
  const protocols = useMemo(
    () => [...new Set(items.map((item) => item.protocol || 'http'))].sort(),
    [items],
  );

  const stateCounts = useMemo(() => {
    const counts: Record<Exclude<ProxyStateFilter, 'all'>, number> = { ok: 0, fail: 0, none: 0, off: 0 };
    for (const item of items) counts[proxyState(item)] += 1;
    return counts;
  }, [items]);

  const shown = useMemo(() => {
    const query = keyword.trim().toLowerCase();
    return items.filter((item) => {
      if (protocolFilter !== 'all' && (item.protocol || 'http') !== protocolFilter) return false;
      if (stateFilter !== 'all' && proxyState(item) !== stateFilter) return false;
      if (!query) return true;
      return [item.name, item.host, item.protocol, String(item.port), item.username ?? '']
        .join(' ')
        .toLowerCase()
        .includes(query);
    });
  }, [items, keyword, protocolFilter, stateFilter]);

  const filtersActive = keyword.trim() !== '' || protocolFilter !== 'all' || stateFilter !== 'all';
  const clearFilters = (): void => {
    setKeyword('');
    setProtocolFilter('all');
    setStateFilter('all');
  };

  const stateOptions: Array<{ value: ProxyStateFilter; label: string; count: number }> = [
    { value: 'all', label: t('全部'), count: items.length },
    { value: 'ok', label: t('可用'), count: stateCounts.ok },
    { value: 'fail', label: t('失败'), count: stateCounts.fail },
    { value: 'none', label: t('未检测'), count: stateCounts.none },
    { value: 'off', label: t('已禁用'), count: stateCounts.off },
  ];

  return (
    <div className="page-container">
      <div className="page-header">
        <div>
          <h1 className="page-title">{t('网络代理')}</h1>
          <p className="page-description">
            {t('集中管理出口代理：账号池的代理字段填 pool:<id> 引用单条，或填 rotate / rotate:<id>,<id> 轮询换出口 IP')}
          </p>
        </div>
        <div className="page-actions">
          <Button variant="secondary" onClick={openSettings}>
            <SlidersHorizontal size={16} /> {t('连通性测试设置')}
          </Button>
          <Button variant="secondary" onClick={() => void syncClash()} disabled={syncingClash}>
            {syncingClash ? <Loader2 size={16} className="spin" /> : <RefreshCw size={16} />} {t('同步 EasyClash')}
          </Button>
          <Button variant="secondary" onClick={load}>
            <RefreshCw size={16} /> {t('刷新')}
          </Button>
          <Button onClick={openCreate}>
            <Plus size={16} /> {t('新建代理')}
          </Button>
        </div>
      </div>

      {settingsOpen && (
        <Card className="proxy-form-card">
          <h2 className="proxy-form-title">{t('连通性测试设置')}</h2>
          <form onSubmit={saveSettings} className="proxy-form">
            <Input
              label={t('测试目标网址')}
              placeholder={settingsDefault}
              value={settingsUrl}
              onChange={(e) => setSettingsUrl(e.target.value)}
            />
            <span className="form-hint">
              {t('通过代理访问该网址，检查是否能收到 HTTP 响应。留空沿用网关默认值。')}
            </span>
            {settingsError && (
              <span className="form-hint" style={{ color: 'var(--danger, #dc2626)' }}>❌ {settingsError}</span>
            )}
            <div className="proxy-form-actions">
              <Button
                type="button"
                variant="secondary"
                style={{ marginRight: 'auto' }}
                onClick={() => { setSettingsUrl(''); setSettingsError(''); }}
              >
                {t('恢复默认')}
              </Button>
              <Button type="button" variant="secondary" onClick={() => setSettingsOpen(false)}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={settingsSaving}>
                {settingsSaving && <Loader2 size={16} className="spin" />} {t('保存设置')}
              </Button>
            </div>
          </form>
        </Card>
      )}

      {/* 筛选栏（A2A 代理池页：搜索 / 协议 / 状态 三段；纯前端派生，接口不变） */}
      {!loading && items.length > 0 ? (
        <Card className="proxy-form-card">
          <div className="proxy-form-row" style={{ alignItems: 'flex-end', flexWrap: 'wrap' }}>
            <label className="form-group" style={{ minWidth: 220, flex: '1 1 220px' }}>
              <span>{t('搜索代理')}</span>
              <span style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                <Search size={14} aria-hidden="true" />
                <input
                  className="form-input"
                  type="search"
                  value={keyword}
                  placeholder={t('搜索名称 / 地址 / 端口')}
                  aria-label={t('搜索代理')}
                  onChange={(e) => setKeyword(e.target.value)}
                />
              </span>
            </label>
            <div className="form-group" style={{ minWidth: 140 }}>
              <label htmlFor="proxyProtocolFilter">{t('按协议筛选')}</label>
              <Select
                id="proxyProtocolFilter"
                value={protocolFilter}
                onChange={(e) => setProtocolFilter(e.target.value)}
              >
                <option value="all">{t('全部协议')}</option>
                {protocols.map((protocol) => (
                  <option key={protocol} value={protocol}>
                    {protocol.toUpperCase()}
                  </option>
                ))}
              </Select>
            </div>
            <div className="form-group" style={{ flex: '1 1 320px' }}>
              <label>{t('按状态筛选')}</label>
              <Tabs<ProxyStateFilter>
                ariaLabel={t('按状态筛选')}
                items={stateOptions.map((option) => ({
                  key: option.value,
                  label: `${option.label}（${option.count}）`,
                }))}
                active={stateFilter}
                onChange={(key) => setStateFilter(key)}
              />
            </div>
            <Button
              type="button"
              variant="secondary"
              size="sm"
              disabled={!filtersActive}
              onClick={clearFilters}
            >
              {t('清空筛选')}
            </Button>
          </div>
        </Card>
      ) : null}

      {showForm && (
        <Card className="proxy-form-card">
          <h2 className="proxy-form-title">{form.id ? t('编辑代理') : t('新建代理')}</h2>
          <form onSubmit={submit} className="proxy-form">
            <div className="proxy-form-row">
              <Input
                label={t('名称')}
                placeholder={t('如：香港节点')}
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                required
              />
              <div className="form-group">
                <label>{t('协议')}</label>
                <Select
                  value={form.protocol}
                  onChange={(e) => setForm({ ...form, protocol: e.target.value })}
                >
                  <option value="http">http</option>
                  <option value="https">https</option>
                  <option value="socks5">socks5</option>
                  <option value="socks5h">socks5h</option>
                </Select>
                <span className="form-hint">
                  {t('socks5 与 socks5h 均可：socks5 会按 socks5h（DNS 在代理端解析）处理')}
                </span>
              </div>
            </div>
            <div className="proxy-form-row">
              <Input
                label={t('主机')}
                placeholder="127.0.0.1"
                value={form.host}
                onChange={(e) => setForm({ ...form, host: e.target.value })}
                required
              />
              <Input
                label={t('端口')}
                placeholder="8080"
                type="number"
                min={1}
                max={65535}
                value={form.port}
                onChange={(e) => setForm({ ...form, port: e.target.value })}
                required
              />
            </div>
            <div className="proxy-form-row">
              <Input
                label={t('用户名（可选）')}
                value={form.username}
                onChange={(e) => setForm({ ...form, username: e.target.value })}
              />
              <Input
                label={t('密码（可选）')}
                type="password"
                hint={form.id ? t('留空保留当前密码') : undefined}
                value={form.password}
                onChange={(e) => setForm({ ...form, password: e.target.value })}
              />
            </div>
            <div className="proxy-form-actions">
              <Button type="button" variant="secondary" onClick={() => setShowForm(false)}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={saving}>
                {saving && <Loader2 size={16} className="spin" />} {t('保存')}
              </Button>
            </div>
          </form>
        </Card>
      )}

      {loading ? (
        <Loading />
      ) : items.length === 0 ? (
        <EmptyState
          icon="🌐"
          message={t('还没有代理')}
        />
      ) : shown.length === 0 ? (
        <EmptyState
          icon="🌐"
          message={t('没有匹配当前筛选的出口')}
          action={
            <Button variant="secondary" size="sm" onClick={clearFilters}>
              {t('清空筛选')}
            </Button>
          }
        />
      ) : (
        <div className="proxy-grid">
          {shown.map((entry) => {
            const last = entry.lastTest ?? null;
            const fromClash = entry.source === 'clash';
            return (
              <Card key={entry.id} className={`proxy-card${entry.enabled ? '' : ' proxy-card-disabled'}`}>
                <div className="proxy-card-head">
                  <div className="proxy-card-title">
                    <span className="proxy-card-name">{entry.name}</span>
                    <span className="proxy-card-addr">
                      {entry.protocol}://{entry.host}:{entry.port}
                      {entry.username && ` · ${entry.username}`}
                    </span>
                    {fromClash ? <span className="proxy-usedby">{t('只读：在 EasyClash 里改')}</span> : null}
                  </div>
                  <Switch
                    checked={entry.enabled}
                    onChange={() => toggleEnabled(entry)}
                    ariaLabel={t('启用 {{name}}', { name: entry.name })}
                  />
                </div>
                {/* 最近检测：时间 + 耗时 + 出口 IP / 失败原因分开显示。
                    时间永远在前——长失败原因不该把「什么时候测的」挤掉。 */}
                {last && last.tested_at ? (
                  <div className={`proxy-test-result${last.success ? ' ok' : ' bad'}`}>
                    <span>
                      {last.success
                        ? `✅ ${t('可用')} ${last.ip || '—'} · ${last.duration_ms}ms`
                        : `❌ ${t('失败')}：${last.error ?? t('连接失败')}`}
                    </span>
                    <span className="proxy-card-addr">{t('检测于 {{time}}', { time: fmtTestedAt(last.tested_at) })}</span>
                  </div>
                ) : (
                  <div className="proxy-test-result">
                    {t('未检测')}
                  </div>
                )}
                {entry.usedBy && entry.usedBy.length > 0 && (
                  <div className="proxy-usedby">
                    {t('{{count}} 个账号引用', { count: entry.usedBy.length })}
                  </div>
                )}
                <div className="proxy-card-actions">
                  <Button variant="secondary" size="sm" onClick={() => testEntry(entry)} disabled={testing(entry.id)}>
                    {testing(entry.id) ? <Loader2 size={14} className="spin" /> : <Globe size={14} />} {t('测试')}
                  </Button>
                  <Button variant="secondary" size="sm" onClick={() => copyReference(entry)}>
                    pool:{entry.id.slice(0, 10)}…
                  </Button>
                  {fromClash ? null : (
                    <Button variant="secondary" size="sm" onClick={() => openEdit(entry)}>
                      <Pencil size={14} />
                    </Button>
                  )}
                  <Button variant="danger" size="sm" onClick={() => removeEntry(entry)}>
                    <Trash2 size={14} />
                  </Button>
                </div>
              </Card>
            );
          })}
        </div>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}
