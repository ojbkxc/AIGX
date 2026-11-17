/**
 * 「系统已升级，请刷新」提示条。
 *
 * 只在前端 bundle 与当前部署版本不一致时出现（见 `useVersionGuard`）。
 * 用固定底栏而不是 toast：这个状态**不会自己消失**，用会淡出的 toast
 * 反而让人以为点掉了就没事了。
 */
import { useTranslation } from 'react-i18next';
import { RefreshCw } from 'lucide-react';
import { useVersionGuard } from '../hooks/useVersionGuard';

export default function VersionGuardBanner(): JSX.Element | null {
  const { t } = useTranslation();
  const { stale, deployed } = useVersionGuard();
  if (!stale) return null;
  return (
    <div className="version-guard-banner" role="status" aria-live="polite">
      <span>
        {t('系统已升级到 v{{version}}，当前页面仍是旧版本，请刷新', { version: deployed })}
      </span>
      <button type="button" onClick={() => window.location.reload()}>
        <RefreshCw size={14} />
        {t('刷新页面')}
      </button>
    </div>
  );
}
