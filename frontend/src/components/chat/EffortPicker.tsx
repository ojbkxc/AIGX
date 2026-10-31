/**
 * 思考强度选择器：关/低/中/高 循环 pill（chat 页与运维页通用）。
 *
 * 此前两页各写了一份且交互不一致（chat 是下拉+开关不持久化，运维是循环
 * pill 持久化）。统一为运维页形态：点击循环切换，storageKey 持久化。
 */
import { useCallback, useState } from 'react';
import { useTranslation } from 'react-i18next';

export type Effort = 'off' | 'low' | 'medium' | 'high';

const EFFORT_CYCLE: Effort[] = ['off', 'low', 'medium', 'high'];
const EFFORT_LABEL: Record<Effort, string> = {
  off: '关',
  low: '低',
  medium: '中',
  high: '高',
};

export interface EffortPickerProps {
  /** localStorage 键（各自页面独立记忆） */
  storageKey: string;
  /** 受控值（不传则内部持久化管理） */
  value?: Effort;
  onChange?: (effort: Effort) => void;
  disabled?: boolean;
}

function readInitial(storageKey: string): Effort {
  try {
    const raw = window.localStorage.getItem(storageKey);
    if (raw && (EFFORT_CYCLE as string[]).includes(raw)) return raw as Effort;
  } catch {
    /* 隐私模式等场景忽略 */
  }
  return 'off';
}

export default function EffortPicker({
  storageKey,
  value,
  onChange,
  disabled,
}: EffortPickerProps): JSX.Element {
  const { t } = useTranslation();
  const [internal, setInternal] = useState<Effort>(() => readInitial(storageKey));
  const current = value ?? internal;

  const cycle = useCallback(() => {
    if (disabled) return;
    const idx = EFFORT_CYCLE.indexOf(current);
    const next = EFFORT_CYCLE[(idx + 1) % EFFORT_CYCLE.length];
    if (value === undefined) {
      setInternal(next);
    }
    try {
      window.localStorage.setItem(storageKey, next);
    } catch {
      /* 忽略 */
    }
    onChange?.(next);
  }, [current, disabled, onChange, storageKey, value]);

  return (
    <button
      type="button"
      className={`effort-pill${current !== 'off' ? ' active' : ''}`}
      onClick={cycle}
      disabled={disabled}
      title={t('思考强度（点击切换）')}
      aria-label={`${t('思考强度')}: ${t(EFFORT_LABEL[current])}`}
    >
      {t('思考')}
      {current !== 'off' ? ` ${t(EFFORT_LABEL[current])}` : ''}
    </button>
  );
}
