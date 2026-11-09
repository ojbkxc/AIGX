import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

/**
 * 截断自动续写（系统设置 → 运维 → 网关）
 *
 * 上游一轮说到一半就停了（输出预算用尽、连接被掐、整轮烧在思考通道），
 * 客户端拿到的是半截答案。开启后网关判出截断，把已写出的半截当上下文
 * 再问一轮「请继续」，新内容接在同一条响应后面，直到自然停止、续满
 * 轮数、或复读检测拦下（两轮正文几乎一模一样即停）。
 *
 * 工具调用轮永不续写（工具执行权在客户端）。续写轮复用同一渠道，
 * 每轮都是真实上游往返，计费按各轮用量求和。默认关闭：关着时数据面
 * 连包装都不做，下发字节与未接入时逐字一致。
 */
export default function AutoContinuePanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [enabled, setEnabled] = useState(false);
  const [maxRounds, setMaxRounds] = useState(3);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getLimits();
        const d = res?.data as Record<string, unknown> | undefined;
        setEnabled(Boolean(d?.auto_continue));
        setMaxRounds(Number(d?.auto_continue_max_rounds ?? 3));
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
        auto_continue: enabled,
        auto_continue_max_rounds: maxRounds,
      });
      addToast(t('自动续写设置已保存，下一个请求立即生效'));
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
        <h2>{t('截断自动续写')}</h2>
        <p className="card-subtitle">
          {t('上游答复被截断时，网关自动追问「请继续」并把续写内容接在同一条响应后面。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={enabled}
                disabled={saving}
                onChange={(e) => setEnabled(e.target.checked)}
              />
              <span>{t('启用自动续写')}</span>
            </label>
            <div className="form-hint">
              {t(
                '关闭时下发字节与未接入时逐字一致。开启后每轮续写都是一次真实上游往返（按各轮用量计费），且答案内容会出现在所选供应商的上游上；工具调用轮永不续写。',
              )}
            </div>
          </div>
          <div className="form-group">
            <label>{t('最多续写轮数')}</label>
            <input
              type="number"
              min={1}
              max={8}
              value={maxRounds}
              disabled={saving}
              onChange={(e) =>
                setMaxRounds(Math.min(8, Math.max(1, Math.trunc(Number(e.target.value) || 1))))
              }
            />
            <div className="form-hint">
              {t('不含首轮，即一条回答最多被续写几次；复读检测会提前收手。')}
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
