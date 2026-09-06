import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { ShieldAlert } from 'lucide-react';

/**
 * PermissionDenied — 普通用户误入管理员页面的友好提示。
 * 不做跳转/不触发管理接口，避免被 401 误踢出登录态。
 */
export default function PermissionDenied(): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="route-error-page">
      <div className="route-error-card">
        <div className="route-error-icon" aria-hidden="true">
          <ShieldAlert size={36} strokeWidth={1.5} />
        </div>
        <h1>{t('无权访问')}</h1>
        <p className="route-error-detail">
          {t('此页面仅管理员可用。你当前登录为普通用户，可前往 Playground、API 密钥或钱包。')}
        </p>
        <div className="route-error-actions">
          <Link className="btn btn-primary btn-sm" to="/">{t('回到首页')}</Link>
          <Link className="btn btn-outline btn-sm" to="/playground">{t('去 Playground')}</Link>
        </div>
      </div>
    </div>
  );
}
