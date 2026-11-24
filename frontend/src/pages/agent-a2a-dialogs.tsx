/**
 * A2A（Agent2API v3.5.1）Agent 页弹窗层。
 *
 * 对齐 `agent-path-dialog.tsx`（指定配置路径）与 `agent-profile-dialog.tsx`
 * （新增 / 编辑配置）：字段顺序、按钮与文案照 A2A。
 *
 * AIGX 后端缺「配置保存 / 预览 / 应用」接口，保存动作保留 A2A 的按钮与顺序，
 * 写入待后端接口就绪后接线（数据源缺失已记入交付清单）。
 */

import { useState } from 'react';
import { ChevronDown, ChevronRight, Folder } from 'lucide-react';
import { Button } from '../components/ui';
import { A2A_TEXT } from './agent-a2a-types';
import type { A2aClient, A2aProfile } from './agent-a2a-types';
import { A2aModelsFields, type A2aCatalogModel } from './agent-a2a-models';

/** A2A 的客户端协议标签（对齐 agent-model.ts 的 protocolLabel） */
export function protocolLabelText(value: string): string {
  return {
    anthropic: 'Anthropic Messages',
    responses: 'OpenAI Responses',
    openai_chat: 'OpenAI Chat Completions',
    gemini: 'Gemini API',
    native: '客户端原生配置',
  }[value] || value;
}

/** 折叠区触发（与 A2A CollapsibleTrigger 同形态） */
export function A2aCollapsibleTrigger({
  open,
  label,
  onToggle,
}: {
  open: boolean;
  label: string;
  onToggle: () => void;
}): JSX.Element {
  return (
    <button type="button" className="a2a-collapsible-trigger" onClick={onToggle} aria-expanded={open}>
      <span className="a2a-chevron">{open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
      {label}
    </button>
  );
}

function DialogShell({
  title,
  subtitle,
  onClose,
  children,
  footer,
  width = 520,
}: {
  title: string;
  subtitle?: string;
  onClose: () => void;
  children: React.ReactNode;
  footer?: React.ReactNode;
  width?: number;
}): JSX.Element {
  return (
    <div className="a2a-modal-overlay" onClick={onClose} role="dialog" aria-modal="true">
      <div
        className="a2a-modal"
        style={{ maxWidth: width, width: '90%' }}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="a2a-modal-header">
          <div>
            <h3 className="a2a-modal-title">{title}</h3>
            {subtitle && <p className="a2a-modal-subtitle">{subtitle}</p>}
          </div>
          <button type="button" className="a2a-modal-close" onClick={onClose} aria-label="关闭">×</button>
        </div>
        <div className="a2a-modal-body">{children}</div>
        <div className="a2a-modal-footer">
          <span className="a2a-modal-footer-spacer" />
          {footer}
          <Button variant="outline" onClick={onClose}>{A2A_TEXT.cancel}</Button>
        </div>
      </div>
    </div>
  );
}

/** 标签 + 控件的字段块（对齐 A2A 的 Field/FieldLabel 结构） */
function A2aField({
  label,
  fieldKey,
  hint,
  children,
}: {
  label: string;
  fieldKey?: string;
  hint?: string;
  children: React.ReactNode;
}): JSX.Element {
  return (
    <div className="a2a-field">
      <span className="a2a-label">
        {label}
        {fieldKey && <code className="a2a-field-key">{fieldKey}</code>}
      </span>
      {children}
      {hint && <p className="a2a-hint">{hint}</p>}
    </div>
  );
}

/** A2A 的「指定配置路径」弹窗（agent-path-dialog.tsx） */
export function A2aPathDialog({ client, onClose }: { client: A2aClient; onClose: () => void }): JSX.Element {
  const [path, setPath] = useState(client.pathOverride || client.configPath);
  const [error, setError] = useState('');

  const save = (nextPath: string): void => {
    setError('');
    // AIGX 后端缺「配置路径保存」接口：保存按钮与位置照 A2A，写入待接线。
    if (nextPath === path) {
      onClose();
      return;
    }
    setError('AIGX 后端暂未提供配置路径保存接口，本项已按 A2A 结构落位，写入待接线。');
  };

  const browse = (): void => {
    // AIGX 无桌面端文件选择桥：按钮保留 A2A 位置与文案。
    setError('AIGX 当前环境不支持浏览本机文件，请直接填写完整路径。');
  };

  return (
    <DialogShell
      title={`${A2A_TEXT.pathDialogTitle} · ${client.name}`}
      onClose={onClose}
      footer={
        <>
          <Button variant="secondary" onClick={() => save('')}>{A2A_TEXT.restoreAuto}</Button>
          <Button onClick={() => save(path)}>{A2A_TEXT.savePath}</Button>
        </>
      }
    >
      {error && <div className="a2a-alert a2a-alert-destructive">{error}</div>}
      <A2aField label={A2A_TEXT.pathLabel} hint="只改变本页读取与写入的目标文件，不修改客户端自身启动设置。">
        <div className="a2a-path-row">
          <input
            className="form-input a2a-path-input"
            value={path}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => setPath(event.currentTarget.value)}
          />
          <Button variant="outline" size="sm" onClick={browse}>
            <Folder size={13} />
            {A2A_TEXT.browseFile}
          </Button>
        </div>
      </A2aField>
    </DialogShell>
  );
}

