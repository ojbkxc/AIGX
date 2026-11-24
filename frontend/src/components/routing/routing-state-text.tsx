/**
 * 选路状态的统一文案（对齐 Agent2API v3.5.1 `routing-state-text.tsx`）。
 *
 * 与 `Badge` 同口径，但这里只输出「点 + 文案」，不带彩色底 —— 卡片行里
 * 已经有开关和操作按钮，再加一块色底会把注意力从名字上抢走。
 *
 * 词表与 A2A 逐项对齐；AIGX 后端目前只下发 A2A 词表的一个子集
 * （账号状态在 AIGX 侧没有独立端点），未知状态落到「未就绪」而不是猜。
 */
import type React from 'react';
import { useTranslation } from 'react-i18next';

export function stateLabel(t: (s: string) => string, state?: string): string {
  return state === 'usable' ? t('可用') : state === 'cooldown' ? t('冷却中')
    : state === 'concurrency' ? t('并发已满') : state === 'disabled' ? t('已禁用')
      : state === 'no-credentials' ? t('无凭证') : state === 'limiter' ? t('限制器跳过')
        : state === 'excluded' ? t('未参与') : state === 'tried' ? t('已尝试')
          : state === 'enabled' ? t('已启用') : state === 'missing' ? t('账号已移除')
            : t('未就绪');
}

export function stateTone(state?: string): 'success' | 'warning' | 'danger' | 'muted' {
  if (state === 'usable' || state === 'enabled') return 'success';
  if (state === 'cooldown' || state === 'concurrency' || state === 'limiter') return 'warning';
  if (state === 'no-credentials' || state === 'tried') return 'danger';
  return 'muted';
}

/** 点 + 文案（A2A `RoutingStateText`）；`children` 给了就用它替代词表文案。 */
export function RoutingStateText({
  state,
  children,
  title,
}: {
  state?: string;
  children?: React.ReactNode;
  title?: string;
}): JSX.Element {
  const { t } = useTranslation();
  const tone = stateTone(state);
  return (
    <span className={`rt-state-text rt-tone-${tone}`} title={title}>
      <i className="rt-dot" aria-hidden="true" />
      {children ?? stateLabel(t, state)}
    </span>
  );
}