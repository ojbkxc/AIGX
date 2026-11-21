// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// Skill / MCP 安装与卸载弹窗（工具服务页「安装 / 更新 / 卸载」入口）。
// 对照 A2A ui-islands tools-install.tsx：选客户端目录 + 选网关 Key（安装时才
// 取明文 Key 写进客户端配置），提交后逐客户端回执。
// 契约：GET /api/tools/install-targets、GET /api/tools/api-keys、
//       POST /api/tools/install-skill|install-mcp|uninstall-skill|uninstall-mcp。

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Copy, Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import { Badge, Button, Input, Select, Textarea } from '../ui';
import type {
  ToolApiKeyOption,
  ToolInstallInfo,
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
  const [info, setInfo] = useState<ToolInstallInfo | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [variant, setVariant] = useState('');
  const [mcpMode, setMcpMode] = useState<'http' | 'bridge'>('http');
  const [apiKeyId, setApiKeyId] = useState('');
  const [outputDirectory, setOutputDirectory] = useState('');
  const [referenceDirectories, setReferenceDirectories] = useState('');
  const [loading, setLoading] = useState(true);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const [outcomes, setOutcomes] = useState<ToolInstallOutcome[] | null>(null);

  /** 图片生成支持本地桥接（需要输出目录）；视觉也支持桥接。 */
  const supportsBridge = service.id === 'vision' || service.id === 'image-generation';
  const imageBridge = service.id === 'image-generation' && mode === 'mcp' && mcpMode === 'bridge';

  useEffect(() => {
    let alive = true;
    (async () => {
      try {
        const [targetRes, keyRes, infoRes] = await Promise.all([
          api.getToolInstallTargets(serviceIdOf(service), mode),
          uninstall ? Promise.resolve(null) : api.listToolApiKeys().catch(() => null),
          uninstall ? Promise.resolve(null) : api.getToolInstallInfo(serviceIdOf(service)).catch(() => null),
        ]);
        if (!alive) return;
        const list = targetRes?.data?.targets ?? [];
        setTargets(list);
        // 默认勾选检测到 / 已安装的目标
        setSelected(new Set(list.filter((item) => item.detected || item.installed).map((item) => item.key)));
        const keyList = keyRes?.data?.keys ?? [];
        setKeys(keyList);
        if (keyList.length > 0) setApiKeyId(keyList[0].id);
        setInfo((infoRes?.data as ToolInstallInfo | null) ?? null);
        const variants = service.variants ?? [];
        setVariant(service.recommended_variant || variants.find((v) => v.recommended)?.id || variants[0]?.id || '');
        // 视觉 / 图片生成默认本地桥接（上游同款）；其余 MCP 默认远程 HTTP
        setMcpMode(supportsBridge ? 'bridge' : 'http');
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
  }, [service, mode, uninstall, supportsBridge]);

  const selectableTargets = useMemo(
    () => targets.filter((target) => target.supported !== false),
    [targets],
  );
  const allSelected = selectableTargets.length > 0 && selectableTargets.every((target) => selected.has(target.key));

  /** MCP 客户端配置示例（把网关 Key 换成自己的即可） */
  const mcpSnippet = useMemo(() => {
    if (!info) return '';
    return JSON.stringify(
      {
        mcpServers: {
          [info.mcp.server_name]: {
            type: 'http',
            url: info.mcp.url,
            headers: { Authorization: 'Bearer <网关 Key>' },
          },
        },
      },
      null,
      2,
    );
  }, [info]);

  const copySnippet = async (): Promise<void> => {
    if (!mcpSnippet) return;
    try {
      await navigator.clipboard.writeText(mcpSnippet);
      addToast(t('已复制'));
    } catch {
      addToast(t('复制失败，请手动选择后复制'), 'error');
    }
  };

  const canSubmit = useMemo(() => {
    if (selected.size === 0) return false;
    if (uninstall) return true;
    if (!apiKeyId) return false;
    if (mode === 'skill' && !variant) return false;
    if (imageBridge && !outputDirectory.trim()) return false;
    return true;
  }, [selected, uninstall, apiKeyId, mode, variant, imageBridge, outputDirectory]);

  const toggle = (key: string): void => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const toggleAll = (): void => {
    setSelected(
      allSelected ? new Set() : new Set(selectableTargets.map((target) => target.key)),
    );
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
        if (imageBridge && !outputDirectory.trim()) {
          setError(t('请选择图片输出目录'));
          return;
        }
        const res = await api.installToolMcp({
          service_id: serviceIdOf(service),
          mode: mcpMode,
          api_key_id: apiKeyId,
          clients: [...selected],
          ...(imageBridge
            ? {
                output_directory: outputDirectory.trim(),
                reference_directories: referenceDirectories
                  .split('\n')
                  .map((line) => line.trim())
                  .filter(Boolean),
              }
            : {}),
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
                <div className="tid-block-head">
                  <span className="tid-label">{t('客户端目录')}</span>
                  <span className="tid-count">
                    {t('已选择 {{count}} / {{total}} 个', { count: selected.size, total: targets.length })}
                  </span>
                  {selectableTargets.length > 0 ? (
                    <Button variant="outline" size="sm" onClick={toggleAll}>
                      {allSelected ? t('取消全选') : t('全选')}
                    </Button>
                  ) : null}
                </div>
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

              {!uninstall && mode === 'mcp' && selected.has('dsh') ? (
                <div className="tid-alert">
                  {t('DSH 使用 Agent 页当前选择的配置目标。写入后请在 DSH 中确认工具是否出现，必要时重启对应客户端。')}
                </div>
              ) : null}

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

              {!uninstall && mode === 'mcp' && supportsBridge && (
                <label className="tid-block">
                  <span className="tid-label">{t('接入方式')}</span>
                  <Select
                    value={mcpMode}
                    onChange={(e) => setMcpMode(e.target.value === 'bridge' ? 'bridge' : 'http')}
                    aria-label={t('接入方式')}
                  >
                    <option value="bridge">{t('本地桥接脚本')}</option>
                    <option value="http">{t('远程 HTTP（需客户端支持 HTTP MCP）')}</option>
                  </Select>
                  <span className="tid-note">
                    {service.id === 'image-generation'
                      ? t('本地桥接需要 Node.js 18+，自动读取授权目录的参考图并保存生成结果；HTTP 接入只返回下载地址。')
                      : t('本地桥接会在本机运行 node 脚本，支持直接传本地图片路径。')}
                  </span>
                </label>
              )}

              {!uninstall && imageBridge && (
                <>
                  <label className="tid-block">
                    <span className="tid-label">{t('图片输出目录')}</span>
                    <Input
                      value={outputDirectory}
                      onChange={(e) => setOutputDirectory(e.target.value)}
                      placeholder={t('已有目录，图片只会写入此目录及其子目录')}
                    />
                  </label>
                  <label className="tid-block">
                    <span className="tid-label">{t('参考图允许读取的目录（可选，每行一个）')}</span>
                    <Textarea
                      value={referenceDirectories}
                      rows={3}
                      onChange={(e) => setReferenceDirectories(e.target.value)}
                      placeholder={t('每行一个已存在的绝对目录')}
                    />
                  </label>
                </>
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

              {!uninstall && mode === 'mcp' && info ? (
                <div className="tid-block">
                  <div className="tid-block-head">
                    <span className="tid-label">{t('客户端配置示例（mcpServers）')}</span>
                    <Button variant="outline" size="sm" onClick={() => void copySnippet()}>
                      <Copy size={12} /> {t('复制')}
                    </Button>
                  </div>
                  <p className="tid-note">
                    {t('把下面这段合并到客户端配置的 mcpServers 里，并把 Key 换成你自己的。')}
                  </p>
                  <pre className="tid-snippet">{mcpSnippet}</pre>
                </div>
              ) : null}

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