/** A2A 的「新增配置 / 编辑配置」弹窗（agent-profile-dialog.tsx） */
export function A2aProfileDialog({
  client,
  profile,
  creating,
  onClose,
  catalog = [],
}: {
  client: A2aClient;
  profile: A2aProfile;
  creating: boolean;
  onClose: () => void;
  catalog?: A2aCatalogModel[];
}): JSX.Element {
  const [draft, setDraft] = useState<A2aProfile>(() => ({ ...profile }));
  const [error, setError] = useState('');
  const [saving, setSaving] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);

  const update = (patch: Partial<A2aProfile>): void => {
    setDraft((current) => ({ ...current, ...patch }));
    setError('');
  };

  const save = (): void => {
    setSaving(true);
    setError('');
    // AIGX 后端缺「配置保存」接口：字段顺序与按钮照 A2A，写入待接线。
    setError('AIGX 后端暂未提供客户端配置写入接口，本项已按 A2A 结构落位，保存待接线。');
    setSaving(false);
  };

  const isGateway = draft.kind === 'gateway';

  return (
    <DialogShell
      title={`${creating ? A2A_TEXT.newConfig : A2A_TEXT.editConfig} · ${client.name}`}
      subtitle={creating ? '保存后可在配置列表中切换，不影响当前使用的配置。' : '保存后同步更新客户端；写入前可查看改动，并自动备份。'}
      onClose={onClose}
      width={620}
      footer={
        <Button disabled={saving} onClick={save}>
          {saving ? A2A_TEXT.saving : A2A_TEXT.save}
        </Button>
      }
    >
      {error && <div className="a2a-alert a2a-alert-destructive">{error}</div>}

      <section className="a2a-profile-connection">
        <div className="a2a-grid-2">
          <A2aField label={A2A_TEXT.profileName}>
            <input
              className="form-input"
              value={draft.name}
              autoComplete="off"
              onChange={(event) => update({ name: event.currentTarget.value })}
            />
          </A2aField>
          <A2aField label={A2A_TEXT.profileNote}>
            <input
              className="form-input"
              value={draft.note}
              autoComplete="off"
              onChange={(event) => update({ note: event.currentTarget.value })}
            />
          </A2aField>
        </div>

        <div className="a2a-grid-2">
          <A2aField label={A2A_TEXT.connectionMode}>
            <select
              className="form-input"
              value={draft.kind}
              onChange={(event) => update({ kind: event.currentTarget.value })}
            >
              <option value="gateway">{A2A_TEXT.modeGateway}</option>
              <option value="official">{A2A_TEXT.modeOfficial}</option>
              <option value="relay">{A2A_TEXT.modeRelay}</option>
              <option value="local">{A2A_TEXT.modeLocal}</option>
            </select>
          </A2aField>
          <A2aField label={A2A_TEXT.authMode}>
            <select
              className="form-input"
              value={draft.authMode}
              onChange={(event) => update({ authMode: event.currentTarget.value })}
            >
              <option value="gateway">{A2A_TEXT.authGatewayKey}</option>
              <option value="key">{A2A_TEXT.authApiKey}</option>
              <option value="env">{A2A_TEXT.authEnv}</option>
              <option value="none">{A2A_TEXT.authNone}</option>
              <option value="preserve">{A2A_TEXT.authPreserve}</option>
            </select>
          </A2aField>
        </div>

        <A2aField
          label={A2A_TEXT.baseUrl}
          fieldKey="baseUrl"
          hint={draft.protocol === 'gemini'
            ? '填写服务根地址，不要附加 /v1beta 或 generateContent 路径。'
            : draft.protocol === 'anthropic'
              ? '不要添加 /v1/messages；客户端会自行追加请求路径。'
              : '通常以 /v1 结尾，不要附加具体请求路径。'}
        >
          <input
            className="form-input"
            value={draft.baseUrl}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => update({ baseUrl: event.currentTarget.value })}
          />
        </A2aField>

        {draft.authMode === 'gateway' && (
          <A2aField label={A2A_TEXT.gatewayKey} hint="应用时使用当前启用的真实 Key；预览会隐藏密钥。">
            <select className="form-input" value={draft.gatewayKeyId} onChange={(event) => update({ gatewayKeyId: event.currentTarget.value })}>
              <option value="">{A2A_TEXT.gatewayKeyFirst}</option>
            </select>
          </A2aField>
        )}

        {draft.authMode === 'key' && (
          <A2aField label={A2A_TEXT.apiKeyField} fieldKey="apiKey" hint="密钥在 Agent2API 中加密保存；使用时按客户端要求写入。">
            <input
              className="form-input"
              type="password"
              autoComplete="new-password"
              value={draft.apiKey}
              placeholder={draft.hasApiKey ? '留空保留已保存密钥' : '请输入密钥'}
              onChange={(event) => update({ apiKey: event.currentTarget.value })}
            />
          </A2aField>
        )}

        {draft.authMode === 'env' && (
          <A2aField label={A2A_TEXT.envKeyField} fieldKey="env_key" hint="只保存变量引用，不修改系统环境；请在启动客户端的环境里设置变量值。">
            <input
              className="form-input"
              value={draft.envKey}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => update({ envKey: event.currentTarget.value })}
            />
          </A2aField>
        )}

        <div className="a2a-collapsible">
          <A2aCollapsibleTrigger
            open={advancedOpen}
            label={`${A2A_TEXT.advanced} · ${protocolLabelText(draft.protocol)}`}
            onToggle={() => setAdvancedOpen((value) => !value)}
          />
          {advancedOpen && (
            <div className="a2a-collapsible-body">
              <A2aField label={A2A_TEXT.protocol} hint={A2A_TEXT.protocolNote}>
                <select
                  className="form-input"
                  value={draft.protocol}
                  onChange={(event) => update({ protocol: event.currentTarget.value })}
                >
                  {client.protocols.map((value) => (
                    <option key={value} value={value}>{protocolLabelText(value)}</option>
                  ))}
                </select>
              </A2aField>
            </div>
          )}
        </div>
      </section>

      {/* 模型配置：支持模型列表的客户端走 ModelsFields，其余走单模型字段（对齐 A2A） */}
      {client.supportsModels ? (
        <A2aModelsFields
          client={client}
          draft={draft}
          update={update}
          catalog={catalog}
          disabled={saving}
        />
      ) : (
        <section className="a2a-model-section">
          <h3 className="a2a-section-title">模型配置</h3>
          <A2aField
            label={isGateway ? A2A_TEXT.chooseFromCatalog : A2A_TEXT.modelId}
            fieldKey="model"
            hint="使用真实模型 ID，不要填写模型展示名。"
          >
            <input
              className="form-input"
              value={draft.model}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => update({ model: event.currentTarget.value, effort: '' })}
            />
          </A2aField>

          {client.supportsEffort && (
            <A2aField label={A2A_TEXT.thinkingLevel} fieldKey="effort">
              <select
                className="form-input"
                value={draft.effort}
                onChange={(event) => update({ effort: event.currentTarget.value })}
              >
                <option value="">{A2A_TEXT.modelDefault}</option>
                {['low', 'medium', 'high', 'xhigh'].map((value) => (
                  <option key={value} value={value}>{value}</option>
                ))}
              </select>
            </A2aField>
          )}

          {client.supportsDefault && (
            <A2aField label={A2A_TEXT.defaultModel}>
              <select
                className="form-input"
                value={draft.model}
                onChange={(event) => update({ model: event.currentTarget.value, effort: '' })}
              >
                {draft.models.filter((model) => model.id).length === 0 && <option value="">{A2A_TEXT.atLeastOneModel}</option>}
                {draft.models.filter((model) => model.id).map((model) => (
                  <option key={model.id} value={model.id}>{model.name || model.id}</option>
                ))}
              </select>
            </A2aField>
          )}
        </section>
      )}

      {client.supportsToggle && (
        <A2aField label={A2A_TEXT.togglingOn} hint="确认写入后会同步修改客户端开关；取消不会生效。">
          <div className="a2a-switch-row">
            <input
              type="checkbox"
              checked={draft.enabled}
              aria-label={A2A_TEXT.togglingOn}
              onChange={(event) => update({ enabled: event.currentTarget.checked })}
            />
          </div>
        </A2aField>
      )}
    </DialogShell>
  );
}

