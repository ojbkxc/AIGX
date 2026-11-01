/**
 * ProxyPool — 网络代理页（对照 Agent2API「网络代理」页 + ds2api 代理管理）。
 *
 * 集中管理一组命名代理（http/https/socks5），账号池的 proxy 字段用
 * `pool:<id>` / `rotate` / `rotate:<id>,<id>` 编码引用它们（改一处、
 * 多账号生效）；每条可独立做连通性测试（结果记入条目，重启后仍可见）。
 */
import { useCallback, useEffect, useState, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Globe, Loader2, Pencil, Plus, RefreshCw, Trash2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, EmptyState, Input, Loading, Select, Switch } from '../components/ui';
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
          <Button variant="secondary" onClick={load}>
            <RefreshCw size={16} /> {t('刷新')}
          </Button>
          <Button onClick={openCreate}>
            <Plus size={16} /> {t('新建代理')}
          </Button>
        </div>
      </div>

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
                </Select>
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
      ) : (
        <div className="proxy-grid">
          {items.map((entry) => {
            const last = entry.lastTest ?? null;
            return (
              <Card key={entry.id} className={`proxy-card${entry.enabled ? '' : ' proxy-card-disabled'}`}>
                <div className="proxy-card-head">
                  <div className="proxy-card-title">
                    <span className="proxy-card-name">{entry.name}</span>
                    <span className="proxy-card-addr">
                      {entry.protocol}://{entry.host}:{entry.port}
                      {entry.username && ` · ${entry.username}`}
                    </span>
                  </div>
                  <Switch
                    checked={entry.enabled}
                    onChange={() => toggleEnabled(entry)}
                    ariaLabel={t('启用 {{name}}', { name: entry.name })}
                  />
                </div>
                {last && (
                  <div className={`proxy-test-result${last.success ? ' ok' : ' bad'}`}>
                    {last.success
                      ? `✅ ${t('出口 IP')} ${last.ip || '—'} · ${last.duration_ms}ms`
                      : `❌ ${last.error ?? t('失败')}`}
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
                  <Button variant="secondary" size="sm" onClick={() => openEdit(entry)}>
                    <Pencil size={14} />
                  </Button>
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
