import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

/**
 * 流式输出兼容（系统设置 → 运维 → 网关）
 *
 * 丢弃上游「换行保活帧」：部分上游会在正文分片之间插 content 只含换行的帧
 * 给自己保活，客户端按 markdown 渲染就成了一行一个词。开关保存即生效
 * （进程级快照，桥接层每条 SSE 帧读取），只改下发给客户端的字节。
 */
export default function StreamCompatPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [strip, setStrip] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getLimits();
        const d = res?.data as Record<string, unknown> | undefined;
        setStrip(Boolean(d?.strip_newline_keepalive));
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const save = async (next: boolean): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      await api.updateLimits({ strip_newline_keepalive: next });
      setStrip(next);
      addToast(next ? t('已开启：换行保活帧将不再下发') : t('已关闭：换行保活帧按原样下发'));
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
        <h2>{t('流式输出兼容')}</h2>
        <p className="card-subtitle">
          {t('上游 SSE 保活帧的过滤；保存即生效，只改下发给客户端的字节。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={strip}
                disabled={saving}
                onChange={(e) => void save(e.target.checked)}
              />
              <span>{t('丢弃上游「换行保活帧」')}</span>
            </label>
            <div className="form-hint">
              {t('部分上游会在正文分片间插入只含换行的帧保活，客户端会渲染成一行一个词；开启后这类帧不再下发（收尾、用量、错误帧始终保留）。')}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}