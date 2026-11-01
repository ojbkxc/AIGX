import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';
import type { PromptMode } from '../types';

/**
 * 系统提示词注入（系统设置 → 运维 → 网关，GatewayPanel 下方）
 *
 * 对照 Agent2API「设置 → 网关 → 系统提示词」：网关在转发前对 messages 的
 * system 消息做统一处置——
 *
 * - 透传：客户端发什么转发什么（默认，行为与未开启时逐字相同）
 * - 替换：删掉客户端全部 system 消息，头部插入网关自有提示词
 * - 追加：保留客户端 system，在其后追加一条网关提示词
 *
 * 保存即热生效：下一请求即按新策略注入，无需重启。
 */

const MODES: Array<{ value: PromptMode; label: string; tip: string }> = [
  {
    value: 'passthrough',
    label: '透传',
    tip: '客户端发什么转发什么，网关不干预（默认）。',
  },
  {
    value: 'custom',
    label: '替换',
    tip: '删除客户端所有 system 消息，头部插入下方网关提示词。想让所有请求共享统一人设/规则时选它。',
  },
  {
    value: 'append',
    label: '追加',
    tip: '保留客户端 system 消息，在开头连续 system 块之后追加一条网关提示词。想叠加安全规则又不破坏用户人设时选它。',
  },
];

export default function PromptPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [mode, setMode] = useState<PromptMode>('passthrough');
  const [text, setText] = useState('');

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getGatewayPrompt();
        const d = res?.data;
        if (d) {
          setMode(d.mode ?? 'passthrough');
          setText(d.text ?? '');
        }
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
      const res = await api.saveGatewayPrompt({ mode, text });
      const d = res?.data;
      if (d) {
        setMode(d.mode ?? mode);
        setText(d.text ?? text);
      }
      addToast(t('系统提示词已保存（下一请求即生效）'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  const modeTip = MODES.find((m) => m.value === mode)?.tip ?? '';

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('系统提示词')}</h2>
        <p className="card-subtitle">
          {t('网关统一处置转发请求的 system 消息；保存即热生效。透传模式行为与未开启时完全相同。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label>{t('注入模式')}</label>
            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
              {MODES.map((m) => (
                <button
                  key={m.value}
                  type="button"
                  className={`btn ${mode === m.value ? 'btn-primary' : 'btn-secondary'}`}
                  onClick={() => setMode(m.value)}
                >
                  {t(m.label)}
                </button>
              ))}
            </div>
            <div className="form-hint">{t(modeTip)}</div>
          </div>

          {mode !== 'passthrough' && (
            <div className="form-group">
              <label>{t('网关提示词')}</label>
              <textarea
                className="form-input"
                rows={8}
                value={text}
                onChange={(e) => setText(e.target.value)}
                placeholder={t('多行文本；空文本等价于透传')}
              />
              <div className="form-hint">
                {t('当前 {{count}} 字符（上限 64000）', { count: text.length })}
              </div>
            </div>
          )}

          <div className="settings-actions">
            <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
              {saving ? t('保存中...') : t('保存更改')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
