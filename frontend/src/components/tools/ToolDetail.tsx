// Ported from Agent2API v3.5.0-beta.6 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// MCP & Skills 详情页 —— 对照 A2A ui-islands tools-detail.tsx：
//   返回列表 → 服务头卡（协议 / 启用 / 能力数 / 版本 / 启停 / 超时）
//   → MCP 接入信息卡（仅 mcp 协议）→ 服务配置 / 服务能力 Tabs。
// 交互与布局按 AIGX 自研组件重写（Card / Badge / Button / Switch / Input / Tabs），
// 不整段拷 islands TSX / Tailwind；配置 Tab 复用现有三个工具面板。
// 契约：PATCH /api/tools/servers/{id}、GET /api/tools/install-info。

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { api } from '../../api';
import { useToast } from '../Toast';
import { Badge, Button, Card, Input, Switch, Tabs } from '../ui';
import ToolImageConfigPanel from './ToolImageConfigPanel';
import ToolProviderPanel from './ToolProviderPanel';
import ToolVisionPanel from './ToolVisionPanel';
import {
  PROTOCOL_LABEL,
  capabilityGroups,
  protocolDesc,
  protocolTone,
  serviceDesc,
  serviceName,
} from './tools-meta';
import type { ToolInstallInfo, ToolServiceItem } from '../../types';
import './ToolDetail.css';

type DetailTab = 'config' | 'capabilities';

interface ToolDetailProps {
  service: ToolServiceItem;
  onBack: () => void;
  /** 启停 / 超时保存成功后回调（父级重新拉列表） */
  onChanged: () => void;
}

/** MCP 接入信息卡：服务地址 / 暴露的工具 / 客户端配置示例 */
function McpConnectCard({ info }: { info: ToolInstallInfo }): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const snippet = JSON.stringify(
    {
      mcpServers: {
        [info.mcp.server_name]: {
          type: 'http',
          url: info.mcp.url,
          headers: { Authorization: 'Bearer <AIGX API Key>' },
        },
      },
    },
    null,
    2,
  );

  const copy = async (text: string): Promise<void> => {
    try {
      await navigator.clipboard.writeText(text);
      addToast(t('已复制'));
    } catch {
      addToast(t('复制失败，请手动选择后复制'), 'error');
    }
  };

  return (
    <Card title={t('MCP 接入信息')}>
      <div className="mcp-connect">
        <div className="mcp-connect-block">
          <div className="mcp-connect-label">{t('服务地址')}</div>
          <div className="mcp-connect-row">
            <code className="mcp-code">{info.mcp.url}</code>
            <Button variant="outline" size="sm" onClick={() => void copy(info.mcp.url)}>
              {t('复制')}
            </Button>
          </div>
          <p className="mcp-hint">
            {t('在支持远程 MCP 的客户端中填写该地址，并使用网关 Key 作为 Bearer 令牌。')}
          </p>
        </div>

        <div className="mcp-connect-block">
          <div className="mcp-connect-label">{t('暴露的工具')}</div>
          <div className="mcp-badges">
            {info.mcp.tools.map((name) => (
              <Badge key={name} tone="neutral">
                {name}
              </Badge>
            ))}
          </div>
        </div>

        <div className="mcp-connect-block">
          <div className="mcp-connect-head">
            <div className="mcp-connect-label">{t('客户端配置示例')}</div>
            <Button variant="outline" size="sm" onClick={() => void copy(snippet)}>
              {t('复制')}
            </Button>
          </div>
          <p className="mcp-hint">
            {t('把下面这段合并到客户端配置的 mcpServers 里，并把 Key 换成你自己的。')}
          </p>
          <pre className="mcp-snippet">{snippet}</pre>
        </div>
      </div>
    </Card>
  );
}

/**
 * ToolDetail — 单个 MCP & Skills 服务的详情。
 *
 * 详情是列表页的内存态切换（不换路由）；刷新页面回到列表可接受。
 * 启停 / 超时成功后调用 onChanged()，父级重拉列表后本组件按新对象同步。
 */
