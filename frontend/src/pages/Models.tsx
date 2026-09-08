import { useState, useEffect, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Boxes, Search, Copy, Layers } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Card, Loading, EmptyState, Badge } from '../components/ui';
import type { ModelInfo } from '../types';
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

export default function Models(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [query, setQuery] = useState('');

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

  const handleCopy = async (id: string) => {
    try {
      await navigator.clipboard.writeText(id);
      addToast(t('已复制到剪贴板'));
    } catch {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('模型预设')}</h1>
          <p>{t('网关聚合的可用模型目录（来自启用渠道的 models 声明）')}</p>
        </div>
        <button className="btn btn-outline btn-sm" onClick={() => void load()} disabled={loading} style={{ gap: 6 }}>
          {loading ? t('加载中...') : t('刷新')}
        </button>
      </div>

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
                    onClick={() => void handleCopy(m.id)}
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
    </div>
  );
}