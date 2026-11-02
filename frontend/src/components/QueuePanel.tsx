import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

/**
 * 排队等待（系统设置 → 运维 → 网关）
 *
 * 有些上游在模型繁忙时不报错、只排队：回一句「暂不可服务，建议 N 秒后
 * 再来」（Qoder 免费模型用的就是这套，业务码 10605）。开启后按上游建议
 * 时长等一会儿再发同一请求，等几次还排不上才把排队态报给客户端。
 *
 *   - 最多等待次数：0 = 不等，排队即返回 503；
 *   - 单次等待秒数：0 = 跟随上游建议（上游没给建议时退到 15 秒）。
 *
 * 排队等待发生在首字节之前，吃的是「等待响应超时」那份预算：两项设置
 * 一起决定「一次请求最多卡多久」，不会为了排队突破等待响应超时。
 */
export default function QueuePanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [maxWaits, setMaxWaits] = useState(2);
  const [waitSeconds, setWaitSeconds] = useState(0);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getLimits();
        const d = res?.data as Record<string, unknown> | undefined;
        setMaxWaits(Number(d?.queue_max_waits ?? 2));
        setWaitSeconds(Number(d?.queue_wait_seconds ?? 0));
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      await api.updateLimits({
        queue_max_waits: maxWaits,
        queue_wait_seconds: waitSeconds,
      });
      addToast(t('排队等待已保存，下一个请求立即生效'));
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
        <h2>{t('排队等待')}</h2>
        <p className="card-subtitle">
          {t('上游模型繁忙时按建议时长原地等待重发；等几次还排不上才把排队态报给客户端。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label>{t('最多等待次数')}</label>
            <input
              type="number"
              min={0}
              max={10}
              value={maxWaits}
              disabled={saving}
              onChange={(e) =>
                setMaxWaits(Math.min(10, Math.max(0, Math.trunc(Number(e.target.value) || 0))))
              }
            />
            <div className="form-hint">
              {t('0 = 不等：遇到排队立即返回 503（快速失败、客户端自己重试）。')}
            </div>
          </div>
          <div className="form-group">
            <label>{t('单次等待秒数')}</label>
            <input
              type="number"
              min={0}
              max={120}
              value={waitSeconds}
              disabled={saving}
              onChange={(e) =>
                setWaitSeconds(Math.min(120, Math.max(0, Math.trunc(Number(e.target.value) || 0))))
              }
            />
            <div className="form-hint">
              {t('0 = 跟随上游建议（建议值钳在 5~30 秒，上游没给时 15 秒）。')}
            </div>
          </div>
          <div className="form-group">
            <button className="btn btn-primary" disabled={saving} onClick={() => void save()}>
              {saving ? t('保存中') : t('保存')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
