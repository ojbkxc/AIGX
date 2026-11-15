import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  FileClock, Loader2, Pencil, Plus, RefreshCw, RotateCcw, Save, Trash2, Upload,
} from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Badge, Button, EmptyState, Input, Loading, Switch, Tabs, Textarea } from '../components/ui';
import type {
  ClientConfigBackupsData,
  ClientConfigListData,
  ClientConfigPreviewData,
  ClientConfigRecord,
  ClientConfigTarget,
  ClientConfigTargetId,
} from '../types/client-config';
import './ClientConfig.css';

/** 毫秒时间戳 → 本地时间；0 显示占位。 */
function fmtTime(ms: number): string {
  if (!ms) return '—';
  return new Date(ms).toLocaleString();
}

/** 字节数 → 可读大小。 */
function fmtSize(bytes: number): string {
  if (!bytes) return '0 B';
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

/** 模型条目的 id（字符串或含 id 的对象）。 */
function modelIdOf(item: unknown): string {
  if (typeof item === 'string') return item.trim();
  if (item && typeof item === 'object' && 'id' in item) {
    const id = (item as { id?: unknown }).id;
    return typeof id === 'string' ? id.trim() : '';
  }
  return '';
}

function modelsOf(record: ClientConfigRecord): string {
  const ids = record.models.map(modelIdOf).filter(Boolean);
  if (ids.length === 0) return record.model || '—';
  return ids.join(', ');
}

function optionText(record: ClientConfigRecord, key: string): string {
  const value = record.options?.[key];
  return typeof value === 'string' ? value : '';
}

function optionBool(record: ClientConfigRecord, key: string, fallback: boolean): boolean {
  const value = record.options?.[key];
  return typeof value === 'boolean' ? value : fallback;
}

interface FormState {
  name: string;
  baseUrl: string;
  apiKey: string;
  model: string;
  modelsText: string;
  note: string;
  enabled: boolean;
  clearApiKey: boolean;
  route: string;
  api: string;
  wireApi: string;
  authField: string;
  effort: string;
  envKey: string;
  setDefault: boolean;
}

function emptyForm(): FormState {
  return {
    name: '',
    baseUrl: '',
    apiKey: '',
    model: '',
    modelsText: '',
    note: '',
    enabled: true,
    clearApiKey: false,
    route: '',
    api: '',
    wireApi: '',
    authField: '',
    effort: '',
    envKey: '',
    setDefault: true,
  };
}

function formFrom(record: ClientConfigRecord): FormState {
  return {
    name: record.name,
    baseUrl: record.baseUrl,
    apiKey: '',
    model: record.model,
    modelsText: record.models.map(modelIdOf).filter(Boolean).join('\n'),
    note: record.note,
    enabled: record.enabled,
    clearApiKey: false,
    route: optionText(record, 'route'),
    api: optionText(record, 'api'),
    wireApi: optionText(record, 'wireApi'),
    authField: optionText(record, 'authField'),
    effort: optionText(record, 'effort'),
    envKey: optionText(record, 'envKey'),
    setDefault: optionBool(record, 'setDefault', true),
  };
}

/** 表单 → 后端 options（只发送当前目标认识的键）。 */
function buildOptions(target: ClientConfigTargetId, form: FormState): Record<string, unknown> {
  const options: Record<string, unknown> = {};
  if (target === 'codex' || target === 'dsh') {
    if (form.route.trim()) options.route = form.route.trim();
  }
  if (target === 'codex') {
    if (form.wireApi) options.wireApi = form.wireApi;
    if (form.envKey.trim()) options.envKey = form.envKey.trim();
  }
  if (target === 'dsh') {
    if (form.api) options.api = form.api;
    options.setDefault = form.setDefault;
  }
  if (target === 'claude-code') {
    if (form.authField) options.authField = form.authField;
    if (form.effort.trim()) options.effort = form.effort.trim();
  }
  return options;
}

/** 目标格式 → 展示标签。 */
function formatLabel(format: string): string {
  if (format === 'toml') return 'TOML';
  if (format === 'yaml') return 'YAML';
  return 'JSON';
}

export default function ClientConfig(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [data, setData] = useState<ClientConfigListData | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState('');
  const [activeTarget, setActiveTarget] = useState<ClientConfigTargetId>('claude-code');
  const [confirm, setConfirm] = useState<ConfirmState | null>(null);

  const [formOpen, setFormOpen] = useState(false);
  const [editing, setEditing] = useState<ClientConfigRecord | null>(null);
  const [form, setForm] = useState<FormState>(emptyForm);
  const [formError, setFormError] = useState('');
  const [saving, setSaving] = useState(false);

  const [preview, setPreview] = useState<ClientConfigPreviewData | null>(null);
  const [previewLoadingId, setPreviewLoadingId] = useState('');
  const [applying, setApplying] = useState(false);

  const [backups, setBackups] = useState<ClientConfigBackupsData | null>(null);
  const [backupsLoading, setBackupsLoading] = useState(false);
  const [busyId, setBusyId] = useState('');

  const load = useCallback(async () => {
    try {
      setLoadError('');
      const res = await api.listClientConfigs();
      if (!res.data) throw new Error(res.error?.message || t('加载失败'));
      setData(res.data);
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error));
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    void load();
  }, [load]);

  const targets = useMemo<ClientConfigTarget[]>(() => data?.targets ?? [], [data]);
  const current: ClientConfigTarget | undefined = useMemo(
    () => targets.find((item) => item.id === activeTarget) ?? targets[0],
    [targets, activeTarget],
  );

  useEffect(() => {
    if (current && current.id !== activeTarget) setActiveTarget(current.id);
  }, [current, activeTarget]);

  const openCreate = (): void => {
    setEditing(null);
    setForm({ ...emptyForm(), enabled: current ? !current.configs.some((item) => item.enabled) : true });
    setFormError('');
    setFormOpen(true);
  };

  const openEdit = (record: ClientConfigRecord): void => {
    setEditing(record);
    setForm(formFrom(record));
    setFormError('');
    setFormOpen(true);
  };

  const submitForm = async (): Promise<void> => {
    if (!current) return;
    const name = form.name.trim();
    if (!name) {
      setFormError(t('请填写名称'));
      return;
    }
    if (!form.baseUrl.trim()) {
      setFormError(t('请填写接口地址'));
      return;
    }
    const models = form.modelsText
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
    setSaving(true);
    setFormError('');
    try {
      const options = buildOptions(current.id, form);
      if (editing) {
        await api.updateClientConfig(editing.id, {
          name,
          baseUrl: form.baseUrl.trim(),
          apiKey: form.apiKey.trim() || undefined,
          clearApiKey: form.clearApiKey || undefined,
          model: form.model.trim(),
          models,
          options,
          note: form.note.trim(),
          enabled: form.enabled,
        });
        addToast(t('已保存'));
      } else {
        await api.createClientConfig({
          target: current.id,
          name,
          baseUrl: form.baseUrl.trim(),
          apiKey: form.apiKey.trim() || undefined,
          model: form.model.trim(),
          models,
          options,
          note: form.note.trim(),
          enabled: form.enabled,
        });
        addToast(t('已新增'));
      }
      setFormOpen(false);
      await load();
    } catch (error) {
      setFormError(error instanceof Error ? error.message : String(error));
    } finally {
      setSaving(false);
    }
  };

  const toggleEnabled = async (record: ClientConfigRecord, next: boolean): Promise<void> => {
    setBusyId(record.id);
    try {
      await api.updateClientConfig(record.id, { enabled: next });
      addToast(next ? t('已启用') : t('已停用'));
      await load();
    } catch (error) {
      addToast(error instanceof Error ? error.message : String(error), 'error');
    } finally {
      setBusyId('');
    }
  };

  const removeRecord = (record: ClientConfigRecord): void => {
    setConfirm({
      title: t('删除配置'),
      message: (
        <span>
          {t('删除后不可恢复，确定继续？')} <strong>{record.name}</strong>
        </span>
      ),
      danger: true,
      confirmText: t('删除'),
      onConfirm: async () => {
        try {
          await api.deleteClientConfig(record.id);
          addToast(t('已删除'));
          await load();
        } catch (error) {
          addToast(error instanceof Error ? error.message : String(error), 'error');
        }
        setConfirm(null);
      },
    });
  };

  const openPreview = async (record: ClientConfigRecord): Promise<void> => {
    setPreviewLoadingId(record.id);
    try {
      const res = await api.previewClientConfig(record.id);
      if (!res.data) throw new Error(res.error?.message || t('加载失败'));
      setPreview(res.data);
    } catch (error) {
      addToast(error instanceof Error ? error.message : String(error), 'error');
    } finally {
      setPreviewLoadingId('');
    }
  };

  const applyPreview = async (): Promise<void> => {
    if (!preview) return;
    setApplying(true);
    try {
      const res = await api.applyClientConfig(preview.recordId, preview.token);
      addToast(res.data?.changed ? t('已写入客户端配置') : t('内容无变化，无需写入'));
      setPreview(null);
      await load();
    } catch (error) {
      addToast(error instanceof Error ? error.message : String(error), 'error');
    } finally {
      setApplying(false);
    }
  };

  const openBackups = async (record: ClientConfigRecord): Promise<void> => {
    setBackupsLoading(true);
    setBackups(null);
    try {
      const res = await api.getClientConfigBackups(record.id);
      if (!res.data) throw new Error(res.error?.message || t('加载失败'));
      setBackups(res.data);
    } catch (error) {
      addToast(error instanceof Error ? error.message : String(error), 'error');
    } finally {
      setBackupsLoading(false);
    }
  };

  const restoreBackup = (record: ClientConfigRecord, backupId: string, kind: string): void => {
    setConfirm({
      title: t('恢复备份'),
      message: (
        <span>
          {t('恢复备份会覆盖当前文件，确定继续？')} <strong>{backupId}</strong>
        </span>
      ),
      danger: true,
      confirmText: t('恢复'),
      onConfirm: async () => {
        try {
          await api.restoreClientConfig(record.id, backupId, kind === 'credentials' ? 'credentials' : 'config');
          addToast(t('已恢复备份'));
          await openBackups(record);
          await load();
        } catch (error) {
          addToast(error instanceof Error ? error.message : String(error), 'error');
        }
        setConfirm(null);
      },
    });
  };

  if (loading) return <Loading />;

  return (
    <div className="cc-page">
      <div className="page-header cc-header">
        <div>
          <h1>{t('客户端接入')}</h1>
          <p>
            {t('为 Claude Code / Codex / Claude Desktop / DSH 维护多份命名配置：预览差异、自动备份后写入本机配置文件。')}
          </p>
        </div>
        <div className="cc-header-actions">
          <Button variant="outline" onClick={() => void load()}>
            <RefreshCw size={14} /> {t('刷新')}
          </Button>
          <Button onClick={openCreate} disabled={!current}>
            <Plus size={14} /> {t('新增配置')}
          </Button>
        </div>
      </div>

      {loadError ? <div className="cc-error">{loadError}</div> : null}

      <Tabs<ClientConfigTargetId>
        items={targets.map((target) => ({
          key: target.id,
          label: (
            <span className="cc-tab-label">
              {target.label}
              <Badge>{target.configs.length}</Badge>
            </span>
          ),
        }))}
        active={current?.id ?? 'claude-code'}
        onChange={setActiveTarget}
        ariaLabel={t('客户端类型')}
      />

      {current ? (
        <>
          <div className="cc-pathbar">
            <span className="cc-path-label">{t('配置文件')}</span>
            {current.files.map((file) => (
              <code key={file.path} className="cc-path" title={file.path}>
                {file.path}
              </code>
            ))}
            <Badge>{formatLabel(current.format)}</Badge>
          </div>

          {current.configs.length === 0 ? (
            <EmptyState
              icon="📄"
              message={t('还没有配置。新建一份配置，填写接口地址与 API Key，预览确认后即可写入本机客户端。')}
              action={<Button onClick={openCreate}>{t('新增配置')}</Button>}
            />
          ) : (
            <div className="cc-list">
              {current.configs.map((record) => (
                <div key={record.id} className={`cc-item${record.enabled ? ' cc-item-active' : ''}`}>
                  <div className="cc-item-main">
                    <div className="cc-item-title">
                      <span className="cc-item-name">{record.name}</span>
                      {record.enabled ? <Badge>{t('已启用')}</Badge> : <Badge>{t('已停用')}</Badge>}
                      {record.hasApiKey ? null : <Badge>{t('无 Key')}</Badge>}
                      {record.lastAppliedAt ? (
                        <span className="cc-item-applied" title={fmtTime(record.lastAppliedAt)}>
                          {t('最近写入')}: {fmtTime(record.lastAppliedAt)}
                        </span>
                      ) : null}
                    </div>
                    <div className="cc-item-meta">
                      <span className="cc-mono">{record.baseUrl}</span>
                    </div>
                    <div className="cc-item-meta">
                      {t('模型')}: <span className="cc-mono">{modelsOf(record)}</span>
                      {record.note ? <> · {record.note}</> : null}
                    </div>
                  </div>
                  <div className="cc-item-actions">
                    <Switch
                      checked={record.enabled}
                      disabled={busyId === record.id}
                      onChange={(next) => void toggleEnabled(record, next)}
                      ariaLabel={t('是否启用')}
                    />
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={previewLoadingId === record.id}
                      onClick={() => void openPreview(record)}
                    >
                      {previewLoadingId === record.id ? <Loader2 size={13} className="cc-spin" /> : <Upload size={13} />}
                      {t('预览并写入')}
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => openEdit(record)}>
                      <Pencil size={13} /> {t('编辑')}
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => void openBackups(record)}>
                      <FileClock size={13} /> {t('备份')}
                    </Button>
                    <Button variant="danger" size="sm" onClick={() => removeRecord(record)}>
                      <Trash2 size={13} /> {t('删除')}
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </>
      ) : null}

      {/* ── 新增 / 编辑弹窗 ───────────────────────────────── */}
      {formOpen && current ? (
        <div className="modal-overlay">
          <form
            className="modal cc-modal"
            onSubmit={(event) => {
              event.preventDefault();
              void submitForm();
            }}
          >
            <div className="modal-header">
              <h3>{editing ? t('编辑配置') : t('新增配置')} · {current.label}</h3>
              <button type="button" className="modal-close" onClick={() => setFormOpen(false)}>
                &times;
              </button>
            </div>
            <div className="modal-body cc-form">
              {formError ? <div className="cc-error">{formError}</div> : null}
              <Input
                label={t('配置名称')}
                value={form.name}
                onChange={(event) => setForm({ ...form, name: event.target.value })}
                placeholder={t('例如：主线路 / 备用线路')}
                maxLength={200}
              />
              <Input
                label={t('接口地址')}
                value={form.baseUrl}
                onChange={(event) => setForm({ ...form, baseUrl: event.target.value })}
                placeholder="https://gateway.example.com"
              />
              <Input
                label="API Key"
                type="password"
                value={form.apiKey}
                onChange={(event) => setForm({ ...form, apiKey: event.target.value })}
                hint={editing ? t('留空表示不修改') : t('凭据加密落库；写入时按目标格式落到对应文件')}
                autoComplete="new-password"
              />
              {editing ? (
                <label className="cc-check">
                  <input
                    type="checkbox"
                    checked={form.clearApiKey}
                    onChange={(event) => setForm({ ...form, clearApiKey: event.target.checked })}
                  />
                  {t('清除已存凭据')}
                </label>
              ) : null}
              <Input
                label={t('模型 ID')}
                value={form.model}
                onChange={(event) => setForm({ ...form, model: event.target.value })}
                hint={t('可选；作为默认模型')}
              />
              <Textarea
                label={t('模型列表')}
                rows={3}
                value={form.modelsText}
                onChange={(event) => setForm({ ...form, modelsText: event.target.value })}
                hint={t('每行一个模型 ID')}
              />
              <Input
                label={t('备注（可选）')}
                value={form.note}
                onChange={(event) => setForm({ ...form, note: event.target.value })}
              />
              <label className="cc-check">
                <input
                  type="checkbox"
                  checked={form.enabled}
                  onChange={(event) => setForm({ ...form, enabled: event.target.checked })}
                />
                {t('启用这份配置（同一目标只保留一份启用）')}
              </label>

              <details className="cc-advanced">
                <summary>{t('高级选项')}</summary>
                <div className="cc-advanced-body">
                  {current.id === 'codex' || current.id === 'dsh' ? (
                    <Input
                      label={t('路由名')}
                      value={form.route}
                      onChange={(event) => setForm({ ...form, route: event.target.value })}
                      placeholder="aigx"
                      hint={t('provider 键名（小写字母/数字/-/_），默认 aigx')}
                    />
                  ) : null}
                  {current.id === 'codex' ? (
                    <>
                      <label className="cc-field">
                        <span>{t('协议格式')}</span>
                        <select
                          className="form-input"
                          value={form.wireApi}
                          onChange={(event) => setForm({ ...form, wireApi: event.target.value })}
                        >
                          <option value="">{t('默认（responses）')}</option>
                          <option value="responses">responses</option>
                          <option value="chat">chat</option>
                        </select>
                      </label>
                      <Input
                        label={t('环境变量名')}
                        value={form.envKey}
                        onChange={(event) => setForm({ ...form, envKey: event.target.value })}
                        placeholder="OPENAI_API_KEY"
                        hint={t('可选；填写后 Codex 从环境变量读取 Key，不写密钥')}
                      />
                    </>
                  ) : null}
                  {current.id === 'dsh' ? (
                    <>
                      <label className="cc-field">
                        <span>{t('协议')}</span>
                        <select
                          className="form-input"
                          value={form.api}
                          onChange={(event) => setForm({ ...form, api: event.target.value })}
                        >
                          <option value="">openai-completions</option>
                          <option value="openai-completions">openai-completions</option>
                          <option value="openai-responses">openai-responses</option>
                          <option value="anthropic-messages">anthropic-messages</option>
                        </select>
                      </label>
                      <label className="cc-check">
                        <input
                          type="checkbox"
                          checked={form.setDefault}
                          onChange={(event) => setForm({ ...form, setDefault: event.target.checked })}
                        />
                        {t('同时设为 DSH 默认模型提供商')}
                      </label>
                    </>
                  ) : null}
                  {current.id === 'claude-code' ? (
                    <>
                      <label className="cc-field">
                        <span>{t('凭据字段')}</span>
                        <select
                          className="form-input"
                          value={form.authField}
                          onChange={(event) => setForm({ ...form, authField: event.target.value })}
                        >
                          <option value="">ANTHROPIC_AUTH_TOKEN</option>
                          <option value="token">ANTHROPIC_AUTH_TOKEN</option>
                          <option value="key">ANTHROPIC_API_KEY</option>
                        </select>
                      </label>
                      <Input
                        label={t('思考等级')}
                        value={form.effort}
                        onChange={(event) => setForm({ ...form, effort: event.target.value })}
                        placeholder="high"
                        hint={t('可选；写入 CLAUDE_CODE_EFFORT_LEVEL')}
                      />
                    </>
                  ) : null}
                  {current.id === 'claude-desktop' ? (
                    <div className="form-hint">
                      {t('Claude Desktop 仅更新 env；mcpServers 等其余设置原样保留。')}
                    </div>
                  ) : null}
                </div>
              </details>
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setFormOpen(false)} disabled={saving}>
                {t('取消')}
              </Button>
              <Button type="submit" disabled={saving}>
                {saving ? <Loader2 size={14} className="cc-spin" /> : <Save size={14} />} {t('保存')}
              </Button>
            </div>
          </form>
        </div>
      ) : null}

      {/* ── 预览弹窗 ──────────────────────────────────────── */}
      {preview ? (
        <div className="modal-overlay">
          <div className="modal cc-preview-modal">
            <div className="modal-header">
              <h3>
                {t('写入预览')} · {preview.record.name}
              </h3>
              <button type="button" className="modal-close" onClick={() => setPreview(null)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              {preview.warnings.length > 0 ? (
                <ul className="cc-warnings">
                  {preview.warnings.map((warning) => (
                    <li key={warning}>{warning}</li>
                  ))}
                </ul>
              ) : null}
              {preview.files.map((file) => (
                <div key={`${file.kind}-${file.path}`} className="cc-fileplan">
                  <div className="cc-fileplan-head">
                    <code className="cc-path">{file.path}</code>
                    {file.changed ? <Badge>{t('有变更')}</Badge> : <Badge>{t('无变化')}</Badge>}
                    {file.created ? <Badge>{t('新建文件')}</Badge> : null}
                  </div>
                  <div className="cc-diff">
                    {file.diff.map((line, index) => (
                      <div key={`${index}-${line.kind}`} className={`cc-diff-line cc-diff-${line.kind}`}>
                        <span className="cc-diff-sign">
                          {line.kind === 'added' ? '+' : line.kind === 'removed' ? '-' : ' '}
                        </span>
                        <span className="cc-diff-text">{line.text || ' '}</span>
                      </div>
                    ))}
                  </div>
                  <details className="cc-fulltext">
                    <summary>{t('写入后完整内容（已脱敏）')}</summary>
                    <pre>{file.after}</pre>
                  </details>
                </div>
              ))}
              {!preview.hasChanges ? (
                <div className="cc-hint">{t('目标文件已是这份配置的内容，无需写入。')}</div>
              ) : null}
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setPreview(null)} disabled={applying}>
                {t('取消')}
              </Button>
              <Button
                onClick={() => void applyPreview()}
                disabled={applying || !preview.hasChanges}
              >
                {applying ? <Loader2 size={14} className="cc-spin" /> : <Upload size={14} />} {t('确认写入')}
              </Button>
            </div>
          </div>
        </div>
      ) : null}

      {/* ── 备份弹窗 ──────────────────────────────────────── */}
      {backupsLoading ? <Loading /> : null}
      {backups ? (
        <div className="modal-overlay">
          <div className="modal cc-backup-modal">
            <div className="modal-header">
              <h3>
                {t('备份记录')} · {backups.record.name}
              </h3>
              <button type="button" className="modal-close" onClick={() => setBackups(null)}>
                &times;
              </button>
            </div>
            <div className="modal-body">
              <div className="cc-hint">{t('每次写入前自动备份原文件，最多保留最近 N 份。')}</div>
              {backups.items.length === 0 ? (
                <EmptyState message={t('暂无备份：首次写入后这里会出现自动备份。')} />
              ) : (
                <div className="cc-backup-list">
                  {backups.items.map((item) => (
                    <div key={`${item.kind}-${item.id}`} className="cc-backup-item">
                      <div>
                        <div className="cc-backup-time">{fmtTime(item.createdAt)}</div>
                        <div className="cc-item-meta">
                          <Badge>{item.kind === 'credentials' ? t('凭据文件') : t('主配置')}</Badge>
                          <span className="cc-mono">{fmtSize(item.size)}</span>
                          <span className="cc-mono">{item.fileName}</span>
                        </div>
                      </div>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => restoreBackup(backups.record, item.id, item.kind)}
                      >
                        <RotateCcw size={13} /> {t('恢复')}
                      </Button>
                    </div>
                  ))}
                </div>
              )}
            </div>
            <div className="modal-footer">
              <Button variant="outline" onClick={() => setBackups(null)}>
                {t('关闭')}
              </Button>
            </div>
          </div>
        </div>
      ) : null}

      <ConfirmDialog state={confirm} onClose={() => setConfirm(null)} />
    </div>
  );
}