/** A2A 的「一键接入」弹窗（agent-quick-connect.tsx） */
export function A2aQuickConnectDialog({
  client,
  catalog = [],
  gatewayKeys = [],
  defaultModel = '',
  onClose,
}: {
  client: A2aClient;
  catalog?: A2aCatalogModel[];
  gatewayKeys?: Array<{ id: string; name: string }>;
  defaultModel?: string;
  onClose: () => void;
}): JSX.Element {
  const [draft, setDraft] = useState<A2aProfile>(() => quickProfile(client, gatewayKeys[0]?.id || '', defaultModel));
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  const update = (patch: Partial<A2aProfile>): void => {
    setDraft((current) => ({ ...current, ...patch }));
    setError('');
  };

  const selectedKey = gatewayKeys.find((key) => key.id === draft.gatewayKeyId);

  const connect = (): void => {
    if (busy) return;
    setBusy(true);
    setError('');
    // AIGX 后端缺「一键接入」写入接口：结构、字段与按钮照 A2A，写入待接线。
    setError('AIGX 后端暂未提供客户端写入接口，本项已按 A2A 结构落位，接入待接线。');
    setBusy(false);
  };

  return (
    <DialogShell
      title={`${A2A_TEXT.quickConnectTitle} · ${client.name}`}
      subtitle={A2A_TEXT.quickConnectDesc}
      onClose={onClose}
      width={620}
      footer={
        <Button disabled={busy} onClick={connect}>{A2A_TEXT.connectClient}</Button>
      }
    >
      {error && <div className="a2a-alert a2a-alert-destructive">{error}</div>}

      <section className="a2a-quick-info">
        <div className="a2a-quick-info-row">
          <span className="a2a-quick-info-key">{A2A_TEXT.endpoint}</span>
          <code className="a2a-quick-info-value">{draft.baseUrl || '—'}</code>
          <span className="a2a-quick-badge">{protocolLabelText(draft.protocol)}</span>
        </div>
        <div className="a2a-quick-info-row">
          <span className="a2a-quick-info-key">{A2A_TEXT.writeFile}</span>
          <code className="a2a-quick-info-value is-muted">{client.resolvedPath}</code>
        </div>
        <div className="a2a-quick-info-row">
          <span className="a2a-quick-info-key">{A2A_TEXT.gatewayKey}</span>
          <select
            className="form-input a2a-quick-info-select"
            value={draft.gatewayKeyId}
            disabled={busy || !gatewayKeys.length}
            aria-label={A2A_TEXT.gatewayKey}
            onChange={(event) => update({ gatewayKeyId: event.currentTarget.value })}
          >
            {!gatewayKeys.length && <option value="">选择网关 Key</option>}
            {gatewayKeys.map((key) => (
              <option key={key.id} value={key.id}>{key.name || key.id}</option>
            ))}
          </select>
          <span className="a2a-quick-info-hint">
            {selectedKey ? '换 Key 后会按新 Key 的授权重新核对已选模型' : '选择网关 Key'}
          </span>
        </div>
      </section>

      {client.supportsModels ? (
        <A2aModelsFields
          client={client}
          draft={draft}
          update={update}
          catalog={catalog}
          disabled={busy}
          quick
          layout="split"
        />
      ) : (
        <A2aField label={A2A_TEXT.modelId} fieldKey="model" hint="使用真实模型 ID，不要填写模型展示名。">
          <input
            className="form-input"
            value={draft.model}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => update({ model: event.currentTarget.value, effort: '' })}
          />
        </A2aField>
      )}

      {!catalog.length && <div className="a2a-alert a2a-alert-warning">{A2A_TEXT.modelCatalogEmpty}</div>}
    </DialogShell>
  );
}

