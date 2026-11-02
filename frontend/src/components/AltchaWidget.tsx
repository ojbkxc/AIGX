import { useTranslation } from 'react-i18next';
import type { AltchaState } from '../hooks/useAltcha';

/**
 * 机器人校验（ALTCHA proof-of-work）状态块 —— 登录页 / 注册页共用。
 *
 * 只负责显示与「失败后点击重试」；领题与求解在 `useAltcha` 里。
 * `state === 'disabled'` 时（后端开关关闭）整块不渲染。
 */
export default function AltchaWidget({
  state,
  onRetry,
}: {
  state: AltchaState;
  onRetry: () => void;
}): JSX.Element | null {
  const { t } = useTranslation();
  if (state === 'disabled') return null;

  const label =
    state === 'verifying'
      ? t('验证中…')
      : state === 'verified'
        ? t('验证成功')
        : t('验证失败，请重试');

  return (
    <div
      role={state === 'error' ? 'button' : undefined}
      tabIndex={state === 'error' ? 0 : undefined}
      onClick={state === 'error' ? onRetry : undefined}
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: 8,
        padding: '10px 12px',
        marginBottom: 12,
        borderRadius: 8,
        border: '1px solid var(--border-color, #ddd)',
        fontSize: 13,
        cursor: state === 'error' ? 'pointer' : 'default',
        opacity: state === 'verified' ? 0.85 : 1,
      }}
    >
      <span
        aria-hidden="true"
        className={state === 'verifying' ? 'altcha-spinner' : undefined}
        style={{
          width: 16,
          height: 16,
          borderRadius: '50%',
          flex: '0 0 auto',
          background:
            state === 'verified'
              ? 'var(--success-color, #16a34a)'
              : state === 'error'
                ? 'var(--error-color, #dc2626)'
                : 'var(--accent-color, #2563eb)',
        }}
      />
      <span>{label}</span>
    </div>
  );
}