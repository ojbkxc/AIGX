/**
 * GatewayRuntimePanel — 网关运行参数面板（「渠道与账号池」页 Tab「网关」）。
 *
 * 07 报告 D.3-④：把「网关」这一层显式化。网关监听地址/端口/CORS/反代信任
 * 的**编辑入口**一直在「系统设置 → 网关」（GatewayPanel），但用户在
 * 「渠道与账号池」页配置上游时看不到网关自己的运行口径——到底监听在哪、
 * 客户端该往哪填。本面板只读展示运行参数 + 指引去设置页改（单一编辑入口，
 * 不做第二份表单，避免两处可改的漂移）。
 *
 * 数据源：GET /api/gateway/settings（adminOnly；本页本身是 AdminRoute 包裹）。
 * 形状对齐 GatewaySettingsItem（types/index.ts）。
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { ExternalLink, RefreshCw } from 'lucide-react';
import { api } from '../api';
import { Badge, Button, Card, EmptyState, Loading } from '../components/ui';
import type { GatewaySettingsItem } from '../types';

/** 通配监听地址：客户端不能直接填它（要用机器 IP / 站点域名） */
const WILDCARD_HOSTS = ['0.0.0.0', '::', '::0'];

export default function GatewayRuntimePanel(): JSX.Element {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [gs, setGs] = useState<GatewaySettingsItem | null>(null);

  const load = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getGatewaySettings();
      if (res?.success && res?.data) setGs(res.data);
      else setError(res?.error?.message || t('网关运行参数加载失败'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const isWildcard = Boolean(gs?.host && WILDCARD_HOSTS.includes(gs.host.toLowerCase()));
  const loopback = gs?.host === '127.0.0.1' || gs?.host === 'localhost';

  return (
    <div>
      <Card
        title={t('网关运行参数')}
        actions={
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading} style={{ gap: 6 }}>
            <RefreshCw size={13} /> {t('刷新')}
          </Button>
        }
      >
        {loading ? (
          <Loading text={t('加载网关运行参数')} />
        ) : error ? (
          <EmptyState
            message={error}
            action={<Button size="sm" onClick={() => void load()}>{t('重试')}</Button>}
          />
        ) : !gs ? (
          <EmptyState message={t('暂无网关运行参数')} />
        ) : (
          <>
            <p className="gw-note">
              {t('网关自身的监听口径：客户端的 API 地址 / Base URL 按这里的地址填。修改入口在「系统设置 → 网关」（本面板只读，避免两处可改）。')}
            </p>
            <div className="gw-runtime-grid">
              <div className="gw-runtime-item">
                <div className="gw-runtime-label">{t('监听地址')}</div>
                <div className="gw-runtime-value">
                  <code>{gs.host || '—'}</code>{' '}
                  {isWildcard ? (
                    <Badge tone="info">{t('通配 · 局域网可访问')}</Badge>
                  ) : loopback ? (
                    <Badge tone="warning">{t('仅本机')}</Badge>
                  ) : null}
                </div>
              </div>
              <div className="gw-runtime-item">
                <div className="gw-runtime-label">{t('端口')}</div>
                <div className="gw-runtime-value"><code>{gs.port ?? '—'}</code></div>
              </div>
              <div className="gw-runtime-item">
                <div className="gw-runtime-label">{t('CORS 允许来源')}</div>
                <div className="gw-runtime-value">
                  {gs.corsOrigins?.length
                    ? gs.corsOrigins.map((o) => <code key={o} className="gw-chip">{o}</code>)
                    : <Badge tone="neutral">{t('默认（站点地址 / 本机）')}</Badge>}
                </div>
              </div>
              <div className="gw-runtime-item">
                <div className="gw-runtime-label">{t('信任反代头')}</div>
                <div className="gw-runtime-value">
                  <Badge tone={gs.trustProxyHeaders ? 'success' : 'neutral'}>
                    {gs.trustProxyHeaders ? t('开启（XFF 采信）') : t('关闭')}
                  </Badge>
                </div>
              </div>
            </div>
            <div className="gw-runtime-actions">
              <Link to="/settings" className="btn btn-outline btn-sm" style={{ gap: 6 }}>
                <ExternalLink size={13} /> {t('去系统设置修改网关参数')}
              </Link>
            </div>
          </>
        )}
      </Card>
    </div>
  );
}
