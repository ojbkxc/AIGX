import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from '../components/Toast';

/**
 * 网关设置（系统设置 → 运维 → 网关）
 *
 * - 监听地址 / 端口：`0.0.0.0` = 允许局域网访问（同网段设备可直接指向本机 IP）
 * - CORS 来源：浏览器跨源调用 `/api`、`/v1` 的允许来源（一行一个；`*` 表示不限）
 * - 信任反代头：仅在 Nginx/CF 之后部署时开启（否则 XFF 可被伪造）
 *
 * 监听地址 / 端口 / CORS 属于启动期配置，保存后需重启服务生效（设置 → 更新 → 重启服务）；
 * 信任反代头通过进程级快照，保存即生效。
 */
export default function GatewayPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [host, setHost] = useState('0.0.0.0');
  const [port, setPort] = useState('9527');
  const [origins, setOrigins] = useState('');
  const [trustProxy, setTrustProxy] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        const res = await api.getGatewaySettings();
        const d = res?.data;
        if (!d) return;
        setHost(d.host || '0.0.0.0');
        setPort(String(d.port ?? 9527));
        setOrigins((d.corsOrigins || []).join('\n'));
        setTrustProxy(Boolean(d.trustProxyHeaders));
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
      const portVal = Number(port);
      if (!Number.isFinite(portVal) || portVal < 1 || portVal > 65535) {
        setError(t('端口必须在 1-65535 之间'));
        return;
      }
      const corsOrigins = origins
        .split('\n')
        .map((line) => line.trim())
        .filter((line) => line.length > 0);
      const res = await api.saveGatewaySettings({
        host: host.trim(),
        port: portVal,
        cors_origins: corsOrigins,
        trust_proxy_headers: trustProxy,
      });
      const d = res?.data;
      if (d) {
        setHost(d.host || host);
        setPort(String(d.port ?? portVal));
        setOrigins((d.corsOrigins || []).join('\n'));
        setTrustProxy(Boolean(d.trustProxyHeaders));
      }
      addToast(t('网关设置已保存（信任反代头立即生效，监听地址/端口/CORS 需重启）'));
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
        <h2>{t('网关')}</h2>
        <p className="card-subtitle">
          {t('监听地址、跨源访问与反向代理信任；信任反代头立即生效，其余保存后需重启服务生效。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          <div style={{ display: 'grid', gridTemplateColumns: '2fr 1fr', gap: 12 }}>
            <div className="form-group">
              <label>{t('监听地址')}</label>
              <input
                className="form-input"
                value={host}
                onChange={(e) => setHost(e.target.value)}
                placeholder="0.0.0.0"
              />
              <div className="form-hint">
                {t('0.0.0.0 = 局域网可访问；127.0.0.1 = 仅本机可访问。')}
              </div>
            </div>
            <div className="form-group">
              <label>{t('端口')}</label>
              <input
                className="form-input"
                type="number"
                min={1}
                max={65535}
                value={port}
                onChange={(e) => setPort(e.target.value)}
              />
            </div>
          </div>

          <div className="form-group">
            <label>{t('CORS 允许来源（一行一个，* = 不限）')}</label>
            <textarea
              className="form-input"
              rows={4}
              placeholder={'https://admin.example.com\nhttp://127.0.0.1:5173'}
              value={origins}
              onChange={(e) => setOrigins(e.target.value)}
            />
            <div className="form-hint">
              {t('留空时：有站点地址则只允许该地址，否则仅放行本机来源；跨源直连 /v1 需要显式配置。')}
            </div>
          </div>

          <div className="form-group">
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input
                type="checkbox"
                checked={trustProxy}
                onChange={(e) => setTrustProxy(e.target.checked)}
              />
              <span>{t('信任反向代理的 X-Forwarded-For / X-Real-IP')}</span>
            </label>
            <div className="form-hint">
              {t('仅在 Nginx / Cloudflare 等可信反代之后开启；直连部署开启会让客户端伪造 IP 绕过 IP 规则。')}
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
