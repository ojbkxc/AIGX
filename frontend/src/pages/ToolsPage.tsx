// Ported from Agent2API v3.5.0-beta.6 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// ToolsPage — MCP & Skills 列表页（管理员专属）。
// 信息结构对照 A2A ui-islands tools-page.tsx / tools-data.ts：
//   Card 内一张 8 列表格（服务名称 / 接入协议 / 状态 / 能力数量 / 版本 /
//   服务入口 / 操作 / 本机接入）+ 全部·Skill·MCP 过滤 + 刷新；
//   点「详情」进入 ToolDetail（内存态切换，不换路由）；安装/卸载走弹窗。
// 配置面板（搜索提供商 / 视觉 / 图片生成）已移入详情页对应 Tab。
//
// 契约锚点：A2A api/tools.rs（/api/tools/servers|install-targets|install-skill|
// install-mcp|uninstall-skill|uninstall-mcp|api-keys|install-info）。
// AIGX 后端由 T7 灌满，未就绪 404 → 降级。

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { Loader2, RefreshCw, ShieldCheck } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, EmptyState, Switch, Tabs, Tooltip } from '../components/ui';
import ToolDetail from '../components/tools/ToolDetail';
import ToolInstallDialog from '../components/tools/ToolInstallDialog';
import {
  PROTOCOL_LABEL,
  protocolDesc,
  protocolTone,
  serviceDesc,
  serviceName,
  toolPayload,
} from '../components/tools/tools-meta';
import type { ToolInstallTarget, ToolServiceItem } from '../types';
import './ToolsPage.css';

type ToolTab = 'all' | 'skill' | 'mcp';

/**
 * ToolsPage — MCP & Skills 列表。
 *
 * 列表只负责清单与安装/卸载；服务的启停、超时、配置与能力说明都在详情页。
 * 「本机接入」列在 AIGX 恒显示（服务端管理面板，「本机」= 服务器主机）。
 */
