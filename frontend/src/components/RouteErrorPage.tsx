import { Link, useLocation, useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';

/**
 * RouteErrorPage — 路由层兜底错误页（errorElement）。
 * 与 ErrorBoundary 分工：Boundary 捕渲染异常，本页接路由 loader/未匹配错误。
 * 404/403/500 三态 + 一键回首页/重试。
 *
 * 注意：本组件**不调用 `useRouteError()`**。那个 hook 只在 `errorElement`
 * 上下文里安全；catch-all 404 是普通 `<Route element>`，调用它会直接抛错，
 * 进而被外层 ErrorBoundary 兜成「出错了」—— 用户看不到 404 页。状态码由
 * 调用方通过 `status` 传入即可。
 */
interface RouteErrorPageProps {
  /** 显式传入的状态码（catch-all 路由给 404） */
  status?: number;
}

export default function RouteErrorPage({ status: statusProp }: RouteErrorPageProps): JSX.Element {
  const navigate = useNavigate();
  const location = useLocation();
  const { t } = useTranslation();

  const status = statusProp ?? 500;
  const message = t('发生未知错误');

  const is404 = status === 404;
  const icon = is404 ? '🧭' : status === 403 ? '🔒' : '⚠️';

  return (
    <div className="route-error-page">
      <div className="route-error-card">
        <div className="route-error-icon" aria-hidden="true">{icon}</div>
        <h1>{is404 ? t('页面不存在') : status === 403 ? t('无权访问') : t('出错了')}</h1>
        <p className="route-error-detail">
          {is404
            ? t('地址 %path% 不存在或已被移动。').replace('%path%', location.pathname)
            : message}
        </p>
        {!is404 && <code className="route-error-code">{String(status)}</code>}
        <div className="route-error-actions">
          <button
            type="button"
            className="btn btn-outline btn-sm"
            onClick={() => navigate(-1)}
          >
            {t('返回上页')}
          </button>
          <Link className="btn btn-primary btn-sm" to="/">{t('回到首页')}</Link>
        </div>
      </div>
    </div>
  );
}
