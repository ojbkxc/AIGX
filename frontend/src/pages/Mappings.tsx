import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import './Mappings.css';

interface ModelMapping {
  /** 前端行 ID（crypto.randomUUID），编辑列表用稳定 key 避免重排串行 */
  id: string;
  key: string;
  value: string;
}

type MappingMap = Record<string, unknown>;

interface SettingsResponse {
  mappings?: MappingMap;
  data?: { mappings?: MappingMap };
}

const DEFAULT_MODELS: ModelMapping[] = [
  { id: 'default-glm-5.2', key: 'glm-5.2', value: '@cf/zai-org/glm-5.2' },
  { id: 'default-glm-4.7-flash', key: 'glm-4.7-flash', value: '@cf/zai-org/glm-4.7-flash' },
  { id: 'default-kimi-k2.7-code', key: 'kimi-k2.7-code', value: '@cf/moonshotai/kimi-k2.7-code' },
  { id: 'default-kimi-k2.6', key: 'kimi-k2.6', value: '@cf/moonshotai/kimi-k2.6' },
  { id: 'default-deepseek-v3', key: 'deepseek-v3', value: '@cf/deepseek-ai/deepseek-v3-0324' },
  { id: 'default-deepseek-r1-distill', key: 'deepseek-r1-distill', value: '@cf/deepseek-ai/deepseek-r1-distill-qwen-32b' },
  { id: 'default-qwen-2.5-72b', key: 'qwen-2.5-72b', value: '@cf/qwen/qwen2.5-72b-instruct' },
  { id: 'default-qwen-2.5-coder-32b', key: 'qwen-2.5-coder-32b', value: '@cf/qwen/qwen2.5-coder-32b-instruct' },
  { id: 'default-llama-4-scout', key: 'llama-4-scout', value: '@cf/meta/llama-4-scout-17b-16e-instruct' },
  { id: 'default-llama-4-maverick', key: 'llama-4-maverick', value: '@cf/meta/llama-4-maverick-17b-128e-instruct' },
  { id: 'default-llama-3.3-70b', key: 'llama-3.3-70b', value: '@cf/meta/llama-3.3-70b-instruct-fp8-fast' },
  { id: 'default-llama-3.1-8b', key: 'llama-3.1-8b', value: '@cf/meta/llama-3.1-8b-instruct' },
  { id: 'default-gemma-4-27b-it', key: 'gemma-4-27b-it', value: '@cf/google/gemma-4-27b-it' },
  { id: 'default-gemma-4-9b-it', key: 'gemma-4-9b-it', value: '@cf/google/gemma-4-9b-it' },
  { id: 'default-mixtral-8x7b', key: 'mixtral-8x7b', value: '@cf/mistral/mixtral-8x7b-instruct' },
  { id: 'default-bge-m3', key: 'bge-m3', value: '@cf/baai/bge-m3' },
  { id: 'default-whisper-1', key: 'whisper-1', value: '@cf/openai/whisper' },
  { id: 'default-flux-1-schnell', key: 'flux-1-schnell', value: '@cf/black-forest-labs/flux-1-schnell' },
  { id: 'default-tts', key: 'tts', value: '@cf/myshell-ai/tts' },
];