export default function ToolsPage(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [tab, setTab] = useState<ToolTab>('all');
  const [services, setServices] = useState<ToolServiceItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [notReady, setNotReady] = useState(false);
  const [error, setError] = useState('');
  const [toggling, setToggling] = useState<string | null>(null);
  const [targets, setTargets] = useState<Record<string, ToolInstallTarget[]>>({});
  /** 详情态：存 service.key，列表 reload 后从新对象派生，避免快照陈旧 */
  const [detailKey, setDetailKey] = useState<string | null>(null);
  /** 安装/卸载弹窗：{service, mode} */
  const [installTarget, setInstallTarget] = useState<{ service: ToolServiceItem; mode: 'skill' | 'mcp' } | null>(null);
  const [uninstallTarget, setUninstallTarget] = useState<{ service: ToolServiceItem; mode: 'skill' | 'mcp' } | null>(null);

  const loadTargets = async (rows: ToolServiceItem[]): Promise<void> => {
    const entries = await Promise.all(
      rows.map(async (row) => {
        try {
          const res = await api.getToolInstallTargets(row.id, row.protocol === 'skill' ? 'skill' : 'mcp');
          return [row.key, toolPayload<{ targets: ToolInstallTarget[] }>(res)?.targets ?? []] as const;
        } catch {
          return [row.key, []] as const;
        }
      }),
    );
    setTargets(Object.fromEntries(entries));
  };

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const res = await api.listToolServices();
      // API 是固定对外的接口，不作为 MCP & Skills 页的协议条目展示
      const rows = (toolPayload<ToolServiceItem[]>(res) ?? []).filter((row) => row.protocol !== 'api');
      setServices(rows);
      // 详情服务若已消失（后端下线），退回列表
      setDetailKey((current) => (current && rows.some((row) => row.key === current) ? current : null));
      setNotReady(false);
      setError('');
      // 安装目标探测（失败不影响主列表）
      void loadTargets(rows);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      setServices([]);
      setDetailKey(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggleService = async (service: ToolServiceItem, enabled: boolean): Promise<void> => {
    setToggling(service.key);
    setError('');
    try {
      await api.patchToolService(service.id, { protocol: service.protocol, enabled });
      addToast(t('服务状态已更新'));
      await load();
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
      await load();
    } finally {
      setToggling(null);
    }
  };

  const visible = useMemo(
    () => (tab === 'all' ? services : services.filter((s) => s.protocol === tab)),
    [services, tab],
  );

  const detail = detailKey ? services.find((service) => service.key === detailKey) ?? null : null;
  if (detail) {
    return (
      <ToolDetail
        service={detail}
        onBack={() => {
          setDetailKey(null);
          void load();
        }}
        onChanged={() => {
          void load();
        }}
      />
    );
  }

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('MCP & Skills')}</h1>
          <p>{t('将搜索、视觉等能力通过 MCP 或 Skills 接入客户端。')}</p>
        </div>
      </div>

      {notReady ? (
        <EmptyState
          icon="🧰"
          message={t('MCP & Skills 后端尚未就绪')}
          action={
            <p className="tp-empty-hint">
              {t('MCP & Skills 管理面（/api/tools/*）由后端 tools 模块提供，接口上线后本页自动可用。')}
            </p>
          }
        />
      ) : (
        <>
          <Card title={t('MCP & Skills')}>
            <div className="tp-toolbar">
              <Tabs<ToolTab>
                ariaLabel={t('接入协议')}
                items={[
                  { key: 'all', label: t('全部') },
                  { key: 'skill', label: 'Skill' },
                  { key: 'mcp', label: 'MCP' },
                ]}
                active={tab}
                onChange={(key) => setTab(key)}
              />
              <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
                {loading ? <Loader2 size={13} className="tp-spin" /> : <RefreshCw size={13} />}
                {t('刷新')}
              </Button>
            </div>

            {error && <div className="error-message">{error}</div>}

            {loading && services.length === 0 ? (
              <div className="tp-loading">
                <Loader2 size={14} className="tp-spin" />
                {t('正在加载 MCP & Skills…')}
              </div>
            ) : visible.length === 0 ? (
              <EmptyState icon="🧰" message={t('没有匹配的工具服务')} />
            ) : (
              <div className="table-wrapper">
                <table className="dense-table">
                  <thead>
                    <tr>
                      <th>{t('服务名称')}</th>
                      <th>{t('接入协议')}</th>
                      <th>{t('状态')}</th>
                      <th>{t('能力数量')}</th>
                      <th>{t('版本')}</th>
                      <th>{t('服务入口')}</th>
                      <th>{t('操作')}</th>
                      <th>{t('本机接入')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {visible.map((service) => {
                      const serviceTargets = targets[service.key] ?? [];
                      const installed = serviceTargets.filter((tgt) => tgt.installed).length;
                      const busy = toggling === service.key;
                      return (
                        <tr key={service.key}>
                          <td data-col={t('服务名称')}>
                            <div className="tp-name">
                              <strong>{serviceName(t, service.id)}</strong>
                              <span className="tp-name-desc">{serviceDesc(t, service.id)}</span>
                            </div>
                          </td>
                          <td data-col={t('接入协议')}>
                            <div className="tp-proto">
                              <Badge tone={protocolTone(service.protocol)}>{PROTOCOL_LABEL[service.protocol]}</Badge>
                              <span className="tp-proto-desc">{protocolDesc(t, service.protocol)}</span>
                            </div>
                          </td>
                          <td data-col={t('状态')}>
                            <div className="tp-status">
                              <Badge tone={service.enabled ? 'success' : 'neutral'}>
                                {service.enabled ? t('已启用') : t('已停用')}
                              </Badge>
                              <Switch
                                checked={service.enabled}
                                disabled={busy}
                                onChange={(next) => void toggleService(service, next)}
                                ariaLabel={t('启用 / 停用此服务')}
                              />
                            </div>
                          </td>
                          <td data-col={t('能力数量')}>{service.capability_count}</td>
                          <td data-col={t('版本')}>
                            <Badge tone="neutral">v{service.version}</Badge>
                          </td>
                          <td data-col={t('服务入口')}>
                            <code className="tp-endpoint">{service.endpoint}</code>
                          </td>
                          <td data-col={t('操作')}>
                            <Button variant="outline" size="sm" onClick={() => setDetailKey(service.key)}>
                              {t('详情')}
                            </Button>
                          </td>
                          <td data-col={t('本机接入')}>
                            <div className="tp-install-actions">
                              <Tooltip label={t('安装 / 更新到本机客户端目录（选客户端与网关 Key）')}>
                                <Button
                                  variant="outline"
                                  size="sm"
                                  disabled={!service.enabled || busy}
                                  onClick={() =>
                                    setInstallTarget({
                                      service,
                                      mode: service.protocol === 'mcp' ? 'mcp' : 'skill',
                                    })
                                  }
                                >
                                  {installed > 0 ? t('更新') : t('安装')}
                                </Button>
                              </Tooltip>
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={installed === 0 || busy}
                                onClick={() =>
                                  setUninstallTarget({
                                    service,
                                    mode: service.protocol === 'mcp' ? 'mcp' : 'skill',
                                  })
                                }
                              >
                                {t('卸载')}
                              </Button>
                              {installed > 0 ? (
                                <span
                                  className="tp-installed-count"
                                  title={t('已接入 {{count}} 个目标', { count: installed })}
                                >
                                  {installed}
                                </span>
                              ) : null}
                            </div>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </Card>

          <Card
            title={t('用户工具策略')}
            actions={
              <Link className="btn btn-outline btn-sm" to="/users?tab=tools">
                <ShieldCheck size={13} /> {t('前往配置')}
              </Link>
            }
          >
            <p className="tp-policy-hint">
              {t('终端用户可调用哪些工具、每个分组允许哪些工具，在「用户管理 → 工具权限」中统一配置（与这里的服务接入相互独立）。')}
            </p>
          </Card>
        </>
      )}

      {installTarget && (
        <ToolInstallDialog
          service={installTarget.service}
          mode={installTarget.mode}
          onClose={() => setInstallTarget(null)}
          onDone={() => {
            void load();
          }}
        />
      )}

      {uninstallTarget && (
        <ToolInstallDialog
          service={uninstallTarget.service}
          mode={uninstallTarget.mode}
          uninstall
          onClose={() => setUninstallTarget(null)}
          onDone={() => {
            void load();
          }}
        />
      )}
    </div>
  );
}
