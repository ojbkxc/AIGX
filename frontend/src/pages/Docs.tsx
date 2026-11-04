/**
 * 文档页（接口接入指引）—— 对照 Agent2API「接口地址」页。
 *
 * 三块内容：
 * 1. Base URL：网关监听地址（/api/gateway/settings 的 host/port；通配
 *    地址 0.0.0.0/::/127.0.0.1/localhost 时退回站点 origin，保证复制
 *    出去的地址客户端真的能访问）。
 * 2. 对话协议三种：OpenAI Chat Completions / OpenAI Responses /
 *    Anthropic Messages——每种的端点路径与鉴权头。后端三个数据面端点
 *    均存在（/v1/chat/completions、/v1/responses、/v1/messages）。
 * 3. 模型列表：从 /api/models/available 拉取。数据面 /v1/models 需要
 *    sk-xxx 网关 Key 鉴权，浏览器登录态（Bearer JWT）调不了；
 *    /api/models/available 与 /v1/models 同口径（后端聚合逻辑一致），
 *    登录态可用，是最合适的数据来源。
 */
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Copy, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Button, EmptyState, Loading } from '../components/ui';
import { isAdmin } from '../lib/utils';
import type { GatewaySettingsItem, ModelInfo } from '../types';
import './Docs.css';

/** 通配监听地址：不是客户端可访问的地址，显示站点 origin 更实用 */
const WILDCARD_HOSTS = ['0.0.0.0', '::', '::0', '127.0.0.1', 'localhost'];

interface ProtocolEntry {
  /** 协议名（客户端固定叫法，不翻译） */
  name: string;
  /** 数据面端点路径（相对 Base URL） */
  path: string;
  /** 鉴权头写法 */
  auth: string;
  /** i18n 键：用途说明 */
  noteKey: string;
}

const PROTOCOLS: ProtocolEntry[] = [
  {
    name: 'OpenAI Chat Completions',
    path: '/v1/chat/completions',
    auth: 'Authorization: Bearer <key>',
    noteKey: '多数 OpenAI 兼容客户端的默认协议',
  },
  {
    name: 'OpenAI Responses',
    path: '/v1/responses',
    auth: 'Authorization: Bearer <key>',
    noteKey: 'OpenAI 新协议（Codex、新版 SDK）',
  },
  {
    name: 'Anthropic Messages',
    path: '/v1/messages',
    auth: 'x-api-key: <key>',
    noteKey: 'Claude Code / Anthropic SDK',
  },
];

