import { useState, useEffect, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { Boxes, Search, Copy, Layers, Plus, Pencil, Trash2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Card, Loading, EmptyState, Badge } from '../components/ui';
import { isAdmin } from '../lib/utils';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import type { ModelInfo, ModelMetaOverride } from '../types';
import './Models.css';

/** 模型能力标记（capabilities 数组里出现的关键词 → 展示徽章） */
const CAPABILITY_LABELS: Array<{ match: string; labelKey: string; tone: 'success' | 'info' | 'warning' }> = [
  { match: 'vision', labelKey: '视觉', tone: 'info' },
  { match: 'image', labelKey: '图像', tone: 'info' },
  { match: 'audio', labelKey: '音频', tone: 'warning' },
  { match: 'tool', labelKey: '工具调用', tone: 'success' },
  { match: 'function', labelKey: '函数调用', tone: 'success' },
  { match: 'reasoning', labelKey: '推理', tone: 'success' },
  { match: 'embedding', labelKey: '向量', tone: 'warning' },
];

function fmtContextLength(n: number | null | undefined): string {
  if (n == null || n <= 0) return '—';
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
  if (n >= 1_000) return (n / 1_000).toFixed(0) + 'K';
  return String(n);
}

function fmtPrice(p: number | null | undefined): string {
  if (p == null || p === 0) return '—';
  if (p < 0.01) return p.toFixed(4);
  if (p < 1) return p.toFixed(3);
  return p.toFixed(2);
}

function renderPrice(m: ModelInfo, t: (k: string) => string): JSX.Element {
  const input = m.price_input as number | undefined;
  const output = m.price_output as number | undefined;
  const type = (m.price_type as string | undefined) || 'token';
  if ((input == null || input === 0) && (output == null || output === 0)) {
    return <span className="model-price-empty">{t('未定价')}</span>;
  }
  if (type === 'count') {
    return <span className="model-price">{t('¥/次')} {fmtPrice(input)}</span>;
  }
  return (
    <span className="model-price">
      {t('输入')} {fmtPrice(input)} / {t('输出')} {fmtPrice(output)}
    </span>
  );
}

export default function Models(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [query, setQuery] = useState('');

  // ── 元信息覆盖管理（P1 收尾：管理员可覆盖 owned_by/上下文长度/能力）──
  const [overrides, setOverrides] = useState<Record<string, ModelMetaOverride>>({});
  const [showMetaModal, setShowMetaModal] = useState(false);
  const [metaModel, setMetaModel] = useState('');
  const [metaOwnedBy, setMetaOwnedBy] = useState('');
  const [metaContext, setMetaContext] = useState('');
  const [metaCaps, setMetaCaps] = useState('');
  const [metaError, setMetaError] = useState('');
  const [metaSaving, setMetaSaving] = useState(false);
  const [metaConfirm, setMetaConfirm] = useState<ConfirmState | null>(null);

  const loadOverrides = async () => {
    try {
      const res = await api.listModelMeta();
      setOverrides(res?.data ?? {});
    } catch {
      // 覆盖列表加载失败不阻塞模型目录
    }
  };

  useEffect(() => {
    void loadOverrides();
  }, []);

  const openMetaCreate = () => {
    setMetaModel(''); setMetaOwnedBy(''); setMetaContext(''); setMetaCaps('');
    setMetaError(''); setShowMetaModal(true);
  };

  const openMetaEdit = (model: string) => {
    const o = overrides[model];
    if (!o) return;
    setMetaModel(model);
    setMetaOwnedBy(o.owned_by ?? '');
    setMetaContext(o.context_length != null ? String(o.context_length) : '');
    setMetaCaps((o.capabilities ?? []).join(', '));
    setMetaError('');
    setShowMetaModal(true);
  };

  const handleMetaSave = async () => {
    setMetaError('');
    if (!metaModel.trim()) { setMetaError(t('模型 ID 为必填项')); return; }
    if (!metaOwnedBy.trim()) { setMetaError(t('归属方为必填项')); return; }
    setMetaSaving(true);
    try {
      const ctx = metaContext.trim() ? Number(metaContext.trim()) : null;
      if (metaContext.trim() && (ctx === null || !Number.isFinite(ctx) || ctx < 0)) {
        setMetaError(t('上下文长度需为非负整数'));
        setMetaSaving(false);
        return;
      }
      const meta: ModelMetaOverride = {
        owned_by: metaOwnedBy.trim(),
        context_length: ctx,
        capabilities: metaCaps.split(/[,，]/).map((c) => c.trim()).filter(Boolean),
      };
      await api.setModelMeta(metaModel.trim(), meta);
      addToast(t('元信息覆盖已保存'));
      setShowMetaModal(false);
      await loadOverrides();
      await load();
    } catch (err) {
      setMetaError(err instanceof Error ? err.message : String(err));
    } finally {
      setMetaSaving(false);
    }
  };

  const handleMetaDelete = (model: string) => {
    setMetaConfirm({
      title: t('删除元信息覆盖'),
      message: t('删除后将回落到内置推断') + '：' + model,
      confirmText: t('删除'),
      danger: true,
      onConfirm: async () => {
        await api.deleteModelMeta(model);
        addToast(t('已删除'));
        await loadOverrides();
        await load();
      },
    });
  };

  useEffect(() => {
    void load();
  }, []);

  const load = async () => {
    setLoading(true);
    setError('');
    try {
      const res = await api.listModels();
      setModels(res?.data ?? []);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const q = query.trim().toLowerCase();
  const visible = useMemo(() => {
    if (!q) return models;
    return models.filter((m) => {
      const haystack = [
        m.id,
        m.owned_by,
        (m.capabilities ?? []).join(' '),
      ].filter(Boolean).join(' ').toLowerCase();
      return haystack.includes(q);
    });
  }, [models, q]);

  const ownedBySet = useMemo(() => {
    const set = new Set<string>();
    for (const m of models) if (m.owned_by) set.add(m.owned_by);
    return set;
  }, [models]);

  // 复制模型 ID：HTTP 部署下 navigator.clipboard 不可用，参照 Keys.tsx 降级 execCommand
  const handleCopy = (id: string) => {
    const fallbackCopy = (): boolean => {
      try {
        const ta = document.createElement('textarea');
        ta.value = id;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        const ok = document.execCommand('copy');
        document.body.removeChild(ta);
        return ok;
      } catch {
        return false;
      }
    };
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(id).then(() => {
        addToast(t('已复制到剪贴板'));
      }).catch(() => {
        if (!fallbackCopy()) addToast(t('复制失败，请手动选择复制'), 'error');
        else addToast(t('已复制到剪贴板'));
      });
    } else if (fallbackCopy()) {
      addToast(t('已复制到剪贴板'));
    } else {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('模型与价格')}</h1>
          <p>{t('网关聚合的可用模型目录（来自启用渠道的 models 声明）')}</p>
        </div>
        <button className="btn btn-outline btn-sm" onClick={() => void load()} disabled={loading} style={{ gap: 6 }}>
          {loading ? t('加载中...') : t('刷新')}
        </button>
      </div>

      {/* 管理员提示：价格在此只读展示，编辑入口在 系统设置 → 计费/易支付 */}
      {isAdmin() && (
        <div className="notify-note" style={{ fontSize: 12.5, lineHeight: 1.7 }}>
          {t('价格在此为只读展示。')}{' '}
          <Link to="/pricing">{t('编辑定价请前往 定价倍率页')}</Link>
        </div>
      )}

      {error && <div className="error-message">{error}</div>}

      {/* 汇总行 */}
      <div className="models-stats">
        <div className="models-stat">
          <Boxes size={16} className="models-stat-icon" />
          <div>
            <div className="models-stat-value">{models.length}</div>
            <div className="models-stat-label">{t('模型总数')}</div>
          </div>
        </div>
        <div className="models-stat">
          <Layers size={16} className="models-stat-icon" />
          <div>
            <div className="models-stat-value">{ownedBySet.size}</div>
            <div className="models-stat-label">{t('厂商')}</div>
          </div>
        </div>
      </div>

      <Card bodyClassName="">
        <div className="models-toolbar">
          <div className="models-search">
            <Search size={14} className="models-search-icon" />
            <input
              className="form-input"
              placeholder={t('搜索模型名称 / 厂商 / 能力…')}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              aria-label={t('搜索模型')}
            />
          </div>
          <span className="models-count">{t('显示')} {visible.length} / {models.length}</span>
        </div>

        {loading ? (
          <Loading text={t('加载模型目录')} />
        ) : visible.length === 0 ? (
          <EmptyState
            icon="📦"
            message={q ? t('没有匹配的模型') : t('暂无可用模型，请先启用渠道并声明模型')}
          />
        ) : (
          <div className="models-grid">
            {visible.map((m) => (
              <div key={m.id} className="model-card">
                <div className="model-card-head">
                  <strong className="model-card-id">{m.id}</strong>
                  <button
                    type="button"
                    className="btn btn-outline btn-sm model-copy-btn"
                    onClick={() => handleCopy(m.id)}
                    title={t('复制模型 ID')}
                    aria-label={t('复制模型 ID')}
                  >
                    <Copy size={13} />
                  </button>
                </div>
                <div className="model-card-meta">
                  {m.owned_by && <Badge tone="neutral">{m.owned_by}</Badge>}
                  <span className="model-card-context">{t('上下文')} {fmtContextLength(m.context_length)}</span>
                </div>
                <div className="model-card-price">
                  {renderPrice(m, t)}
                </div>
                {(m.capabilities && m.capabilities.length > 0) && (
                  <div className="model-card-caps">
                    {CAPABILITY_LABELS
                      .filter((c) => (m.capabilities ?? []).some((cap) => cap.toLowerCase().includes(c.match)))
                      .map((c) => (
                        <Badge key={c.match} tone={c.tone}>{t(c.labelKey)}</Badge>
                      ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </Card>

      {/* ── 元信息覆盖管理（P1 收尾）── */}
      <Card
        title={t('元信息覆盖')}
        actions={
          <button className="btn btn-outline btn-sm" onClick={openMetaCreate} style={{ gap: 6 }}>
            <Plus size={13} />
            {t('新增覆盖')}
          </button>
        }
      >
        {Object.keys(overrides).length === 0 ? (
          <EmptyState
            icon="🏷️"
            message={t('暂无覆盖项。内置推断已覆盖常见模型，需要自定义归属/上下文/能力时在此添加')}
          />
        ) : (
          <div className="models-grid">
            {Object.entries(overrides).map(([model, o]) => (
              <div key={model} className="model-card">
                <div className="model-card-head">
                  <strong className="model-card-id">{model}</strong>
                  <div style={{ display: 'flex', gap: 6 }}>
                    <button
                      type="button"
                      className="btn btn-outline btn-sm"
                      onClick={() => openMetaEdit(model)}
                      title={t('编辑')}
                    >
                      <Pencil size={13} />
                    </button>
                    <button
                      type="button"
                      className="btn btn-danger btn-sm"
                      onClick={() => handleMetaDelete(model)}
                      title={t('删除')}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                </div>
                <div className="model-card-meta">
                  <Badge tone="info">{o.owned_by}</Badge>
                  <span className="model-card-context">{t('上下文')} {o.context_length ?? '—'}</span>
                </div>
                {(o.capabilities && o.capabilities.length > 0) && (
                  <div className="model-card-caps">
                    {o.capabilities.map((c) => <Badge key={c} tone="neutral">{c}</Badge>)}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </Card>

      {showMetaModal && (
        <div className="modal-overlay" onClick={() => setShowMetaModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('元信息覆盖')}</h3>
              <button className="modal-close" onClick={() => setShowMetaModal(false)}>&times;</button>
            </div>
            <div className="modal-body">
              {metaError && <div className="error-message">{metaError}</div>}
              <div className="form-group">
                <label>{t('模型 ID')} *</label>
                <input
                  className="form-input"
                  value={metaModel}
                  onChange={(e) => setMetaModel(e.target.value)}
                  placeholder="gpt-4o"
                  disabled={metaSaving}
                />
              </div>
              <div className="form-group">
                <label>{t('归属方')} *</label>
                <input
                  className="form-input"
                  value={metaOwnedBy}
                  onChange={(e) => setMetaOwnedBy(e.target.value)}
                  placeholder="openai"
                  disabled={metaSaving}
                />
              </div>
              <div className="form-group">
                <label>{t('上下文长度（可选）')}</label>
                <input
                  className="form-input"
                  value={metaContext}
                  onChange={(e) => setMetaContext(e.target.value)}
                  placeholder="128000"
                  disabled={metaSaving}
                />
              </div>
              <div className="form-group">
                <label>{t('能力标签（逗号分隔，可选）')}</label>
                <input
                  className="form-input"
                  value={metaCaps}
                  onChange={(e) => setMetaCaps(e.target.value)}
                  placeholder="chat, vision"
                  disabled={metaSaving}
                />
              </div>
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setShowMetaModal(false)} disabled={metaSaving}>{t('取消')}</button>
              <button className="btn btn-primary" onClick={() => void handleMetaSave()} disabled={metaSaving}>
                {metaSaving ? t('保存中...') : t('保存')}
              </button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog state={metaConfirm} onClose={() => setMetaConfirm(null)} />
    </div>
  );
}