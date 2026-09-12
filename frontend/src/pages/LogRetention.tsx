import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

/**
 * 日志保留设置（系统设置 → 运维 tab 子标签）
 *
 * - 保留天数：设置后每日后台自动清理早于截止时间的请求/审计日志；
 *   不设置时长策略只受容量上限兜底。
 * - 容量上限：超过时自动清最旧一批（与后端 purge_overflow 对齐，此处可调）。
 * - 手动清理：立即按保留天数执行一次清理。
 */
export default function LogRetention(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [cleaning, setCleaning] = useState(false);
  /** 保留天数（空串 = 不限） */
  const [days, setDays] = useState('');
  const [capacity, setCapacity] = useState('');

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getLogRetention();
        const d = res?.data;
        setDays(d?.max_age_days != null ? String(d.max_age_days) : '');
        setCapacity(d?.max_capacity != null ? String(d.max_capacity) : '');
      } catch {
        // 读取失败保持默认空值
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const save = async (): Promise<void> => {
    setSaving(true);
    try {
      const daysVal = days.trim() === '' ? undefined : Math.max(1, parseInt(days) || 0);
      const capVal = capacity.trim() === '' ? undefined : Math.max(1000, parseInt(capacity) || 0);
      await api.updateLogRetention({ max_age_days: daysVal, max_capacity: capVal });
      addToast(t('日志保留配置已保存（每日自动清理）'));
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setSaving(false);
    }
  };

  const cleanupNow = async (): Promise<void> => {
    if (days.trim() === '') {
      addToast(t('请先设置保留天数'), 'error');
      return;
    }
    if (!window.confirm(t('将立即删除所有早于保留天数的请求与审计日志，该操作不可撤销。确定继续？'))) {
      return;
    }
    setCleaning(true);
    try {
      const res = await api.cleanupLogs();
      const removed = res?.data?.removed ?? 0;
      addToast(`${t('已清理')} ${removed} ${t('条日志')}`);
    } catch (err) {
      addToast(err instanceof Error ? err.message : String(err), 'error');
    } finally {
      setCleaning(false);
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('日志保留')}</h2>
      </div>
      <div className="card-body">
        <div className="settings-form">
          <p style={{ fontSize: 13, color: 'var(--text-muted)', marginBottom: 16 }}>
            {t('请求日志与审计日志的保留策略。设置保留天数后每日自动清理过期日志；容量上限超限时自动清最旧一批。')}
          </p>
          <div className="form-group">
            <label>{t('保留天数（天）')}</label>
            <input
              className="form-input"
              type="number"
              min={1}
              max={3650}
              value={days}
              onChange={(e) => setDays(e.target.value)}
              placeholder={t('留空 = 不按天数清理')}
            />
            <div className="form-hint">{t('例：90 = 自动删除 90 天前的日志')}</div>
          </div>
          <div className="form-group">
            <label>{t('容量上限（条）')}</label>
            <input
              className="form-input"
              type="number"
              min={1000}
              max={1000000}
              value={capacity}
              onChange={(e) => setCapacity(e.target.value)}
              placeholder={t('留空 = 默认 100000')}
            />
            <div className="form-hint">{t('请求日志超过上限时自动清理最旧的一批（默认 10 万条）')}</div>
          </div>
          <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
            <button type="button" className="btn btn-primary btn-sm" onClick={() => void save()} disabled={saving}>
              {saving ? t('保存中…') : t('保存配置')}
            </button>
            <button type="button" className="btn btn-outline btn-sm" onClick={() => void cleanupNow()} disabled={cleaning}>
              {cleaning ? t('清理中…') : t('立即清理')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