/** A2A 的写入预览（agent-preview.tsx 的 WritePreview：逐行 diff + 文件级状态） */
function WritePreview({ preview }: { preview: A2aPreviewLike }): JSX.Element {
  return (
    <div className="a2a-write-preview">
      {preview.warnings.map((warning, index) => (
        <div key={index} className="a2a-alert a2a-alert-warning">{warning}</div>
      ))}
      <p className="a2a-hint">{A2A_TEXT.previewHeadline}</p>
      {preview.files.map((file) => (
        <div key={file.path} className="a2a-preview-file">
          <div className="a2a-preview-file-head">
            <code className="a2a-preview-path">{file.path}</code>
            <span className="a2a-preview-state">
              {file.created ? '新建文件' : file.deleted ? '删除新建文件' : file.changed ? '有改动' : '无改动'}
            </span>
          </div>
          <pre className="a2a-code" aria-label="配置差异">
            {file.diff.map((line, index) => (
              <span
                key={index}
                className={`a2a-line${line.kind === 'added' ? ' is-added' : line.kind === 'removed' ? ' is-removed' : ''}`}
              >
                <span className="a2a-line-no">{line.newLine ?? line.oldLine ?? ''}</span>
                <span className="a2a-line-sign" aria-hidden="true">
                  {line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ' '}
                </span>
                <span className="a2a-line-text">{line.text || ' '}</span>
              </span>
            ))}
          </pre>
        </div>
      ))}
    </div>
  );
}

