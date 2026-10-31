import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { SkeletonList } from '../components/ui';
import './Pricing.css';

/**
 * 定价倍率页 —— 只保留「倍率配置」。
 *
 * 模型价格目录已并入「模型管理」（`/model-manage`）：逐模型定价在那里与
 * 启停、别名放在一起，避免同一份数据在两页各改一遍。这里剩下的两项
 * （模型倍率 / 分组倍率）是**全局**参数，与具体模型无关，仍留在本页；
 * 汇率与价格同步在「系统设置 → 计费」。
 */

interface RatiosState {
  model_ratio: Record<string, number>;
  group_ratio: Record<string, number>;
}

export default function Pricing() {
  const { t } = useTranslation();
  const addToast = useToast();

  const [error, setError] = useState('');
  const [ratioLoading, setRatioLoading] = useState(true);
  const [savingRatios, setSavingRatios] = useState(false);
  const [ratioText, setRatioText] = useState('{}');
  const [groupRatioText, setGroupRatioText] = useState('{}');
  const [modelError, setModelError] = useState<string | null>(null);
  const [groupError, setGroupError] = useState<string | null>(null);

  useEffect(() => {
    void loadRatios();
    // 只在挂载时拉一次；保存后由 handleSaveRatios 自行回读
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadRatios = async () => {
    setRatioLoading(true);
    try {
      const res = await api.getRatios();
      const r = (res?.data ?? {}) as unknown as RatiosState;
      setRatioText(JSON.stringify(r.model_ratio || {}, null, 2));
      setGroupRatioText(JSON.stringify(r.group_ratio || {}, null, 2));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRatioLoading(false);
    }
  };

  /** 失焦时校验 JSON，避免保存时才发现格式错 */
  const handleRatioBlur = (field: 'model' | 'group', value: string) => {
    try {
      JSON.parse(value || '{}');
      if (field === 'model') setModelError(null);
      else setGroupError(null);
    } catch {
      if (field === 'model') setModelError(t('模型倍率 JSON 格式错误'));
      else setGroupError(t('分组倍率 JSON 格式错误'));
    }
  };

  const handleSaveRatios = async () => {
    setSavingRatios(true);
    setError('');
    let modelRatio: Record<string, number> = {};
    let groupRatio: Record<string, number> = {};
    try {
      modelRatio = JSON.parse(ratioText || '{}');
    } catch {
      setModelError(t('模型倍率 JSON 格式错误'));
      setSavingRatios(false);
      return;
    }
    try {
      groupRatio = JSON.parse(groupRatioText || '{}');
    } catch {
      setGroupError(t('分组倍率 JSON 格式错误'));
      setSavingRatios(false);
      return;
    }
    try {
      // payload 与后端 RatioConfig 对齐
      await api.updateRatios({ model_ratio: modelRatio, group_ratio: groupRatio });
      addToast(t('倍率配置已保存'));
      void loadRatios();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingRatios(false);
    }
  };

  return (
    <div className="pricing-shell">
      <div className="page-header">
        <div>
          <h1>{t('定价倍率')}</h1>
          <p>{t('全局倍率配置。逐模型的输入/输出价格请到「模型管理」里编辑。')}</p>
        </div>
      </div>

      <div className="notify-note" style={{ fontSize: 12.5, lineHeight: 1.7 }}>
        {t('模型价格目录已并入模型管理页。')}{' '}
        <Link to="/model-manage">{t('前往模型管理编辑价格')}</Link>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="pricing-content">
        <div className="card">
          <div className="card-header">
            <h2>{t('倍率配置')}</h2>
          </div>
          <div className="card-body">
            {ratioLoading ? (
              <SkeletonList rows={3} />
            ) : (
              <>
                <p className="ratio-hint">
                  {t('最终费用 = 基础费用 × 模型倍率 × 分组倍率。缺省倍率为 1.0。')}
                </p>
                <div className="ratio-grid">
                  <div className="form-group">
                    <label>{t('模型倍率 (JSON)')}</label>
                    <textarea
                      className="form-input ratio-textarea"
                      rows={10}
                      value={ratioText}
                      onChange={(e) => {
                        setRatioText(e.target.value);
                        setModelError(null);
                      }}
                      onBlur={() => handleRatioBlur('model', ratioText)}
                      style={modelError ? { borderColor: 'rgb(239,68,68)' } : undefined}
                      placeholder='{"glm-5.2": 1, "deepseek-v3": 0.5}'
                    />
                    <span className="form-hint">
                      {t('模型名 → 倍率。例如')} {'{"gpt-4": 2, "claude-3": 1.5}'}
                    </span>
                    {modelError && (
                      <span className="form-hint" style={{ color: 'rgb(239,68,68)' }}>
                        {modelError}
                      </span>
                    )}
                  </div>
                  <div className="form-group">
                    <label>{t('分组倍率 (JSON)')}</label>
                    <textarea
                      className="form-input ratio-textarea"
                      rows={10}
                      value={groupRatioText}
                      onChange={(e) => {
                        setGroupRatioText(e.target.value);
                        setGroupError(null);
                      }}
                      onBlur={() => handleRatioBlur('group', groupRatioText)}
                      style={groupError ? { borderColor: 'rgb(239,68,68)' } : undefined}
                      placeholder='{"default": 1, "vip": 0.8}'
                    />
                    <span className="form-hint">
                      {t('分组名 → 倍率。例如')} {'{"default": 1, "vip": 0.8}'}
                    </span>
                    {groupError && (
                      <span className="form-hint" style={{ color: 'rgb(239,68,68)' }}>
                        {groupError}
                      </span>
                    )}
                  </div>
                </div>

                <div style={{ marginTop: 16 }}>
                  <button
                    className="btn btn-primary"
                    onClick={handleSaveRatios}
                    disabled={savingRatios}
                  >
                    {savingRatios ? t('保存中...') : t('保存倍率配置')}
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
