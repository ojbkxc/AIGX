// Ported from Agent2API v3.5.0-beta.6 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// Skill / MCP 安装与卸载弹窗（MCP & Skills 列表页「安装 / 更新 / 卸载」入口）。
// 对照 A2A ui-islands tools-install.tsx：选客户端目录 + 选网关 Key（安装时才
// 取明文 Key 写进客户端配置），提交后逐客户端回执；全部成功自动关闭。
// MCP 配置示例已移到详情页「MCP 接入信息」卡，本弹窗不再重复展示。
// 契约：GET /api/tools/install-targets、GET /api/tools/api-keys、
//       POST /api/tools/install-skill|install-mcp|uninstall-skill|uninstall-mcp。

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import { Badge, Button, Input, Select, Textarea } from '../ui';
import { serviceName, toolPayload } from './tools-meta';
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
 * 卸载：只列已安装客户端，不选 Key。两个方向都按客户端逐条回执（部分失败如实展示）。
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
        const [targetRes, keyRes] = await Promise.all([
          api.getToolInstallTargets(serviceIdOf(service), mode),
          uninstall ? Promise.resolve(null) : api.listToolApiKeys().catch(() => null),
        ]);
        if (!alive) return;
        const list = toolPayload<{ targets: ToolInstallTarget[] }>(targetRes)?.targets ?? [];
        // 卸载只列已安装的目标；安装展示全部（unsupported 置灰）
        const shown = uninstall ? list.filter((item) => item.installed) : list;
        setTargets(shown);
        setSelected(
          new Set(
            shown
              .filter((item) => (uninstall ? item.installed : item.detected || item.installed) && item.supported !== false)
              .map((item) => item.key),
          ),
        );
        const keyList = toolPayload<{ keys: ToolApiKeyOption[] }>(keyRes)?.keys ?? [];
        setKeys(keyList);
        if (keyList.length > 0) setApiKeyId(keyList[0].id);
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

  /** 目标状态文案（上游三态） */
  const targetState = (target: ToolInstallTarget): string => {
    if (!target.detected) return t('未检测到');
    return target.installed ? t('已安装') : t('已检测到');
  };

  const submit = async (): Promise<void> => {
    if (selected.size === 0) {
      setError(t('请至少选择一个目标'));
      return;
    }
    if (!uninstall && !apiKeyId) {
      setError(t('请先选择 API Key'));
      return;
    }
    setSubmitting(true);
    setError('');
    try {
      if (uninstall) {
        const body = { service_id: serviceIdOf(service), clients: [...selected] };
        const res =
          mode === 'mcp' ? await api.uninstallToolMcp(body) : await api.uninstallToolSkill(body);
        const results = toolPayload<{ results: ToolInstallOutcome[] }>(res)?.results ?? [];
        setOutcomes(results);
        const failed = results.filter((r) => !r.ok);
        const okCount = results.length - failed.length;
        if (okCount > 0) {
          addToast(t('已处理 {{count}} 个目标', { count: okCount }));
          onDone();
          if (failed.length === 0) onClose();
        }
        if (failed.length > 0) {
          setError(failed.map((r) => `${r.label}：${r.error || t('卸载失败')}`).join('\n'));
        }
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
        const results = toolPayload<{ results: ToolInstallOutcome[] }>(res)?.results ?? [];
        setOutcomes(results);
        const failed = results.filter((r) => !r.ok);
        const okCount = results.length - failed.length;
        if (okCount > 0) {
          addToast(t('已写入 {{count}} 个客户端配置', { count: okCount }));
          onDone();
          if (failed.length === 0) onClose();
        }
        if (failed.length > 0) {
          setError(failed.map((r) => `${r.label}：${r.error || t('写入失败')}`).join('\n'));
        }
      } else {
        const res = await api.installToolSkill({
          variant,
          api_key_id: apiKeyId,
          clients: [...selected],
        });
        const results = toolPayload<{ results: ToolInstallOutcome[] }>(res)?.results ?? [];
        setOutcomes(results);
        const failed = results.filter((r) => !r.ok);
        const okCount = results.length - failed.length;
        if (okCount > 0) {
          addToast(t('已安装到 {{count}} 个客户端', { count: okCount }));
          onDone();
          if (failed.length === 0) onClose();
        }
        if (failed.length > 0) {
          setError(failed.map((r) => `${r.label}：${r.error || t('写入失败')}`).join('\n'));
        }
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      addToast(msg, 'error');
    } finally {
      setSubmitting(false);
    }
  };

  const label = serviceName(t, service.id);
  const title = uninstall
    ? t('移除 {{name}} 配置', { name: label })
    : mode === 'skill'
      ? t('安装 {{name}}', { name: label })
      : t('{{name}} 接入客户端', { name: label });

  return (
    <div className="tid-overlay" role="dialog" aria-modal="true" aria-label={title}>
      <div className="tid-panel">
        <div className="tid-head">
          <div>
            <h2>{title}</h2>
          </div>
          <Badge tone={mode === 'skill' ? 'info' : 'success'}>{mode === 'skill' ? 'Skill' : 'MCP'}</Badge>
        </div>

        <div className="tid-body">
          {loading ? (
            <div className="tid-loading">
              <Loader2 size={14} className="tid-spin" />
              {t('正在加载安装目标…')}
            </div>
          ) : (
            <>
              <p className="tid-note">
                {uninstall
                  ? t('选择要从中移除的客户端，未勾选的客户端保持不变。')
                  : mode === 'skill'
                    ? t('选择安装目标、运行时与调用用的 API Key。Skill 包与密钥只写入所选客户端的本机目录。')
                    : t('选择要写入 MCP 配置的客户端。配置只动本服务那一项，客户端原有的其它 MCP 服务保持不变。')}
              </p>

              {error && (
                <div className="error-message" style={{ whiteSpace: 'pre-wrap' }}>
                  {error}
                </div>
              )}

              <div className="tid-block">
                <div className="tid-block-head">
                  <span className="tid-label">{uninstall ? t('客户端目录') : t('安装目标')}</span>
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
                  <p className="tid-empty">
                    {uninstall ? t('当前没有已安装/已配置的目标') : t('当前没有可用的安装目标')}
                  </p>
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
                            <Badge tone={target.installed ? 'info' : target.detected ? 'success' : 'neutral'}>
                              {targetState(target)}
                            </Badge>
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
                  <span className="tid-note">{t('Node.js 无需额外依赖，推荐使用；Python 需要本机有 python。')}</span>
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
                      ? t('本地桥接需要 Node.js 18 或更新版本，自动读取授权目录的参考图并保存生成结果；HTTP 接入只返回下载地址。')
                      : mcpMode === 'bridge'
                        ? t('本地桥接会在本机运行一个 node 脚本，支持直接传本地图片路径，推荐在桌面端使用。')
                        : t('远程 HTTP 只接受图片 URL 或 data URI，不需要本机安装 node。')}
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
                      placeholder={t('选择已有目录，图片只会写入此目录及其子目录')}
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
                    <span className="tid-note">
                      {t('可选，每行一个已有绝对目录。输出目录默认允许读取；其他位置的文件不会读取或上传。')}
                    </span>
                  </label>
                  <div className="tid-alert">
                    {t('只保存新文件，不覆盖已有不同内容。目录权限写入客户端配置，更新配置时需要重新确认。')}
                  </div>
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
                  <span className="tid-note">{t('Key 明文只会写入所选客户端的本机配置文件。')}</span>
                </label>
              )}

              {!uninstall && service.id === 'image-generation' ? (
                <div className="tid-alert">
                  {t('客户端调用超时设为 120 秒；后台生成超时由服务配置控制，未完成时通过任务编号查询。')}
                </div>
              ) : !uninstall && service.timeout_seconds ? (
                <div className="tid-alert">
                  {t('将按服务的工具执行超时（{{seconds}} 秒）同步写入客户端配置。', {
                    seconds: service.timeout_seconds,
                  })}
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
            {t('取消')}
          </Button>
          <Button
            variant={uninstall ? 'danger' : 'primary'}
            size="sm"
            disabled={!canSubmit || submitting || loading}
            onClick={() => void submit()}
          >
            {submitting ? <Loader2 size={12} className="tid-spin" /> : null}
            {uninstall ? t('卸载') : mode === 'skill' ? t('安装') : t('写入配置')}
          </Button>
        </div>
      </div>
    </div>
  );
}
