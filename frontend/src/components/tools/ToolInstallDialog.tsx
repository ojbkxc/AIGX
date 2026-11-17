// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// Skill / MCP 安装与卸载弹窗（工具服务页「安装 / 更新 / 卸载」入口）。
// 对照 A2A ui-islands tools-install.tsx：选客户端目录 + 选网关 Key（安装时才
// 取明文 Key 写进客户端配置），提交后逐客户端回执。
// 契约：GET /api/tools/install-targets、GET /api/tools/api-keys、
//       POST /api/tools/install-skill|install-mcp|uninstall-skill|uninstall-mcp。

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import { Badge, Button, Input, Select } from '../ui';
import type {
  ToolApiKeyOption,
  ToolInstallOutcome,
  ToolInstallTarget,
  ToolServiceItem,
} from '../../types';
import './ToolInstallDialog.css';

/** 服务 id → 安装逻辑名（后端 web-search / vision / image-generation） */
function serviceIdOf(service: ToolServiceItem): string {
  return service.id;
}

interface ToolInstallDialogProps {
  service: ToolServiceItem;
  mode: 'skill' | 'mcp';
  /** true = 卸载（只选客户端，不选 Key） */
  uninstall?: boolean;
  onClose: () => void;
  /** 完成（安装或卸载）后回调，页面重读目标探测 */
  onDone: () => void;
}

/**
 * ToolInstallDialog — 把服务接入本机客户端目录。
 *
 * 安装：客户端多选 + Skill 变体（skill 模式）/ MCP 模式（mcp 模式）+ 网关 Key。
 * 卸载：只选客户端。两个方向都按客户端逐条回执（部分失败如实展示）。
 */
