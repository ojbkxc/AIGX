// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// ToolsPage — 工具服务页（管理员专属）。
// 信息结构对照 A2A ui-islands tools-page.tsx / tools-data.ts / tools-config.tsx：
//   Tab（全部 / Skill / MCP）、服务启停与超时、搜索提供商密钥管理 / 选择 / 测试、
//   视觉分析绑定（provider / model / 思考等级 / 内部 Key）、Skill/MCP 安装卸载、
//   运行时地址展示。交互与布局按 AIGX 自研组件重写，不整段拷 islands TSX。
//
// 契约锚点：A2A api/tools.rs（/api/tools/servers|providers|provider-selection|
// vision/config|install-targets|install-info|install-skill|install-mcp|
// uninstall-skill|uninstall-mcp|api-keys）。AIGX 后端由 T7 灌满，未就绪 404 → 降级。

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { Loader2, RefreshCw, Settings2, ShieldCheck } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Badge, Button, Card, EmptyState, Input, Loading, Switch, Tabs, Tooltip } from '../components/ui';
import ToolImageConfigPanel from '../components/tools/ToolImageConfigPanel';
import ToolInstallDialog from '../components/tools/ToolInstallDialog';
import ToolProviderPanel from '../components/tools/ToolProviderPanel';
import ToolVisionPanel from '../components/tools/ToolVisionPanel';
import type {
  ToolInstallInfo,
  ToolInstallTarget,
  ToolProtocol,
  ToolServiceItem,
} from '../types';
import './ToolsPage.css';

type ToolTab = 'all' | 'skill' | 'mcp';

/**
 * 服务 id → 名称 / 描述。
 *
 * 用**字面量 t() 调用**而不是「中文表 + t(变量)」：`t('中文')` 的字面量同时
 * 是 i18n 扫描器的登记点，写成运行期变量查表会让六份词典永远缺这几条。
 */
function serviceName(t: (s: string) => string, id: string): string {
  switch (id) {
    case 'web-search': return t('网络搜索');
    case 'vision': return t('视觉分析');
    case 'image-generation': return t('图片生成');
    default: return id;
  }
}

function serviceDesc(t: (s: string) => string, id: string): string {
  switch (id) {
    case 'web-search': return t('聚合标准搜索服务及已有账号的搜索能力；网页抓取仍使用标准服务。');
    case 'vision': return t('把图片与提示词交给后台配置的视觉模型分析，并把模型回复返回给调用方的大模型。');
    case 'image-generation': return t('使用已有账号生成或编辑图片，支持任务查询与本机文件保存。');
    default: return '';
  }
}

/** 接入协议说明（列表页共用；字面量 t() 登记） */
function protocolDesc(t: (s: string) => string, protocol: ToolProtocol): string {
  switch (protocol) {
    case 'skill': return t('打包为 Agent Skill，由面板安装到本机客户端目录。');
    case 'mcp': return t('作为远程 MCP 服务，在支持 MCP 的客户端配置里填写地址即可接入。');
    default: return t('提供 REST API，任何能发 HTTP 请求的程序都可以携带网关 Key 直接调用。');
  }
}

const PROTOCOL_LABEL: Record<ToolProtocol, string> = {
  skill: 'Skill',
  mcp: 'MCP',
  api: 'API',
};

function protocolTone(protocol: ToolProtocol): 'info' | 'success' | 'neutral' {
  if (protocol === 'skill') return 'info';
  if (protocol === 'mcp') return 'success';
  return 'neutral';
}