/** 前端稳定行 ID（编辑列表 key），不可序列化进后端 */
function newEntryId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `row-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

export default function Mappings() {
  const [, setMappings] = useState<MappingMap>({});
  const [, setCustomMappings] = useState<MappingMap>({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [filter, setFilter] = useState('');
  const [showDefaults, setShowDefaults] = useState(true);
  const addToast = useToast();
  const [entries, setEntries] = useState<ModelMapping[]>([]);
  const { t } = useTranslation();

  useEffect(() => {
    loadMappings();
  }, []);

  const loadMappings = async () => {
    setLoading(true);
    setError('');
    try {
      const res = (await api.getSettings()) as SettingsResponse;
      const data = res.data || res || {};
      const allMappings: MappingMap = data.mappings || (data as MappingMap) || {};
      setMappings(allMappings);

      const customOnly: MappingMap = {};
      const customEntries: ModelMapping[] = [];
      for (const [key, value] of Object.entries(allMappings)) {
        const isDefault = DEFAULT_MODELS.some(d => d.key === key && d.value === value);
        if (!isDefault) {
          customOnly[key] = value;
          customEntries.push({ id: newEntryId(), key, value: String(value) });
        }
      }
      setCustomMappings(customOnly);
      setEntries(customEntries);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  const addEntry = () => {
    setEntries([...entries, { id: newEntryId(), key: '', value: '' }]);
  };

  const removeEntry = (id: string) => {
    setEntries(entries.filter((e) => e.id !== id));
  };

  const updateEntry = (id: string, field: 'key' | 'value', val: string) => {
    setEntries(entries.map((entry) =>
      entry.id === id ? { ...entry, [field]: val } : entry
    ));
  };

  const handleSave = async () => {
    const filtered = entries.filter((e) => e.key.trim() && e.value.trim());
    const newMappings: Record<string, string> = {};
    filtered.forEach((e) => {
      newMappings[e.key.trim()] = e.value.trim();
    });
    setSaving(true);
    setError('');
    try {
      await api.updateSettings(newMappings, true);
      addToast(t('模型映射更新成功'));
      setMappings({ ...DEFAULT_MODELS.reduce((acc, d) => ({ ...acc, [d.key]: d.value }), {}), ...newMappings });
      setCustomMappings(newMappings);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const filteredDefaults = DEFAULT_MODELS.filter(d =>
    !filter || d.key.toLowerCase().includes(filter.toLowerCase()) || d.value.toLowerCase().includes(filter.toLowerCase())
  );

  if (loading) return <div className="loading">{t('加载模型映射')}</div>;

  return (
    <div>
      <div className="page-header">
        <h1>{t('模型映射')}</h1>
        <p>{t('配置网关的模型名称映射，将客户端请求的模型名映射到 Cloudflare Workers AI 模型')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="card" style={{ marginBottom: 16 }}>
        <div className="card-header">
          <h2>{t('默认模型映射')}</h2>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <input className="form-input" style={{ width: 200, margin: 0 }} placeholder={t('搜索模型...')}
              value={filter} onChange={(e) => setFilter(e.target.value)} />
            <button className="btn btn-outline btn-sm" onClick={() => setShowDefaults(!showDefaults)}>
              {showDefaults ? t('收起') : t('展开')} ({filteredDefaults.length})
            </button>
          </div>
        </div>
        {showDefaults && (
          <div className="card-body">
            <div className="default-models-grid">
              {filteredDefaults.map((d) => (
                <div className="default-model-chip" key={d.key} title={d.value}>
                  <span className="model-key">{d.key}</span>
                  <span className="model-arrow">→</span>
                  <span className="model-value">{d.value}</span>
                </div>
              ))}
              {filteredDefaults.length === 0 && (
                <div className="empty-state"><p>{t('无匹配的默认模型')}</p></div>
              )}
            </div>
          </div>
        )}
      </div>

      <div className="card">
        <div className="card-header">
          <h2>{t('自定义映射')} ({entries.length})</h2>
          <div className="mappings-actions">
            <button className="btn btn-outline" onClick={addEntry}>{t('+ 添加映射')}</button>
            <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
              {saving ? t('保存中...') : t('保存全部')}
            </button>
          </div>
        </div>
        <div className="card-body">
          {entries.length === 0 ? (
            <div className="empty-state">
              <p>{t('暂无自定义映射，默认模型映射已覆盖常见模型')}</p>
              <button className="btn btn-primary" onClick={addEntry}>{t('添加自定义映射')}</button>
            </div>
          ) : (
            <div className="mappings-list">
              {entries.map((entry) => (
                <div className="mapping-row" key={entry.id}>
                  <div className="mapping-field">
                    <label className="mapping-label">{t('模型键')}</label>
                    <input className="form-input" placeholder={t('mappingsPlaceholderModelKey')} value={entry.key} onChange={(e) => updateEntry(entry.id, 'key', e.target.value)} />
                  </div>
                  <div className="mapping-arrow">→</div>
                  <div className="mapping-field">
                    <label className="mapping-label">{t('映射值')}</label>
                    <input className="form-input" placeholder={t('mappingsPlaceholderModelValue')} value={entry.value} onChange={(e) => updateEntry(entry.id, 'value', e.target.value)} />
                  </div>
                  <button className="btn btn-danger btn-sm mapping-remove" onClick={() => removeEntry(entry.id)} title={t('删除')}>
                    ✕
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
