/**
 * 面板品牌设置（系统设置 → 通用 → 品牌）
 *
 * 对照 Agent2API v3.5.0-beta.3：面板名称 + **Logo PNG 上传**（不再填写 URL）。
 * - 保存后立即生效（无需重启）：配置写入 config.toml，Logo 写入 FileStore。
 * - 上传 PNG / JPG / WebP（≤2 MiB、≤4096×4096），后端统一重编码为静态 PNG
 *   （≤512 长边），生效地址是公开端点 `GET /api/public/branding/logo?v=<hash>`。
 * - 未上传 Logo 时回落到配置的 logo_url（默认 /logo.png），兼容既有部署。
 * - URL 校验由后端完成（拒绝 javascript:/data: 等可执行协议）；前端只展示错误。
 */
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from './Toast';
import type { BrandingData } from '../types';

/** 与后端 `MAX_LOGO_BYTES` 同值 */
const MAX_LOGO_BYTES = 2 * 1024 * 1024;
const ACCEPTED_LOGO_TYPES = ['image/png', 'image/jpeg', 'image/webp'];

export default function BrandingSettingsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const fileInput = useRef<HTMLInputElement>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [panelName, setPanelName] = useState('AIGX');
  const [currentLogoUrl, setCurrentLogoUrl] = useState('/logo.png');
  const [logoUploaded, setLogoUploaded] = useState(false);
  const [faviconUrl, setFaviconUrl] = useState('');
  const [defaults, setDefaults] = useState<BrandingData | null>(null);
  /** undefined = 不改；null = 移除已上传 Logo；string = 待上传的 data URL */
  const [pendingLogo, setPendingLogo] = useState<string | null | undefined>(undefined);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getBranding();
        const d = res?.data;
        if (!d) return;
        setPanelName(d.current.panel_name || 'AIGX');
        setCurrentLogoUrl(d.current.logo_url || '/logo.png');
        setLogoUploaded(Boolean(d.current.logo_uploaded));
        // favicon 为空表示沿用 logo（后端 GET 已回落，但这里保留“未单独设置”语义）
        setFaviconUrl(d.current.favicon_url === d.current.logo_url ? '' : d.current.favicon_url);
        setDefaults({
          panel_name: d.defaults.panel_name,
          logo_url: d.defaults.logo_url,
          favicon_url: d.defaults.favicon_url,
          title: `${d.defaults.panel_name} — AI 中转网关`,
        });
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const pickLogo = (selected?: File): void => {
    if (!selected) return;
    setError('');
    if (!ACCEPTED_LOGO_TYPES.includes(selected.type)) {
      setError(t('Logo 仅支持 PNG、JPG、WebP 图片'));
      return;
    }
    if (!selected.size || selected.size > MAX_LOGO_BYTES) {
      setError(t('Logo 不能超过 2 MiB'));
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === 'string') {
        setPendingLogo(reader.result);
      } else {
        setError(t('无法读取图片，请重新选择'));
      }
    };
    reader.onerror = () => setError(t('无法读取图片，请重新选择'));
    reader.readAsDataURL(selected);
  };

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      const res = await api.updateBranding({
        // 留空 = 恢复默认名称（与输入框提示一致）
        panel_name: panelName.trim() || (defaults?.panel_name ?? 'AIGX'),
        favicon_url: faviconUrl.trim(),
        ...(pendingLogo !== undefined ? { logo: pendingLogo } : {}),
      });
      if (res.success && res.data) {
        addToast(t('品牌设置已保存'), 'success');
        setPanelName(res.data.panel_name);
        setCurrentLogoUrl(res.data.logo_url);
        setLogoUploaded(Boolean(res.data.logo_uploaded));
        setFaviconUrl(res.data.favicon_url === res.data.logo_url ? '' : res.data.favicon_url);
        setPendingLogo(undefined);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const resetToDefaults = (): void => {
    if (!defaults) return;
    setPanelName(defaults.panel_name);
    setFaviconUrl('');
    // 恢复默认品牌 = 同时移除已上传 Logo（保存后生效）
    setPendingLogo(null);
  };

  if (loading) {
    return (
      <div className="card" style={{ marginTop: 16 }}>
        <div className="card-header">
          <h2>{t('品牌')}</h2>
        </div>
        <div className="card-body">
          <p>{t('加载中...')}</p>
        </div>
      </div>
    );
  }

  const previewUrl =
    pendingLogo === undefined ? currentLogoUrl : pendingLogo ?? defaults?.logo_url ?? '/logo.png';

  return (
    <div className="card" style={{ marginTop: 16 }}>
      <div className="card-header">
        <h2>{t('品牌')}</h2>
        <p className="card-subtitle">
          {t('自定义面板名称与 Logo；保存后刷新页面生效。')}
        </p>
      </div>
      <div className="card-body">
        <div className="settings-form">
          <div className="form-group">
            <label>{t('面板名称')}</label>
            <input
              className="form-input"
              value={panelName}
              maxLength={64}
              placeholder="AIGX"
              onChange={(e) => setPanelName(e.target.value)}
            />
            <span className="form-hint">{t('显示在登录页标题与侧栏；留空则回退默认值。')}</span>
          </div>

          <div className="form-group">
            <label>{t('品牌 Logo')}</label>
            <div style={{ display: 'flex', alignItems: 'center', gap: 12, flexWrap: 'wrap' }}>
              <img
                src={previewUrl}
                alt={t('Logo 预览')}
                width={44}
                height={44}
                style={{ borderRadius: 4, objectFit: 'contain' }}
                onError={(e) => {
                  (e.currentTarget as HTMLImageElement).style.visibility = 'hidden';
                }}
              />
              <input
                ref={fileInput}
                type="file"
                accept="image/png,image/jpeg,image/webp"
                style={{ display: 'none' }}
                onChange={(e) => {
                  const selected = e.target.files?.[0];
                  e.target.value = '';
                  pickLogo(selected);
                }}
              />
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => fileInput.current?.click()}
                disabled={saving}
              >
                {t('上传 Logo')}
              </button>
              {(logoUploaded || typeof pendingLogo === 'string') && (
                <button
                  type="button"
                  className="btn"
                  onClick={() => setPendingLogo(null)}
                  disabled={saving}
                >
                  {t('移除已上传 Logo')}
                </button>
              )}
            </div>
            <span className="form-hint">
              {t('支持 PNG、JPG、WebP，最大 2 MiB、4096 × 4096 像素；自动缩小至 512 像素以内，建议使用正方形图片。')}
            </span>
            {pendingLogo === null && (
              <span className="form-hint">{t('保存后将移除已上传的 Logo，回落到默认 Logo 地址。')}</span>
            )}
            {pendingLogo === undefined && !logoUploaded && (
              <span className="form-hint">{t('当前使用配置的 Logo 地址（默认 /logo.png）。')}</span>
            )}
          </div>

          <div className="form-group">
            <label>{t('favicon 地址（可选）')}</label>
            <input
              className="form-input"
              value={faviconUrl}
              placeholder={t('留空则沿用 Logo')}
              onChange={(e) => setFaviconUrl(e.target.value)}
            />
            <span className="form-hint">
              {t('浏览器页签图标；留空则使用 Logo 地址。')}
            </span>
          </div>

          {error && <div className="error-message">{error}</div>}

          <div className="settings-actions">
            <button className="btn btn-primary" onClick={save} disabled={saving}>
              {saving ? t('保存中...') : t('保存更改')}
            </button>
            {defaults && (
              <button className="btn btn-secondary" onClick={resetToDefaults} disabled={saving}>
                {t('恢复默认')}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
