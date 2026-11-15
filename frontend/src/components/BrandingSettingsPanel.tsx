/**
 * 面板品牌设置（系统设置 → 通用 → 品牌）
 *
 * 对照 Agent2API 3.5.0：面板名称 / Logo / favicon 自定义。
 * - 保存后立即生效（无需重启）：配置写入 config.toml，前端刷新页面即读到新值。
 * - Logo 支持外链（http(s) 绝对 URL）或站内路径（以 `/` 开头）。
 * - URL 校验由后端完成（拒绝 javascript:/data: 等可执行协议）；前端只展示错误。
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from './Toast';
import type { BrandingData } from '../types';

export default function BrandingSettingsPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [panelName, setPanelName] = useState('AIGX');
  const [logoUrl, setLogoUrl] = useState('/logo.png');
  const [faviconUrl, setFaviconUrl] = useState('');
  const [defaults, setDefaults] = useState<BrandingData | null>(null);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getBranding();
        const d = res?.data;
        if (!d) return;
        setPanelName(d.current.panel_name || 'AIGX');
        setLogoUrl(d.current.logo_url || '/logo.png');
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

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      const res = await api.updateBranding({
        panel_name: panelName.trim(),
        logo_url: logoUrl.trim(),
        favicon_url: faviconUrl.trim(),
      });
      if (res.success && res.data) {
        addToast(t('品牌设置已保存'), 'success');
        setPanelName(res.data.panel_name);
        setLogoUrl(res.data.logo_url);
        setFaviconUrl(res.data.favicon_url === res.data.logo_url ? '' : res.data.favicon_url);
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
    setLogoUrl(defaults.logo_url);
    setFaviconUrl('');
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

  return (
    <div className="card" style={{ marginTop: 16 }}>
      <div className="card-header">
        <h2>{t('品牌')}</h2>
        <p className="card-subtitle">
          {t('自定义面板名称、Logo 与 favicon；保存后刷新页面生效。')}
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
            <label>{t('Logo 地址')}</label>
            <input
              className="form-input"
              value={logoUrl}
              placeholder="/logo.png"
              onChange={(e) => setLogoUrl(e.target.value)}
            />
            <span className="form-hint">
              {t('支持 http(s) 外链或站内路径（以 / 开头）；默认 /logo.png。')}
            </span>
            {logoUrl.trim() && (
              <div style={{ marginTop: 8 }}>
                <img
                  src={logoUrl}
                  alt={t('Logo 预览')}
                  width={44}
                  height={44}
                  style={{ borderRadius: 4, objectFit: 'contain' }}
                  onError={(e) => {
                    (e.currentTarget as HTMLImageElement).style.display = 'none';
                  }}
                />
              </div>
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