export default function ToolDetail({ service, onBack, onChanged }: ToolDetailProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [info, setInfo] = useState<ToolInstallInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [enabled, setEnabled] = useState(service.enabled);
  const [toggling, setToggling] = useState(false);
  const [timeoutDraft, setTimeoutDraft] = useState(String(service.timeout_seconds));
  const [savedTimeout, setSavedTimeout] = useState(service.timeout_seconds);
  const [savingTimeout, setSavingTimeout] = useState(false);
  const [tab, setTab] = useState<DetailTab>('config');

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError('');
    api
      .getToolInstallInfo(service.id)
      .then((res) => {
        if (alive) setInfo(res?.data ?? null);
      })
      .catch((err) => {
        if (alive) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [service.id, service.capability_count]);

  useEffect(() => {
    setTimeoutDraft(String(service.timeout_seconds));
    setSavedTimeout(service.timeout_seconds);
  }, [service.timeout_seconds]);

  useEffect(() => {
    setEnabled(service.enabled);
  }, [service.enabled]);

  const toggle = async (next: boolean): Promise<void> => {
    setToggling(true);
    setError('');
    try {
      await api.patchToolService(service.id, { protocol: service.protocol, enabled: next });
      setEnabled(next);
      addToast(t('服务状态已更新'));
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setToggling(false);
    }
  };

  const saveTimeout = async (): Promise<void> => {
    const parsed = Number.parseInt(timeoutDraft, 10);
    if (Number.isNaN(parsed) || parsed < 5 || parsed > 600) {
      addToast(t('超时需在 {{min}}–{{max}} 秒之间', { min: 5, max: 600 }), 'error');
      return;
    }
    if (parsed === savedTimeout) return;
    setSavingTimeout(true);
    setError('');
    try {
      await api.patchToolService(service.id, {
        protocol: service.protocol,
        timeout_seconds: parsed,
      });
      setSavedTimeout(parsed);
      addToast(t('工具执行超时已保存'));
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingTimeout(false);
    }
  };

  const capabilities = capabilityGroups(t, service.id).filter(
    (cap) =>
      service.id !== 'image-generation' ||
      cap.title !== t('编辑图片') ||
      Boolean(info?.mcp.tools.includes('edit_images')),
  );

  const timeoutNum = Number.parseInt(timeoutDraft, 10);

  return (
    <div className="mcp-detail">
      <Button variant="outline" size="sm" className="mcp-back" onClick={onBack}>
        ← {t('返回列表')}
      </Button>

      <Card
        title={
          <span className="mcp-detail-title">
            {serviceName(t, service.id)}
            <Badge tone={protocolTone(service.protocol)}>{PROTOCOL_LABEL[service.protocol]}</Badge>
            <Badge tone={enabled ? 'success' : 'neutral'}>
              {enabled ? t('已启用') : t('已停用')}
            </Badge>
            <span className="mcp-detail-meta">
              {t('能力数量')}: {service.capability_count}
            </span>
            <span className="mcp-detail-meta">v{service.version}</span>
          </span>
        }
        actions={
          <span className="mcp-detail-toggle">
            <span className="mcp-detail-meta">{t('启用 / 停用此服务')}</span>
            <Switch
              checked={enabled}
              disabled={toggling}
              onChange={(next) => void toggle(next)}
              ariaLabel={t('启用 / 停用此服务')}
            />
          </span>
        }
      >
        <div className="mcp-detail-body">
          <p className="mcp-detail-desc">{serviceDesc(t, service.id)}</p>
          <p className="mcp-detail-desc">{protocolDesc(t, service.protocol)}</p>

          <div className="mcp-timeout-row">
            <div className="mcp-timeout-field">
              <span className="mcp-field-label">{t('工具执行超时')}</span>
              <Input
                type="number"
                min={5}
                max={600}
                value={timeoutDraft}
                onChange={(e) => setTimeoutDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    void saveTimeout();
                  }
                }}
                disabled={savingTimeout}
              />
            </div>
            <span className="mcp-seconds">{t('秒')}</span>
            <Button
              variant="outline"
              size="sm"
              disabled={
                loading ||
                savingTimeout ||
                !timeoutDraft ||
                Number.isNaN(timeoutNum) ||
                timeoutNum === savedTimeout
              }
              onClick={() => void saveTimeout()}
            >
              {savingTimeout ? <Loader2 size={12} className="mcp-spin" /> : null}
              {t('保存')}
            </Button>
          </div>
          <p className="mcp-hint">
            {service.id === 'image-generation'
              ? t('这是后台生成任务的时间上限。单次 MCP 调用最多等待 20 秒，未完成时返回任务编号；查询不会重新生图。')
              : t('服务端强制单次工具执行的超时上限（同一服务的 Skill / MCP 共用）。安装 MCP 时会按此值写入客户端配置；修改后需在客户端重新「更新」配置并重进会话才会生效。')}
          </p>

          {!enabled ? (
            <div className="mcp-alert-warn">{t('该协议已停用，已接入的客户端会收到服务停用错误。')}</div>
          ) : null}
          {error ? <div className="error-message">{error}</div> : null}
        </div>
      </Card>

      {loading ? (
        <div className="mcp-loading">
          <Loader2 size={14} className="mcp-spin" />
          {t('正在加载 MCP & Skills 配置…')}
        </div>
      ) : service.protocol === 'mcp' && info ? (
        <McpConnectCard info={info} />
      ) : service.protocol === 'skill' ? (
        <div className="mcp-alert-info">{t('该协议通过 AIGX 面板安装到本机客户端目录。')}</div>
      ) : null}

      <Tabs<DetailTab>
        ariaLabel={t('服务配置')}
        items={[
          { key: 'config', label: t('服务配置') },
          { key: 'capabilities', label: t('服务能力') },
        ]}
        active={tab}
        onChange={(key) => setTab(key)}
      />

      {tab === 'config' ? (
        service.id === 'image-generation' ? (
          <ToolImageConfigPanel serviceEnabled={enabled} onSharedError={setError} />
        ) : service.id === 'vision' ? (
          <ToolVisionPanel onSharedError={setError} />
        ) : (
          <ToolProviderPanel />
        )
      ) : (
        <Card
          title={t('服务能力')}
          actions={<span className="mcp-detail-meta">{t('共 {{count}} 项能力', { count: service.capability_count })}</span>}
        >
          <div className="mcp-cap-grid">
            {capabilities.map((cap) => (
              <div key={cap.title} className="mcp-cap">
                <div className="mcp-cap-title">{cap.title}</div>
                <p className="mcp-cap-desc">{cap.desc}</p>
                <div className="mcp-params">
                  {cap.params.map((param) => (
                    <div key={param.name} className="mcp-param">
                      <code className="mcp-param-name">{param.name}</code>
                      <span className="mcp-param-type">{param.type}</span>
                      <span className={param.required ? 'mcp-param-req' : 'mcp-param-opt'}>
                        {param.required ? t('必填') : t('可选')}
                      </span>
                      <span className="mcp-param-desc">{param.desc}</span>
                    </div>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </Card>
      )}
    </div>
  );
}
