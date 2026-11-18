import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import type { PromptMode } from '../types';

/**
 * 系统提示词注入（系统设置 → 运维 → 网关，GatewayPanel 下方）
 *
 * 对照 Agent2API「设置 → 网关 → 系统提示词」：网关在转发前对 messages 的
 * system 消息做统一处置——
 *
 * - 透传：客户端发什么转发什么（默认，行为与未开启时逐字相同）
 * - 替换：删掉客户端全部 system 消息，头部插入网关自有提示词
 * - 追加：保留客户端 system，在其后追加一条网关提示词
 *
 * 覆盖层级（优先级从高到低）：按模型覆盖 > 按提供商覆盖（键 = 账号池
 * 提供商名或渠道类型）> 全局；命中任一层即整条替代下层 mode/text。
 *
 * 保存即热生效：下一请求即按新策略注入，无需重启。
 */

const MODES: Array<{ value: PromptMode; label: string; tip: string }> = [
  {
    value: 'passthrough',
    label: '透传',
    tip: '客户端发什么转发什么，网关不干预（默认）。',
  },
  {
    value: 'custom',
    label: '替换',
    tip: '删除客户端所有 system 消息，头部插入下方网关提示词。想让所有请求共享统一人设/规则时选它。',
  },
  {
    value: 'append',
    label: '追加',
    tip: '保留客户端 system 消息，在开头连续 system 块之后追加一条网关提示词。想叠加安全规则又不破坏用户人设时选它。',
  },
];

/** 单条「按模型覆盖」草稿（键=模型名）。 */
interface ModelOverrideRow {
  model: string;
  mode: PromptMode;
  text: string;
}

/** 单条「按提供商覆盖」草稿（键=账号 provider 名或渠道类型名）。 */
interface ProviderOverrideRow {
  provider: string;
  mode: PromptMode;
  text: string;
}

/** 单条「网关自带提示词」草稿（A2A promptGateway / promptGatewayText）。 */
interface GatewayRow {
  provider: string;
  /** 是否安装网关内置段（缺省/true = 装） */
  enabled: boolean;
  identity: string;
  stable: string;
  dynamic: string;
}

/** 把后端两张表（开关 + 三段覆盖）合并成一行草稿。 */
function collectGatewayRows(
  gateway: Record<string, boolean>,
  gatewayText: Record<string, { identity?: string; stable?: string; dynamic?: string }>,
): GatewayRow[] {
  const providers = Array.from(new Set([...Object.keys(gateway), ...Object.keys(gatewayText)])).sort();
  return providers.map((provider) => ({
    provider,
    enabled: gateway[provider] !== false,
    identity: gatewayText[provider]?.identity ?? '',
    stable: gatewayText[provider]?.stable ?? '',
    dynamic: gatewayText[provider]?.dynamic ?? '',
  }));
}

/** 草稿行 → 后端两张表（空名行丢弃；三段空串 = 不覆盖，交给官方原文）。 */
function serializeGatewayRows(rows: GatewayRow[]): {
  gateway: Record<string, boolean>;
  gateway_text: Record<string, { identity?: string; stable?: string; dynamic?: string }>;
} {
  const gateway: Record<string, boolean> = {};
  const gateway_text: Record<string, { identity?: string; stable?: string; dynamic?: string }> = {};
  for (const row of rows) {
    const name = row.provider.trim();
    if (!name || gateway[name] !== undefined) continue;
    gateway[name] = row.enabled;
    const blocks: { identity?: string; stable?: string; dynamic?: string } = {};
    if (row.identity.trim()) blocks.identity = row.identity;
    if (row.stable.trim()) blocks.stable = row.stable;
    if (row.dynamic.trim()) blocks.dynamic = row.dynamic;
    if (Object.keys(blocks).length > 0) gateway_text[name] = blocks;
  }
  return { gateway, gateway_text };
}