export default function Docs(): JSX.Element {
  const { t } = useTranslation();
  const admin = isAdmin();
  const addToast = useToast();

  // ── Base URL（网关监听地址）────────────────────────────────────
  const [baseUrl, setBaseUrl] = useState('');
  useEffect(() => {
    if (!admin) return;
    api.getGatewaySettings()
      .then((res) => {
        const gs = res?.data as GatewaySettingsItem | undefined;
        const host = gs?.host?.trim() || '';
        if (host && !WILDCARD_HOSTS.includes(host.toLowerCase())) {
          setBaseUrl(`http://${host}:${gs?.port ?? 8080}`);
        } else {
          // 通配地址：显示用户当前访问站点的 origin
          setBaseUrl(window.location.origin);
        }
      })
      .catch(() => setBaseUrl(window.location.origin));
  }, [admin]);

  // ── 复制（navigator.clipboard + toast 反馈；不支持时退化为选区复制）──
  const copyText = useCallback((text: string): void => {
    const ok = (): void => addToast(t('已复制'));
    const fail = (): void => addToast(t('复制失败'), 'error');
    if (navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
      void navigator.clipboard.writeText(text).then(ok).catch(fail);
      return;
    }
    // 降级：临时 textarea + execCommand（file:// 或旧浏览器）
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    const copied = document.execCommand('copy');
    document.body.removeChild(ta);
    copied ? ok() : fail();
  }, [addToast, t]);

  // ── 模型列表 ──────────────────────────────────────────────────
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [loadingModels, setLoadingModels] = useState(true);
  const [modelError, setModelError] = useState('');

  const loadModels = useCallback(async (): Promise<void> => {
    setLoadingModels(true);
    setModelError('');
    try {
      const res = await api.listModels();
      setModels(Array.isArray(res?.data) ? res.data : []);
    } catch (err) {
      setModelError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoadingModels(false);
    }
  }, []);

  useEffect(() => {
    if (!admin) return;
    void loadModels();
  }, [admin, loadModels]);

  if (!admin) return <EmptyState message={t('无权限')} />;

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('文档')}</h1>
          <p>{t('接口接入指引：网关地址、三种对话协议与可用模型清单')}</p>
        </div>
      </div>

      {/* Base URL */}
      <div className="card">
        <div className="card-header">
          <h2>{t('Base URL（网关地址）')}</h2>
        </div>
        <div className="card-body">
          <div className="docs-endpoint-line">
            <code className="docs-code">{baseUrl || '—'}</code>
            <Button
              variant="outline"
              size="sm"
              onClick={() => baseUrl && copyText(baseUrl)}
              disabled={!baseUrl}
            >
              <Copy size={13} /> {t('复制')}
            </Button>
          </div>
          <p className="docs-note">{t('填进客户端的「API 地址 / Base URL」栏')}</p>
        </div>
      </div>

      {/* 对话协议 */}
      <div className="card">
        <div className="card-header">
          <h2>{t('对话协议（按客户端支持的类型选一行）')}</h2>
        </div>
        <div className="card-body">
          {PROTOCOLS.map((p) => (
            <div key={p.name} className="docs-protocol-row">
              <div className="docs-protocol-head">
                <span className="docs-protocol-name">{p.name}</span>
                <span className="docs-protocol-note">{t(p.noteKey)}</span>
              </div>
              <div className="docs-endpoint-line">
                <code className="docs-code">POST {baseUrl || '—'}{p.path}</code>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => baseUrl && copyText(`POST ${baseUrl}${p.path}`)}
                  disabled={!baseUrl}
                >
                  <Copy size={13} /> {t('复制')}
                </Button>
              </div>
              <div className="docs-auth-line">
                {t('鉴权方式')}：<code className="docs-code">{p.auth}</code>
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* 模型列表 */}
      <div className="card">
        <div className="card-header">
          <h2>{t('模型列表')}</h2>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void loadModels()}
            disabled={loadingModels}
            style={{ gap: 6 }}
          >
            <RefreshCw size={13} className={loadingModels ? 'docs-spin' : undefined} /> {t('刷新')}
          </Button>
        </div>
        <div className="card-body">
          <p className="docs-note">
            {t('模型来自全部启用渠道的聚合（与 /v1/models 同口径）；客户端自动拉取可用模型，配 Key 之前就能拉')}
          </p>
          {loadingModels ? (
            <Loading text={t('加载模型列表')} />
          ) : modelError ? (
            <EmptyState
              message={modelError}
              action={<Button size="sm" onClick={() => void loadModels()}>{t('重试')}</Button>}
            />
          ) : models.length === 0 ? (
            <EmptyState message={t('暂无可用模型。请先在渠道管理或账号池里配置上游')} />
          ) : (
            <>
              <div className="docs-model-count">{t('共 {{n}} 个模型', { n: models.length })}</div>
              <div className="docs-model-grid">
                {models.map((m) => (
                  <div key={m.id} className="docs-model-item">
                    <span className="docs-model-name">{m.id}</span>
                    {m.owned_by && <span className="docs-model-owner">{m.owned_by}</span>}
                    <Button
                      variant="outline"
                      size="sm"
                      title={t('复制模型 ID')}
                      onClick={() => copyText(m.id)}
                    >
                      <Copy size={13} />
                    </Button>
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
      </div>

      {/* 鉴权脚注 */}
      <p className="docs-foot">
        {t('鉴权用「API 密钥」页里任一启用的 Key：请求带 ')}
        <code className="docs-code">{'Authorization: Bearer <key>'}</code>
        {t(' 或 ')}
        <code className="docs-code">{'x-api-key: <key>'}</code>
        {t('。一把启用的 Key 都没有时不鉴权（默认网关只监听 127.0.0.1；在设置页开启「局域网访问」后监听所有网卡，鉴权与安全闸门随之启用）。')}
      </p>
    </div>
  );
}
