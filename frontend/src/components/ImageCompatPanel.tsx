import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from './Toast';
import ModelPicker from './ModelPicker';
import type { ImageCompatSettingsItem } from '@/types/image-compat';

/**
 * 会话图片压缩与图片兼容（系统设置 → 聊天 → 图片压缩与兼容）：#7 后端契约的前端面。
 *
 * - 压缩（默认开）：内联 data:image 大图缩边重编码，按原格式；保存即热生效。
 * - 图片兼容（默认关）：请求含图而目标模型无 vision 位时
 *   describe = 辅助识图替换为文本描述 / switch = 改路由到视觉模型。
 * - 备选链：视觉模型失败后按顺序回退。
 *
 * 契约：GET 响应 camelCase；PUT body camelCase（后端收别名），缺省字段保持
 * 原值；越界后端 400，边界值（min/max）随 GET 响应下发，前端直接用。
 */
export default function ImageCompatPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  // 表单态（字符串归字符串、数字归数字；数字输入用 string 承载避免半输入态被改写）
  const [compressEnabled, setCompressEnabled] = useState(true);
  const [maxEdge, setMaxEdge] = useState('1568');
  const [quality, setQuality] = useState('85');
  const [thresholdKb, setThresholdKb] = useState('256');
  const [visionEnabled, setVisionEnabled] = useState(false);
  const [mode, setMode] = useState<'describe' | 'switch'>('describe');
  const [visionModel, setVisionModel] = useState('');
  const [fallbackText, setFallbackText] = useState('');
  const [describePrompt, setDescribePrompt] = useState('');
  // 边界值（GET 响应下发；接口没给时兜底与后端常量一致的默认）
  const [bounds, setBounds] = useState({
    maxEdgeMin: 256,
    maxEdgeMax: 8192,
    qualityMin: 1,
    qualityMax: 100,
    thresholdKbMin: 0,
    thresholdKbMax: 10240,
  });
  const [defaultPrompt, setDefaultPrompt] = useState('');

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getImageCompatSettings();
        const d = res?.data as ImageCompatSettingsItem | undefined;
        if (!d || Array.isArray(d)) return;
        setCompressEnabled(Boolean(d.enabled));
        setMaxEdge(String(d.maxEdge ?? 1568));
        setQuality(String(d.quality ?? 85));
        setThresholdKb(String(d.thresholdKb ?? 256));
        setVisionEnabled(Boolean(d.visionEnabled));
        setMode(d.mode === 'switch' ? 'switch' : 'describe');
        setVisionModel(d.visionModel || '');
        setFallbackText((d.fallbackModels || []).join(', '));
        setDescribePrompt(d.describePrompt || '');
        setBounds({
          maxEdgeMin: d.maxEdgeMin ?? 256,
          maxEdgeMax: d.maxEdgeMax ?? 8192,
          qualityMin: d.qualityMin ?? 1,
          qualityMax: d.qualityMax ?? 100,
          thresholdKbMin: d.thresholdKbMin ?? 0,
          thresholdKbMax: d.thresholdKbMax ?? 10240,
        });
        setDefaultPrompt(d.defaultDescribePrompt || '');
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
      // 客户端先夹一次（后端仍会校验，越界 400）；非数字回落当前生效值
      const edge = Math.min(bounds.maxEdgeMax, Math.max(bounds.maxEdgeMin, Number(maxEdge) || 1568));
      const q = Math.min(bounds.qualityMax, Math.max(bounds.qualityMin, Number(quality) || 85));
      const th = Math.min(bounds.thresholdKbMax, Math.max(bounds.thresholdKbMin, Number(thresholdKb) || 0));
      const res = await api.saveImageCompatSettings({
        enabled: compressEnabled,
        maxEdge: edge,
        quality: q,
        thresholdKb: th,
        visionEnabled,
        mode,
        visionModel: visionModel.trim(),
        fallbackModels: fallbackText
          .split(/[,，]/)
          .map((s) => s.trim())
          .filter(Boolean),
        describePrompt,
      });
      // 保存成功后用后端归一化值回填（trim/去重/夹取后）
      const d = res?.data as ImageCompatSettingsItem | undefined;
      if (d && !Array.isArray(d)) {
        setMaxEdge(String(d.maxEdge));
        setQuality(String(d.quality));
        setThresholdKb(String(d.thresholdKb));
        setFallbackText((d.fallbackModels || []).join(', '));
        setVisionModel(d.visionModel || '');
      }
      addToast(t('图片压缩与兼容设置已保存'));
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
        <h2>{t('图片压缩与兼容')}</h2>
        <p className="card-subtitle">
          {t('会话内联图片压缩与视觉模型兼容（对照 Agent2API 图片压缩/图片兼容）；保存即热生效。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          {/* ── 压缩 ── */}
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={compressEnabled}
                onChange={(e) => setCompressEnabled(e.target.checked)}
              />
              <span>{t('启用会话图片压缩')}</span>
            </label>
            <div className="form-hint">
              {t('转发前把超过阈值的内联图片（data:image/jpeg/png/webp）缩到长边上限并按原格式重编码；失败/压完更大时保留原图，绝不阻断请求。http(s) 外链与动图不处理。')}
            </div>
          </div>

          <div className="form-group">
            <label>{t('触发阈值 (KB)')}</label>
            <input
              className="form-input"
              type="number"
              min={bounds.thresholdKbMin}
              max={bounds.thresholdKbMax}
              value={thresholdKb}
              onChange={(e) => setThresholdKb(e.target.value)}
            />
            <div className="form-hint">
              {t('原图超过该体积才尝试压缩（默认 256 KB；0 = 每张都尝试）。')}
            </div>
          </div>

          <div className="form-group">
            <label>{t('长边上限 (像素)')}</label>
            <input
              className="form-input"
              type="number"
              min={bounds.maxEdgeMin}
              max={bounds.maxEdgeMax}
              value={maxEdge}
              onChange={(e) => setMaxEdge(e.target.value)}
            />
            <div className="form-hint">
              {t('超过该长边等比缩小（默认 1568，多数视觉模型按此折算 token 最优；不放大）。')}
            </div>
          </div>

          <div className="form-group">
            <label>{t('压缩质量')}</label>
            <input
              className="form-input"
              type="number"
              min={bounds.qualityMin}
              max={bounds.qualityMax}
              value={quality}
              onChange={(e) => setQuality(e.target.value)}
            />
            <div className="form-hint">
              {t('JPEG 重编码质量（默认 85）；PNG/WebP 走各自重压缩，体积不降时保留原图。')}
            </div>
          </div>

          {/* ── 图片兼容 ── */}
          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={visionEnabled}
                onChange={(e) => setVisionEnabled(e.target.checked)}
              />
              <span>{t('启用图片兼容（视觉备选模型）')}</span>
            </label>
            <div className="form-hint">
              {t('请求含图片而目标模型不支持视觉时自动处理；默认关闭，关闭时请求逐字节不变。开启后必须选择视觉模型。')}
            </div>
          </div>

          {visionEnabled && (
            <>
              <div className="form-group">
                <label>{t('处理模式')}</label>
                <select
                  className="form-input"
                  value={mode}
                  onChange={(e) => setMode(e.target.value === 'switch' ? 'switch' : 'describe')}
                >
                  <option value="describe">{t('辅助识图（替换为文字描述）')}</option>
                  <option value="switch">{t('切换视觉模型（改路由到视觉模型）')}</option>
                </select>
                <div className="form-hint">
                  {mode === 'describe'
                    ? t('用辅助视觉模型生成图片描述并替换图片块（同一张图一次请求只识图一次）。')
                    : t('把本次请求改路由到下方视觉模型（仍按视觉模型计价；未定价时自动保持原路由）。')}
                </div>
              </div>

              <div className="form-group">
                <label>{t('视觉模型')}</label>
                <ModelPicker value={visionModel} onChange={setVisionModel} />
                <div className="form-hint">
                  {t('辅助识图用的模型（switch 模式下即目标视觉模型）；必须已定价或有渠道声明。')}
                </div>
              </div>

              <div className="form-group">
                <label>{t('视觉模型备选链')}</label>
                <input
                  className="form-input"
                  type="text"
                  value={fallbackText}
                  placeholder={t('逗号分隔，按顺序回退')}
                  onChange={(e) => setFallbackText(e.target.value)}
                />
                <div className="form-hint">
                  {t('视觉模型调用失败（可重试类错误）时按此顺序逐个回退；与主模型重名的项忽略。')}
                </div>
              </div>

              <div className="form-group">
                <label>{t('辅助识图提示词')}</label>
                <textarea
                  className="form-input"
                  rows={3}
                  value={describePrompt}
                  placeholder={defaultPrompt || t('留空使用内置默认提示词')}
                  onChange={(e) => setDescribePrompt(e.target.value)}
                />
                <div className="form-hint">
                  {t('留空使用内置默认提示词；仅在辅助识图模式下生效。')}
                </div>
              </div>
            </>
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