export default function PromptPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [mode, setMode] = useState<PromptMode>('passthrough');
  const [text, setText] = useState('');
  const [devCompat, setDevCompat] = useState(true);
  const [rows, setRows] = useState<ModelOverrideRow[]>([]);
  const [providerRows, setProviderRows] = useState<ProviderOverrideRow[]>([]);
  const [gatewayRows, setGatewayRows] = useState<GatewayRow[]>([]);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getGatewayPrompt();
        const d = res?.data;
        if (d) {
          setMode(d.mode ?? 'passthrough');
          setText(d.text ?? '');
          setDevCompat(d.developer_role_compat !== false);
          setRows(
            Object.entries(d.models ?? {}).map(([model, over]) => ({
              model,
              mode: over.mode ?? 'passthrough',
              text: over.text ?? '',
            })),
          );
          setProviderRows(
            Object.entries(d.providers ?? {}).map(([provider, over]) => ({
              provider,
              mode: over.mode ?? 'passthrough',
              text: over.text ?? '',
            })),
          );
          setGatewayRows(collectGatewayRows(d.gateway ?? {}, d.gateway_text ?? {}));
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      // 空模型名/提供商名 / 重名行直接丢弃（后者保留先出现的），避免写入无法命中的键
      const models: Record<string, { mode: PromptMode; text: string }> = {};
      for (const row of rows) {
        const name = row.model.trim();
        if (!name || models[name]) continue;
        models[name] = { mode: row.mode, text: row.text };
      }
      const providers: Record<string, { mode: PromptMode; text: string }> = {};
      for (const row of providerRows) {
        const name = row.provider.trim();
        if (!name || providers[name]) continue;
        providers[name] = { mode: row.mode, text: row.text };
      }
      const { gateway, gateway_text } = serializeGatewayRows(gatewayRows);
      const res = await api.saveGatewayPrompt({
        mode,
        text,
        developer_role_compat: devCompat,
        models,
        providers,
        gateway,
        gateway_text,
      });
      const d = res?.data;
      if (d) {
        setMode(d.mode ?? mode);
        setText(d.text ?? text);
        setDevCompat(d.developer_role_compat !== false);
        setRows(
          Object.entries(d.models ?? {}).map(([model, over]) => ({
            model,
            mode: over.mode ?? 'passthrough',
            text: over.text ?? '',
          })),
        );
        setProviderRows(
          Object.entries(d.providers ?? {}).map(([provider, over]) => ({
            provider,
            mode: over.mode ?? 'passthrough',
            text: over.text ?? '',
          })),
        );
        setGatewayRows(collectGatewayRows(d.gateway ?? {}, d.gateway_text ?? {}));
      }
      addToast(t('系统提示词已保存（下一请求即生效）'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const updateRow = (index: number, patch: Partial<ModelOverrideRow>): void => {
    setRows(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  };

  const updateProviderRow = (index: number, patch: Partial<ProviderOverrideRow>): void => {
    setProviderRows(providerRows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  };

  const updateGatewayRow = (index: number, patch: Partial<GatewayRow>): void => {
    setGatewayRows(gatewayRows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  const modeTip = MODES.find((m) => m.value === mode)?.tip ?? '';

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('系统提示词')}</h2>
        <p className="card-subtitle">
          {t('网关统一处置转发请求的 system 消息；保存即热生效。透传模式行为与未开启时完全相同。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label>{t('注入模式')}</label>
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
              {MODES.map((m) => (
                <button
                  key={m.value}
                  type="button"
                  className={`btn ${mode === m.value ? 'btn-primary' : 'btn-secondary'}`}
                  onClick={() => setMode(m.value)}
                >
                  {t(m.label)}
                </button>
              ))}
            </div>
            <div className="form-hint">{t(modeTip)}</div>
          </div>

          {mode !== 'passthrough' && (
            <div className="form-group">
              <label>{t('网关提示词')}</label>
              <textarea
                className="form-input"
                rows={8}
                value={text}
                onChange={(e) => setText(e.target.value)}
                placeholder={t('多行文本；空文本等价于透传')}
              />
              <div className="form-hint">
                {t('当前 {{count}} 字符（上限 64000）', { count: text.length })}
              </div>
            </div>
          )}

          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={devCompat}
                onChange={(e) => setDevCompat(e.target.checked)}
              />
              <span>{t('developer 角色兼容（role:"developer" 归一为 system）')}</span>
            </label>
            <div className="form-hint">
              {t('OpenAI 新 SDK（o 系模型）用 developer 角色替代 system；开启时网关自动归一，关闭时按未知角色丢弃（老行为）。')}
            </div>
          </div>

          <div className="form-group">
            <label>{t('按模型覆盖')}</label>
            <div className="form-hint" style={{ marginBottom: 8 }}>
              {t('命中模型名的请求整条走该行配置（模式 + 正文），未命中回落上面的全局设置。正文留空 = 该模型不做注入。')}
            </div>
            {rows.length === 0 ? (
              <div className="empty-state">
                <p>{t('暂无按模型覆盖，全部请求走全局设置')}</p>
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
                {rows.map((row, i) => (
                  <div
                    key={`${row.model}-${i}`}
                    style={{ border: '1px solid var(--border)', borderRadius: 6, padding: 12 }}
                  >
                    <div style={{ display: 'flex', gap: 8, marginBottom: 8 }}>
                      <input
                        className="form-input"
                        style={{ flex: 2 }}
                        placeholder={t('模型名（客户端请求里的原名）')}
                        value={row.model}
                        onChange={(e) => updateRow(i, { model: e.target.value })}
                      />
                      <select
                        className="form-input"
                        style={{ flex: 1 }}
                        value={row.mode}
                        onChange={(e) => updateRow(i, { mode: e.target.value as PromptMode })}
                      >
                        {MODES.map((m) => (
                          <option key={m.value} value={m.value}>
                            {t(m.label)}
                          </option>
                        ))}
                      </select>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={() => setRows(rows.filter((_, idx) => idx !== i))}
                      >
                        {t('删除')}
                      </button>
                    </div>
                    <textarea
                      className="form-input"
                      rows={3}
                      placeholder={t('该模型的提示词正文（留空 = 不注入）')}
                      value={row.text}
                      onChange={(e) => updateRow(i, { text: e.target.value })}
                    />
                  </div>
                ))}
              </div>
            )}
            <div style={{ marginTop: 8 }}>
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setRows([...rows, { model: '', mode: 'append', text: '' }])}
              >
                {t('添加模型覆盖')}
              </button>
            </div>
          </div>

          <div className="form-group">
            <label>{t('按提供商覆盖')}</label>
            <div className="form-hint" style={{ marginBottom: 8 }}>
              {t('键 = 账号池提供商名（qoder / trae …）或渠道类型（openai_compatible / anthropic …）；优先级低于按模型覆盖，命中则整条替代全局设置。正文留空 = 该提供商不做注入。')}
            </div>
            {providerRows.length === 0 ? (
              <div className="empty-state">
                <p>{t('暂无按提供商覆盖，全部请求走全局设置')}</p>
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
                {providerRows.map((row, i) => (
                  <div
                    key={`${row.provider}-${i}`}
                    style={{ border: '1px solid var(--border)', borderRadius: 6, padding: 12 }}
                  >
                    <div style={{ display: 'flex', gap: 8, marginBottom: 8 }}>
                      <input
                        className="form-input"
                        style={{ flex: 2 }}
                        placeholder={t('提供商（账号池提供商名或渠道类型）')}
                        value={row.provider}
                        onChange={(e) => updateProviderRow(i, { provider: e.target.value })}
                      />
                      <select
                        className="form-input"
                        style={{ flex: 1 }}
                        value={row.mode}
                        onChange={(e) => updateProviderRow(i, { mode: e.target.value as PromptMode })}
                      >
                        {MODES.map((m) => (
                          <option key={m.value} value={m.value}>
                            {t(m.label)}
                          </option>
                        ))}
                      </select>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={() => setProviderRows(providerRows.filter((_, idx) => idx !== i))}
                      >
                        {t('删除')}
                      </button>
                    </div>
                    <textarea
                      className="form-input"
                      rows={3}
                      placeholder={t('该提供商的提示词正文（留空 = 不注入）')}
                      value={row.text}
                      onChange={(e) => updateProviderRow(i, { text: e.target.value })}
                    />
                  </div>
                ))}
              </div>
            )}
            <div style={{ marginTop: 8 }}>
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setProviderRows([...providerRows, { provider: '', mode: 'append', text: '' }])}
              >
                {t('添加提供商覆盖')}
              </button>
            </div>
          </div>

          <div className="form-group">
            <label>{t('网关自带提示词（按提供商）')}</label>
            <div className="form-hint" style={{ marginBottom: 8 }}>
              {t('网关内置的身份段（如 ZCode 活动套餐通道的官方三段）由提供商实现提供；这里只控制「装不装」与「正文覆盖」。三段留空 = 用官方原文；取消勾选 = 该提供商不装。')}
            </div>
            {gatewayRows.length === 0 ? (
              <div className="empty-state">
                <p>{t('暂无逐家配置，全部使用官方默认（装）')}</p>
              </div>
            ) : (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
                {gatewayRows.map((row, i) => (
                  <div
                    key={`${row.provider}-${i}`}
                    style={{ border: '1px solid var(--border)', borderRadius: 6, padding: 12 }}
                  >
                    <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 8 }}>
                      <input
                        className="form-input"
                        style={{ flex: 2 }}
                        placeholder={t('提供商 id（如 zcode）')}
                        value={row.provider}
                        onChange={(e) => updateGatewayRow(i, { provider: e.target.value })}
                      />
                      <label style={{ display: 'flex', alignItems: 'center', gap: 6, flex: 1 }}>
                        <input
                          type="checkbox"
                          checked={row.enabled}
                          onChange={(e) => updateGatewayRow(i, { enabled: e.target.checked })}
                        />
                        <span>{t('安装网关内置段')}</span>
                      </label>
                      <button
                        type="button"
                        className="btn btn-secondary"
                        onClick={() => setGatewayRows(gatewayRows.filter((_, idx) => idx !== i))}
                      >
                        {t('删除')}
                      </button>
                    </div>
                    <textarea
                      className="form-input"
                      rows={2}
                      placeholder={t('身份段（identity，留空 = 官方原文）')}
                      value={row.identity}
                      onChange={(e) => updateGatewayRow(i, { identity: e.target.value })}
                    />
                    <textarea
                      className="form-input"
                      rows={2}
                      style={{ marginTop: 6 }}
                      placeholder={t('稳定段（stable，留空 = 官方原文）')}
                      value={row.stable}
                      onChange={(e) => updateGatewayRow(i, { stable: e.target.value })}
                    />
                    <textarea
                      className="form-input"
                      rows={2}
                      style={{ marginTop: 6 }}
                      placeholder={t('动态段（dynamic，支持 {cwd}/{platform}/{model} 等占位符）')}
                      value={row.dynamic}
                      onChange={(e) => updateGatewayRow(i, { dynamic: e.target.value })}
                    />
                  </div>
                ))}
              </div>
            )}
            <div style={{ marginTop: 8 }}>
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() =>
                  setGatewayRows([
                    ...gatewayRows,
                    { provider: '', enabled: true, identity: '', stable: '', dynamic: '' },
                  ])
                }
              >
                {t('添加提供商')}
              </button>
            </div>
          </div>

          <div className="settings-actions">
            <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
              {saving ? t('保存中...') : t('保存更改')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