/**
 * ToolsPage — 工具服务。
 *
 * 一屏三段：服务清单（协议过滤）+ 搜索提供商（密钥 / 选择 / 测试）+ 视觉分析绑定。
 * 安装/卸载走弹窗（选客户端与 Key），运行时地址常显在服务行里方便复制。
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
  const [installInfo, setInstallInfo] = useState<Record<string, ToolInstallInfo>>({});
  /** 安装/卸载弹窗：{service, mode} */
  const [installTarget, setInstallTarget] = useState<{ service: ToolServiceItem; mode: 'skill' | 'mcp' } | null>(null);
  const [uninstallTarget, setUninstallTarget] = useState<{ service: ToolServiceItem; mode: 'skill' | 'mcp' } | null>(null);
  /** 超时编辑草稿（service key → 秒） */
  const [timeoutDrafts, setTimeoutDrafts] = useState<Record<string, string>>({});

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const res = await api.listToolServices();
      // API 是固定对外的接口，不作为本页协议条目展示（说明在接入文档里）
      const rows = (res?.data ?? []).filter((row) => row.protocol !== 'api');
      setServices(rows);
      setNotReady(false);
      setError('');
      // 安装目标探测（失败不影响主列表）
      void loadTargets(rows);
      void loadInstallInfo(rows);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      const is404 = /404|not found/i.test(msg);
      setNotReady(is404);
      setError(is404 ? '' : msg);
      setServices([]);
    } finally {
      setLoading(false);
    }
  }, []);

  const loadTargets = async (rows: ToolServiceItem[]): Promise<void> => {
    const entries = await Promise.all(
      rows
        .filter((row) => row.protocol !== 'api')
        .map(async (row) => {
          try {
            const res = await api.getToolInstallTargets(row.id, row.protocol === 'skill' ? 'skill' : 'mcp');
            return [row.key, res?.data?.targets ?? []] as const;
          } catch {
            return [row.key, []] as const;
          }
        }),
    );
    setTargets(Object.fromEntries(entries));
  };

  const loadInstallInfo = async (rows: ToolServiceItem[]): Promise<void> => {
    const entries = await Promise.all(
      rows.map(async (row) => {
        try {
          const res = await api.getToolInstallInfo(row.id);
          return [row.id, res?.data ?? null] as const;
        } catch {
          return [row.id, null] as const;
        }
      }),
    );
    const next: Record<string, ToolInstallInfo> = {};
    for (const [id, info] of entries) {
      if (info) next[id] = info;
    }
    setInstallInfo(next);
  };

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

  const saveTimeout = async (service: ToolServiceItem): Promise<void> => {
    const raw = timeoutDrafts[service.key];
    const parsed = Number.parseInt(raw ?? '', 10);
    if (!raw || Number.isNaN(parsed) || parsed < 1 || parsed > 3600) {
      addToast(t('超时需在 {{min}}–{{max}} 秒之间', { min: 1, max: 3600 }), 'error');
      setTimeoutDrafts((prev) => ({ ...prev, [service.key]: String(service.timeout_seconds) }));
      return;
    }
    if (parsed === service.timeout_seconds) return;
    setToggling(service.key);
    try {
      await api.patchToolService(service.id, { protocol: service.protocol, timeout_seconds: parsed });
      addToast(t('超时已保存'));
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

  if (loading && services.length === 0) {
    return <Loading text={t('读取工具服务')} />;
  }

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('工具服务')}</h1>
          <p>{t('将搜索、视觉等能力通过 MCP 或 Skills 接入客户端。')}</p>
        </div>
        <Button variant="outline" onClick={() => void load()} disabled={loading}>
          {loading ? <Loader2 size={13} className="tp-spin" /> : <RefreshCw size={13} />}
          {t('刷新')}
        </Button>
      </div>

      {error && <div className="error-message">{error}</div>}

      <Tabs<ToolTab>
        ariaLabel={t('接入协议')}
        items={[
          { key: 'all', label: t('全部') },
          { key: 'skill', label: 'Skill' },
          { key: 'mcp', label: 'MCP' },
        ]}
        active={tab}
        onChange={(k) => setTab(k)}
      />

      {notReady ? (
        <EmptyState
          icon="🧰"
          message={t('工具服务后端尚未就绪')}
          action={<p className="tp-empty-hint">{t('工具服务管理面（/api/tools/*）由后端 tools 模块提供，接口上线后本页自动可用。')}</p>}
        />
      ) : (
        <>
          <Card title={t('MCP & Skills')}>
            {visible.length === 0 ? (
              <EmptyState icon="🧰" message={t('没有匹配的工具服务')} />
            ) : (
              <div className="tp-service-list">
                {visible.map((service) => {
                  const serviceTargets = targets[service.key] ?? [];
                  const installed = serviceTargets.filter((tgt) => tgt.installed).length;
                  const info = installInfo[service.id];
                  const draft = timeoutDrafts[service.key] ?? String(service.timeout_seconds);
                  const busy = toggling === service.key;
                  return (
                    <div key={service.key} className={`tp-service${service.enabled ? '' : ' is-off'}`}>
                      <div className="tp-service-head">
                        <div className="tp-service-title">
                          <h3>{serviceName(t, service.id)}</h3>
                          <Badge tone={protocolTone(service.protocol)}>{PROTOCOL_LABEL[service.protocol]}</Badge>
                          <Badge tone="neutral">v{service.version}</Badge>
                          <Badge tone={service.enabled ? 'success' : 'neutral'}>
                            {service.enabled ? t('已启用') : t('已停用')}
                          </Badge>
                        </div>
                        <div className="tp-service-actions">
                          <Switch
                            checked={service.enabled}
                            disabled={busy}
                            onChange={(v) => void toggleService(service, v)}
                            ariaLabel={t('启用 / 停用此服务')}
                          />
                        </div>
                      </div>
                      <p className="tp-service-desc">{serviceDesc(t, service.id)}</p>
                      <p className="tp-service-protocol">{protocolDesc(t, service.protocol)}</p>

                      <div className="tp-service-grid">
                        <div className="tp-field">
                          <span className="tp-field-label">{t('能力数量')}</span>
                          <span className="tp-field-value">{service.capability_count}</span>
                        </div>
                        <div className="tp-field tp-field-wide">
                          <span className="tp-field-label">{t('服务入口')}</span>
                          <code className="tp-code">{service.endpoint}</code>
                        </div>
                        <div className="tp-field">
                          <span className="tp-field-label">{t('超时（秒）')}</span>
                          <div className="tp-timeout">
                            <Input
                              type="number"
                              min={1}
                              max={3600}
                              value={draft}
                              onChange={(e) =>
                                setTimeoutDrafts((prev) => ({ ...prev, [service.key]: e.target.value }))
                              }
                              onBlur={() => void saveTimeout(service)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter') {
                                  e.preventDefault();
                                  void saveTimeout(service);
                                }
                              }}
                              disabled={busy}
                              style={{ width: 90 }}
                            />
                          </div>
                        </div>
                      </div>

                      {info && (
                        <details className="tp-runtime">
                          <summary>{t('运行时地址（MCP 端点 / 桥接脚本）')}</summary>
                          <div>
                            <span>
                              {t('网关基址')} <code>{info.base_url}</code>
                            </span>
                            <span>
                              {t('MCP 端点')} <code>{info.mcp.url}</code>
                            </span>
                            {info.mcp.bridge_url ? (
                              <span>
                                {t('桥接脚本')} <code>{info.mcp.bridge_url}</code>
                              </span>
                            ) : null}
                            <span>
                              {t('工具')} {info.mcp.tools.join('、')}
                            </span>
                          </div>
                        </details>
                      )}

                      <div className="tp-service-foot">
                        <span className="tp-installed-hint">
                          {installed > 0 ? t('已接入 {{n}} 个客户端目录', { n: installed }) : t('尚未接入本机客户端')}
                        </span>
                        <div className="tp-service-foot-actions">
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
                              <Settings2 size={13} /> {installed > 0 ? t('更新') : t('安装')}
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
                        </div>
                      </div>

                      {service.id === 'image-generation' ? (
                        <div className="tp-service-detail">
                          <ToolImageConfigPanel onSharedError={setError} />
                        </div>
                      ) : null}
                    </div>
                  );
                })}
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

          <ToolProviderPanel />
          <ToolVisionPanel onSharedError={setError} />
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
