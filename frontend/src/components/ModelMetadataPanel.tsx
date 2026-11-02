import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import type { ModelMetadataStatus } from '../types';

/**
 * models.dev 模型元数据（系统设置 → 运维 → 网关）
 *
 * 对照 Agent2API「设置 → 网关 → models.dev 元数据」：内置推断没声明的
 * 上下文长度 / 能力标签用社区目录只填空位（管理员覆盖永远优先）。
 * 开关默认开；关闭 = 不再联网刷新，已装载目录也不再参与展示。
 * 每 24 小时自动刷新一次，这里可手动触发（5 秒内连点会被合并）。
 */
export default function ModelMetadataPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState('');
  const [status, setStatus] = useState<ModelMetadataStatus | null>(null);

  const load = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getModelMetadata();
      setStatus(res?.data ?? null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = async (next: boolean): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      const res = await api.saveModelMetadata(next);
      const d = res?.data;
      setStatus(d ?? ((prev) => (prev ? { ...prev, enabled: next } : prev)));
      addToast(next ? t('已开启 models.dev 元数据补齐') : t('已关闭 models.dev 元数据补齐'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const refresh = async (): Promise<void> => {
    setRefreshing(true);
    setError('');
    try {
      const res = await api.refreshModelMetadata();
      const d = res?.data;
      if (d?.metadata) setStatus(d.metadata);
      if (d?.status === 'refreshed') {
        addToast(t('目录已刷新：共 {{n}} 条', { n: d.count }));
      } else if (d?.status === 'skipped') {
        addToast(t('刷新过于频繁，已合并'));
      } else if (d?.status === 'disabled') {
        addToast(t('models.dev 元数据当前是关闭状态'));
      } else {
        setError(d?.message || t('刷新失败'));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRefreshing(false);
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  const updatedAtText = status?.updatedAt
    ? new Date(status.updatedAt).toLocaleString()
    : t('尚未联网刷新（内嵌快照）');

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('models.dev 模型元数据')}</h2>
        <p className="card-subtitle">
          {t('社区模型能力目录补齐：只填内置推断未声明的上下文长度 / 能力标签，管理员覆盖永远优先。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={Boolean(status?.enabled)}
                disabled={saving}
                onChange={(e) => void toggle(e.target.checked)}
              />
              <span>{t('启用 models.dev 补齐与定时刷新（默认开）')}</span>
            </label>
            <div className="form-hint">
              {t('每 24 小时自动刷新一次；失败保留上一版目录。关闭后不再联网，已装载目录也不再参与展示。')}
            </div>
          </div>

          <div className="form-group">
            <div className="form-hint">
              {t('当前目录')}: {status?.count ?? 0} {t('条')} · {t('来源')}{': '}
              {status?.source || '-'} · {updatedAtText}
            </div>
            <button
              className="btn btn-outline"
              onClick={() => void refresh()}
              disabled={refreshing || !status?.enabled}
              style={{ marginTop: 8 }}
            >
              {refreshing ? t('刷新中...') : t('立即刷新')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}