export default function ToolInstallDialog({
  service,
  mode,
  uninstall = false,
  onClose,
  onDone,
}: ToolInstallDialogProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [targets, setTargets] = useState<ToolInstallTarget[]>([]);
  const [keys, setKeys] = useState<ToolApiKeyOption[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [variant, setVariant] = useState('');
  const [mcpMode, setMcpMode] = useState('');
  const [apiKeyId, setApiKeyId] = useState('');
  const [customDir, setCustomDir] = useState('');
  const [loading, setLoading] = useState(true);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const [outcomes, setOutcomes] = useState<ToolInstallOutcome[] | null>(null);

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const [targetRes, keyRes] = await Promise.all([
          api.getToolInstallTargets(serviceIdOf(service), mode),
          uninstall ? Promise.resolve(null) : api.listToolApiKeys().catch(() => null),
        ]);
        if (!alive) return;
        const list = targetRes?.data?.targets ?? [];
        setTargets(list);
        // 默认勾选检测到 / 已安装的目标
        setSelected(new Set(list.filter((item) => item.detected || item.installed).map((item) => item.key)));
        const keyList = keyRes?.data?.keys ?? [];
        setKeys(keyList);
        if (keyList.length > 0) setApiKeyId(keyList[0].id);
        const variants = service.variants ?? [];
        setVariant(service.recommended_variant || variants.find((v) => v.recommended)?.id || variants[0]?.id || '');
        setMcpMode('');
        setError('');
      } catch (err) {
        if (alive) setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [service, mode, uninstall]);

  const canSubmit = useMemo(() => {
    if (selected.size === 0) return false;
    if (uninstall) return true;
    if (!apiKeyId) return false;
    if (mode === 'skill' && !variant) return false;
    return true;
  }, [selected, uninstall, apiKeyId, mode, variant]);

  const toggle = (key: string): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const submit = async (): Promise<void> => {
    setSubmitting(true);
    setError('');
    try {
      if (uninstall) {
        const body = { service_id: serviceIdOf(service), clients: [...selected] };
        const res =
          mode === 'mcp' ? await api.uninstallToolMcp(body) : await api.uninstallToolSkill(body);
        const results = res?.data?.results ?? [];
        setOutcomes(results);
        const okCount = results.filter((r) => r.ok).length;
        addToast(t('已从 {{ok}}/{{total}} 个客户端卸载', { ok: okCount, total: results.length }), okCount === results.length ? 'success' : 'error');
        onDone();
      } else if (mode === 'mcp') {
        const res = await api.installToolMcp({
          service_id: serviceIdOf(service),
          ...(mcpMode ? { mode: mcpMode } : {}),
          api_key_id: apiKeyId,
          clients: [...selected],
          ...(customDir.trim() ? { output_directory: customDir.trim() } : {}),
        });
        const results = res?.data?.results ?? [];
        setOutcomes(results);
        const okCount = results.filter((r) => r.ok).length;
        addToast(t('已安装到 {{ok}}/{{total}} 个客户端', { ok: okCount, total: results.length }), okCount === results.length ? 'success' : 'error');
        onDone();
      } else {
        const res = await api.installToolSkill({
          variant,
          api_key_id: apiKeyId,
          clients: [...selected],
        });
        const results = res?.data?.results ?? [];
        setOutcomes(results);
        const okCount = results.filter((r) => r.ok).length;
        addToast(t('已安装到 {{ok}}/{{total}} 个客户端', { ok: okCount, total: results.length }), okCount === results.length ? 'success' : 'error');
        onDone();
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      addToast(msg, 'error');
    } finally {
      setSubmitting(false);
    }
  };

  const title = uninstall
    ? t('卸载 {{name}}', { name: t(service.package_name || service.id) })
    : t('安装 {{name}}', { name: t(service.package_name || service.id) });

  return (
    <div className="tid-overlay" role="dialog" aria-modal="true" aria-label={title}>
      <div className="tid-panel">
        <div className="tid-head">
          <div>
            <h2>{title}</h2>
            <p>{t('选择要{{action}}的本机客户端目录', { action: uninstall ? t('卸载') : t('安装') })}</p>
          </div>
          <Badge tone={mode === 'skill' ? 'info' : 'success'}>{mode === 'skill' ? 'Skill' : 'MCP'}</Badge>
        </div>

        <div className="tid-body">
          {loading ? (
            <div className="tid-loading">
              <Loader2 size={14} className="tid-spin" />
              {t('正在探测本机客户端…')}
            </div>
          ) : (
            <>
              {error && <div className="error-message">{error}</div>}

              <div className="tid-block">
                <span className="tid-label">{t('客户端目录')}</span>
                {targets.length === 0 ? (
                  <p className="tid-empty">{t('没有探测到可用的客户端目录')}</p>
                ) : (
                  <div className="tid-targets">
                    {targets.map((target) => {
                      const disabled = target.supported === false;
                      return (
                        <label key={target.key} className={`tid-target${selected.has(target.key) ? ' is-on' : ''}${disabled ? ' is-disabled' : ''}`}>
                          <input
                            type="checkbox"
                            checked={selected.has(target.key)}
                            disabled={disabled}
                            onChange={() => toggle(target.key)}
                          />
                          <span className="tid-target-main">
                            <span className="tid-target-name">{target.label}</span>
                            <code className="tid-target-path">{target.path}</code>
                            {target.note ? <span className="tid-target-note">{target.note}</span> : null}
                          </span>
                          <span className="tid-target-badges">
                            {target.detected ? <Badge tone="success">{t('已探测')}</Badge> : null}
                            {target.installed ? <Badge tone="info">{t('已安装')}</Badge> : null}
                            {disabled ? <Badge tone="neutral">{t('不支持')}</Badge> : null}
                          </span>
                        </label>
                      );
                    })}
                  </div>
                )}
              </div>

              {!uninstall && mode === 'skill' && (
                <label className="tid-block">
                  <span className="tid-label">{t('运行时变体')}</span>
                  <Select value={variant} onChange={(e) => setVariant(e.target.value)} aria-label={t('运行时变体')}>
                    {(service.variants ?? []).map((v) => (
                      <option key={v.id} value={v.id}>
                        {v.label} · {v.runtime} · v{v.version}
                        {v.recommended ? ` · ${t('推荐')}` : ''}
                      </option>
                    ))}
                  </Select>
                </label>
              )}

              {!uninstall && mode === 'mcp' && (
                <label className="tid-block">
                  <span className="tid-label">{t('MCP 模式')}</span>
                  <Select value={mcpMode} onChange={(e) => setMcpMode(e.target.value)} aria-label={t('MCP 模式')}>
                    <option value="">{t('默认')}</option>
                    <option value="url">{t('远程 URL（需客户端支持 HTTP MCP）')}</option>
                    <option value="bridge">{t('本地桥接脚本')}</option>
                  </Select>
                </label>
              )}

              {!uninstall && (
                <label className="tid-block">
                  <span className="tid-label">{t('网关 Key（写入客户端配置，用于调用本工具）')}</span>
                  <Select value={apiKeyId} onChange={(e) => setApiKeyId(e.target.value)} aria-label={t('网关 Key')}>
                    {keys.length === 0 ? <option value="">{t('没有可用的 API Key')}</option> : null}
                    {keys.map((k) => (
                      <option key={k.id} value={k.id}>
                        {k.name} ({k.masked})
                      </option>
                    ))}
                  </Select>
                </label>
              )}

              {!uninstall && mode === 'mcp' && (
                <label className="tid-block">
                  <span className="tid-label">{t('自定义输出目录（可选）')}</span>
                  <Input
                    value={customDir}
                    onChange={(e) => setCustomDir(e.target.value)}
                    placeholder={t('留空 = 用探测到的默认位置')}
                  />
                </label>
              )}

              {outcomes && (
                <div className="tid-outcomes">
                  <span className="tid-label">{t('回执')}</span>
                  {outcomes.map((o) => (
                    <div key={o.clientId} className={`tid-outcome${o.ok ? ' is-ok' : ' is-fail'}`}>
                      <span>{o.label}</span>
                      <code>{o.path}</code>
                      {o.ok ? <Badge tone="success">{t('成功')}</Badge> : <span className="tid-err">{o.error || t('失败')}</span>}
                    </div>
                  ))}
                </div>
              )}
            </>
          )}
        </div>

        <div className="tid-foot">
          <Button variant="outline" size="sm" onClick={onClose} disabled={submitting}>
            {t('关闭')}
          </Button>
          <Button variant="primary" size="sm" disabled={!canSubmit || submitting || loading} onClick={() => void submit()}>
            {submitting ? <Loader2 size={12} className="tid-spin" /> : null}
            {uninstall ? t('卸载') : t('安装')}
          </Button>
        </div>
      </div>
    </div>
  );
}