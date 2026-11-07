/**
 * KeysDocsPanel — 接入文档面板（「API 密钥」页第二 Tab）。
 *
 * 2026-11-07 自原独立「文档」页并入（07 报告 E.3）：密钥与文档是同一件事的
 * 两半（拿 key → 怎么填），合并后少一个顶级菜单。内容三块不变：
 * 1. Base URL：管理员读网关监听地址（/api/gateway/settings 的 host/port，
 *    通配地址 0.0.0.0/::/127.0.0.1/localhost 时退回站点 origin）；普通用户
 *    无权读该端点（adminOnly），直接用 window.location.origin —— 与管理员
 *    的通配回退同一条路径，展示口径一致。
 * 2. 对话协议三种：OpenAI Chat Completions / OpenAI Responses /
 *    Anthropic Messages——每种的端点路径与鉴权头（后端三个数据面端点均在）。
 * 3. 模型列表：/api/models/available（登录态可用、与 /v1/models 同口径）。
 *    管理员看全量聚合；普通用户看到的也是全量（该端点未按 key 白名单过滤），
 *    但**自己 Key 的模型白名单**以密钥列表里的为准——这里在模型清单旁提示。
 *
 * 角色分层结论（07 E.2）：上游无角色概念（单管理员桌面），AIGX 有角色分离，
 * 不能照抄单视图。本 Tab 对所有登录用户可见（Keys 走 ProtectedLayout）；
 * 管理员额外看到「网关监听/鉴权口径」卡片（gateway/settings 是 adminOnly）。
 */
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Copy, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Button, Card, EmptyState, Loading } from '../components/ui';
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

export default function KeysDocsPanel(): JSX.Element {
  const { t } = useTranslation();
  const admin = isAdmin();
  const addToast = useToast();

  // ── Base URL（管理员：网关监听地址；普通用户：站点 origin）──────────
  const [baseUrl, setBaseUrl] = useState('');
  const [gwHost, setGwHost] = useState('');
  const [gwPort, setGwPort] = useState<number | null>(null);
  useEffect(() => {
    if (!admin) {
      // 普通用户无权读 /api/gateway/settings（adminOnly）：站点 origin 就是
      // 客户端当前能访问的地址，与管理员读到通配监听时的回退口径一致。
      setBaseUrl(window.location.origin);
      return;
    }
    api.getGatewaySettings()
      .then((res) => {
        const gs = res?.data as GatewaySettingsItem | undefined;
        const host = gs?.host?.trim() || '';
        if (host && !WILDCARD_HOSTS.includes(host.toLowerCase())) {
          setGwHost(host);
          setGwPort(gs?.port ?? null);
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
    void loadModels();
  }, [loadModels]);

  return (
    <div className="keys-docs">
      {/* Base URL */}
      <Card title={t('Base URL（网关地址）')}>
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
        {!admin && (
          <p className="docs-note">
            {t('普通用户视角：地址即当前站点。监听地址等网关参数由管理员在「系统设置 → 网关」维护。')}
          </p>
        )}
      </Card>

      {/* 对话协议 */}
      <Card title={t('对话协议（按客户端支持的类型选一行）')}>
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
      </Card>

      {/* 模型列表 */}
      <Card
        title={t('模型列表')}
        actions={
          <Button
            variant="outline"
            size="sm"
            onClick={() => void loadModels()}
            disabled={loadingModels}
            style={{ gap: 6 }}
          >
            <RefreshCw size={13} className={loadingModels ? 'docs-spin' : undefined} /> {t('刷新')}
          </Button>
        }
      >
        <p className="docs-note">
          {t('模型来自全部启用渠道的聚合（与 /v1/models 同口径）；客户端自动拉取可用模型，配 Key 之前就能拉')}
        </p>
        <p className="docs-note">
          {t('若你的密钥配置了「模型白名单」，实际可调用的模型以密钥列表里该白名单为准。')}
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
      </Card>

      {/* 管理员视角：网关监听口径（gateway/settings 为 adminOnly，普通用户不拉） */}
      {admin && (
        <Card title={t('网关监听（仅管理员可见）')}>
          <p className="docs-note">
            {gwHost
              ? t('网关监听 {{host}}:{{port}}（本页 Base URL 按该地址展示；修改请到「系统设置 → 网关」）', { host: gwHost, port: gwPort ?? 8080 })
              : t('网关监听为通配地址（0.0.0.0 等），本页 Base URL 回退为当前站点 origin；监听地址在「系统设置 → 网关」维护。')}
          </p>
        </Card>
      )}

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