/** A2A 的「应用 / 停用 / 删除 / 恢复」确认弹窗（agent-preview.tsx 的 ApplyDialog） */
export function A2aApplyDialog({
  client,
  operation,
  preview,
  onClose,
}: {
  client: A2aClient;
  operation: import('./agent-a2a-types').A2aOperation;
  preview: A2aPreviewLike | null;
  onClose: () => void;
}): JSX.Element {
  const deleting = operation === 'native-delete' || operation === 'config-delete';
  const title = deleting
    ? A2A_TEXT.deleteConfig
    : operation === 'config-disable'
      ? A2A_TEXT.confirmDisable
      : operation === 'config-use'
        ? (client.supportsDefault ? A2A_TEXT.useConfig : A2A_TEXT.toggleOn)
        : operation === 'disconnect'
          ? A2A_TEXT.disconnect
          : operation === 'restore'
            ? A2A_TEXT.restore
            : operation === 'default'
              ? A2A_TEXT.applyDefault
              : A2A_TEXT.applyToClient;
  const confirmText = deleting
    ? A2A_TEXT.confirmDelete
    : operation === 'config-disable'
      ? A2A_TEXT.confirmDisable
      : operation === 'config-use'
        ? A2A_TEXT.confirmUse
        : A2A_TEXT.apply;

  return (
    <DialogShell
      title={`${title} · ${client.name}`}
      onClose={onClose}
      width={620}
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>{A2A_TEXT.regenerate}</Button>
          <Button variant={deleting ? 'danger' : 'primary'} disabled={!preview} onClick={onClose}>
            {confirmText}
          </Button>
        </>
      }
    >
      {preview ? <WritePreview preview={preview} /> : (
        <div className="a2a-preview-loading">正在生成准确预览…</div>
      )}
    </DialogShell>
  );
}

/** 预览负载（对齐 A2A AgentPreview 的文件级 diff 形状） */
type A2aPreviewLike = import('./agent-a2a-types').A2aPreview;

/** A2A 的 blankProfile / quickProfile 口径（agent-model.ts + agent-quick-model.ts） */
function quickProfile(client: A2aClient, gatewayKeyId: string, defaultModel: string): A2aProfile {
  const protocol = client.protocols[0] || '';
  return {
    id: `quick-${client.id}`,
    name: 'Agent2API',
    kind: client.supportsGateway ? 'gateway' : 'relay',
    protocol,
    baseUrl: '',
    model: defaultModel,
    provider: '',
    effort: '',
    enabled: true,
    default: client.supportsDefault,
    note: '',
    models: [],
    claudeModels: { opus: '', sonnet: '', haiku: '', subagent: '' },
    authMode: client.supportsGateway ? 'gateway' : 'key',
    apiKey: '',
    envKey: '',
    gatewayKeyId,
  };
}
