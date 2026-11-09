import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from './Toast';
import ModelPicker from './ModelPicker';

/**
 * 聊天功能设置（系统设置 → 聊天）：#10 后端契约的前端面。
 *
 * - 自动标题：开启后新会话首条消息落库时异步生成标题（失败静默保持截断标题）
 * - 标题模型 / 渠道：与 AI 助手设置同款控件（ModelPicker + 启用渠道下拉）
 * - 提示词截断：首条用户消息发给模型的最大字符数（默认 500，超出截断）
 * - 保存即热生效（后端运行期重读配置，无需重启）
 */
export default function ChatSettingsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [autoTitle, setAutoTitle] = useState(false);
  const [titleModel, setTitleModel] = useState('');
  const [titleChannel, setTitleChannel] = useState('');
  const [titlePromptChars, setTitlePromptChars] = useState('500');
  const [channels, setChannels] = useState<Array<{ id: string; name: string }>>([]);

  useEffect(() => {
    void (async () => {
      try {
        const [res, chRes] = await Promise.all([api.getChatSettings(), api.listChannels()]);
        const list = (Array.isArray(chRes?.data) ? chRes.data : [])
          .filter((c) => (c.status ?? 'enabled') === 'enabled')
          .map((c) => ({ id: String(c.id), name: c.name }));
        setChannels(list);
        const d = res?.data;
        if (!d || Array.isArray(d)) return;
        setAutoTitle(Boolean(d.autoTitle));
        setTitleModel(d.titleModel || '');
        setTitleChannel(d.titleChannel || '');
        setTitlePromptChars(String(d.titlePromptChars ?? 500));
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
      // PUT body 走 snake_case（对齐后端 ChatConfig serde；缺省字段保持原值语义由后端处理）
      const res = await api.saveChatSettings({
        auto_title: autoTitle,
        title_model: titleModel.trim(),
        title_channel: titleChannel.trim(),
        // 后端不 clamp；前端兜底 ≥1（auto_title=true 且 0 会被后端 400）
        title_prompt_chars: Math.max(1, Number(titlePromptChars) || 500),
      });
      const d = res?.data;
      if (d) {
        setAutoTitle(Boolean(d.autoTitle));
        setTitleModel(d.titleModel || '');
        setTitleChannel(d.titleChannel || '');
        setTitlePromptChars(String(d.titlePromptChars ?? 500));
      }
      addToast(t('聊天设置已保存'));
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
        <h2>{t('聊天')}</h2>
        <p className="card-subtitle">
          {t('对话增值能力：LLM 自动为会话生成标题；保存后立即生效，无需重启。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={autoTitle}
                onChange={(e) => setAutoTitle(e.target.checked)}
              />
              <span>{t('启用会话自动标题')}</span>
            </label>
            <div className="form-hint">
              {t('开启后新会话首条消息落库时异步生成标题；生成失败保持截断标题，静默不弹错。')}
            </div>
          </div>

          <div className="form-group">
            <label>{t('标题模型')}</label>
            <ModelPicker value={titleModel} onChange={setTitleModel} />
            <div className="form-hint">{t('留空自动发现启用且已定价的模型（与 AI 助手同口径）。')}</div>
          </div>

          <div className="form-group">
            <label>{t('供应商锁定')}</label>
            <select
              className="form-input"
              value={titleChannel}
              onChange={(e) => setTitleChannel(e.target.value)}
            >
              <option value="">{t('全局调度（默认）')}</option>
              {channels.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
            <div className="form-hint">{t('指定后标题生成只走该供应商；留空按全局优先级自动选路。')}</div>
          </div>

          <div className="form-group">
            <label>{t('提示词截断长度（字符）')}</label>
            <input
              className="form-input"
              type="number"
              min={1}
              max={5000}
              value={titlePromptChars}
              onChange={(e) => setTitlePromptChars(e.target.value)}
            />
            <div className="form-hint">
              {t('发给模型的首条用户消息最大字符数（默认 500，超出截断）。')}
            </div>
          </div>

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
