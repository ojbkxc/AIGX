import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

/**
 * 客户端兼容（系统设置 → 运维 → 网关）
 *
 * 两项都与「客户端是什么」有关，但判定在服务端：
 * - 缺省模型回落：请求体完全没带 `model` 时用这里的值，避免客户端 400。
 *   显式传了 model 一律以请求为准，不做静默改写。
 * - 第三方模型回答完自动折叠过程（Codex 专用）：Codex 桌面端只在助手消息带
 *   `phase: "final_answer"` 时才折叠过程；开启后网关为非原生（第三方模型）
 *   答复补该标记。只对识别为 Codex 的客户端生效，保存即生效。
 */
export default function ClientCompatPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [defaultModel, setDefaultModel] = useState('');
  const [collapse, setCollapse] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getLimits();
        const d = res?.data as Record<string, unknown> | undefined;
        setDefaultModel(String(d?.defaultModel ?? ''));
        setCollapse(Boolean(d?.thirdPartyReasoningCollapse));
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const saveDefaultModel = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      await api.updateLimits({ default_model: defaultModel.trim() });
      addToast(t('已保存：缺省模型回落设置立即生效'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const saveCollapse = async (next: boolean): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      await api.updateLimits({ third_party_reasoning_collapse: next });
      setCollapse(next);
      addToast(next ? t('已开启：Codex 端将折叠第三方模型的过程') : t('已关闭：不再补阶段标记'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('客户端兼容')}</h2>
        <p className="card-subtitle">
          {t('缺省模型回落与 Codex 端过程折叠；保存即生效，均不改变显式指定的请求参数。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label>{t('缺省模型（请求未带 model 时回落）')}</label>
            <div style={{ display: 'flex', gap: 8 }}>
              <input
                className="form-input"
                placeholder={t('留空 = 不回落（维持 400）')}
                value={defaultModel}
                onChange={(e) => setDefaultModel(e.target.value)}
                disabled={saving}
              />
              <button className="btn btn-primary" onClick={() => void saveDefaultModel()} disabled={saving}>
                {t('保存')}
              </button>
            </div>
            <div className="form-hint">
              {t('仅在请求体完全没带 model 时生效；该模型必须在定价表/模型映射里能解析，否则回落本身会报「模型未定价」。')}
            </div>
          </div>
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={collapse}
                disabled={saving}
                onChange={(e) => void saveCollapse(e.target.checked)}
              />
              <span>{t('第三方模型回答完自动折叠过程（Codex）')}</span>
            </label>
            <div className="form-hint">
              {t('Codex 桌面端只在助手消息带「最终答案」阶段标记时折叠过程；开启后网关给第三方模型的答复补该标记。错误、截断与缺结束证据时不补。')}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}