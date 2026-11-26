// 转发前压缩对话内嵌大图（系统设置 → 网关 → 图片压缩）。
//
// 对照 Agent2API 的 `/api/image-compress`：把对话请求体里内嵌的 base64 大图
// 等比缩到长边上限、重编码成 JPEG，既缩小请求体也降低按像素折算的上游计费。
// 只改发给上游的副本；请求日志里保存的仍是客户端发来的原图。只作用在对话链路
// （生图 / 图片编辑在转发层另有分支，不经过这里）。
//
// 真正的压缩在后端；前端只负责：能力总开关 + 三项参数 + 把它们落库。
import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from './Toast';
import { Switch } from './ui';
import type { ImageCompressPayload, ImageCompressSettings } from '../api/endpoints/imageCompress';

interface FieldSpec {
  key: keyof Pick<ImageCompressSettings, 'imageCompressMaxEdge' | 'imageCompressQuality' | 'imageCompressThresholdKb'>;
  labelKey: string;
  hintKey: string;
  unitKey: string;
  min: number;
  max: number;
  fallback: number;
}

const FIELDS: FieldSpec[] = [
  {
    key: 'imageCompressMaxEdge',
    labelKey: '长边像素上限',
    hintKey: '图片最长边超过这个值就等比缩小（只缩不放）。多数视觉模型在 2048 量级已够用，调小更省流量但细节会丢。',
    unitKey: '像素',
    min: 256,
    max: 8192,
    fallback: 2048,
  },
  {
    key: 'imageCompressQuality',
    labelKey: '重编码质量',
    hintKey: '缩放后重新存成 JPEG 时用的质量；数值越小体积越小、画质越差。',
    unitKey: '质量',
    min: 40,
    max: 95,
    fallback: 80,
  },
  {
    key: 'imageCompressThresholdKb',
    labelKey: '触发阈值',
    hintKey: '解码后小于这个体积的图片保持原样；填 0 表示所有内嵌图片都过一遍。',
    unitKey: 'KB',
    min: 0,
    max: 10240,
    fallback: 512,
  },
];

/** 响应兼容裸对象与 {data} 信封。 */
function unwrap(payload: ImageCompressPayload | null | undefined): Partial<ImageCompressSettings> {
  if (!payload) return {};
  if (payload.data && typeof payload.data === 'object') return payload.data;
  return payload;
}

export default function ImageCompressPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const [error, setError] = useState('');
  const [enabled, setEnabled] = useState(true);
  const [values, setValues] = useState<Record<string, string>>({
    imageCompressMaxEdge: '',
    imageCompressQuality: '',
    imageCompressThresholdKb: '',
  });
  // 服务端实际生效值（用于展示默认/回填占位）
  const [effective, setEffective] = useState<Partial<ImageCompressSettings>>({});

  const applyPayload = useCallback((payload: ImageCompressPayload): void => {
    const data = unwrap(payload);
    setEffective(data);
    if (typeof data.imageCompressEnabled === 'boolean') setEnabled(data.imageCompressEnabled);
    setValues({
      imageCompressMaxEdge: typeof data.imageCompressMaxEdge === 'number' ? String(data.imageCompressMaxEdge) : '',
      imageCompressQuality: typeof data.imageCompressQuality === 'number' ? String(data.imageCompressQuality) : '',
      imageCompressThresholdKb: typeof data.imageCompressThresholdKb === 'number' ? String(data.imageCompressThresholdKb) : '',
    });
  }, []);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      applyPayload(await api.getImageCompress());
      setUnavailable(false);
    } catch (err) {
      const status = (err as { status?: number }).status;
      if (status === 404) {
        setUnavailable(true);
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setLoading(false);
    }
  }, [applyPayload]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async (): Promise<void> => {
    setError('');
    const patch: Partial<ImageCompressSettings> = { imageCompressEnabled: enabled };
    for (const field of FIELDS) {
      const raw = values[field.key].trim();
      if (raw === '') continue;
      const num = Number(raw);
      if (!Number.isInteger(num) || num < field.min || num > field.max) {
        setError(t('{{label}}需在 {{min}}~{{max}} 之间', { label: t(field.labelKey), min: field.min, max: field.max }));
        return;
      }
      patch[field.key] = num;
    }
    setSaving(true);
    try {
      applyPayload(await api.saveImageCompress(patch));
      addToast(t('图片压缩设置已保存，下一个请求立即生效'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const toggleEnabled = (next: boolean): void => {
    // 开关单独落库：翻一下就是一次明确的能力切换，不必等「保存」
    setEnabled(next);
    setSaving(true);
    setError('');
    void api
      .saveImageCompress({ imageCompressEnabled: next })
      .then((payload) => {
        applyPayload(payload);
        addToast(next ? t('已开启会话图片压缩') : t('已关闭会话图片压缩'));
      })
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => setSaving(false));
  };

  return (
    <div className="card" style={{ marginTop: 16 }}>
      <div className="card-header">
        <h2>{t('图片压缩')}</h2>
        <p className="card-subtitle">
          {t('转发前把对话里内嵌的 base64 大图等比缩到长边上限并重编码为 JPEG，既缩小请求体也降低按像素折算的上游计费。只改发给上游的副本，请求日志保存的仍是原图；生图与图片编辑不受影响。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        {unavailable ? (
          <div className="settings-state">{t('后端未就绪：/api/image-compress 端点尚未接线，此项暂不可用。')}</div>
        ) : loading ? (
          <div className="empty-state"><p>{t('加载中...')}</p></div>
        ) : (
          <div className="settings-form">
            <div className="form-group">
              <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <Switch checked={enabled} onChange={toggleEnabled} disabled={saving} />
                {t('启用转发前压缩')}
              </label>
              <span className="form-hint">
                {t('默认开启。关闭后内嵌图片原样转发（与接入前逐字节一致）。')}
              </span>
            </div>

            {FIELDS.map((field) => (
              <div key={field.key} className="form-group" style={{ maxWidth: 320 }}>
                <label htmlFor={`img-compress-${field.key}`}>
                  {t(field.labelKey)}（{t(field.unitKey)}）
                </label>
                <input
                  id={`img-compress-${field.key}`}
                  className="form-input"
                  type="number"
                  min={field.min}
                  max={field.max}
                  placeholder={String(typeof effective[field.key] === 'number' ? effective[field.key] : field.fallback)}
                  value={values[field.key]}
                  disabled={saving}
                  onChange={(e) => setValues((prev) => ({ ...prev, [field.key]: e.target.value }))}
                />
                <span className="form-hint">{t(field.hintKey)}</span>
              </div>
            ))}

            <div className="settings-actions">
              <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
                {saving ? t('保存中...') : t('保存更改')}
              </button>
              <button className="btn btn-secondary" style={{ marginLeft: 8 }} onClick={() => void load()} disabled={saving}>
                {t('刷新')}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
