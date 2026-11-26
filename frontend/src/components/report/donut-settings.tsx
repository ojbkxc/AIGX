// 用量分布饼图的「合并小项」设置（对齐 A2A report/usage-donuts.tsx:10-76）：
// 开关 + 阈值下拉，落 localStorage 跨会话记住；同一份设置同时作用于模型饼图与
// 供应商饼图（A2A 也是两个饼图共用一套 settings，右侧列表始终完整）。
//
// 与 A2A 的差异：A2A 把设置藏在 Popover 里（标题行只显示一行摘要），AIGX 没有
// Popover 组件，改成就地一个开关 + 一个下拉 —— 报表页本来就是一屏多控件，
// 多一层弹层反而多一次点击。
import { useTranslation } from 'react-i18next';
import Switch from '../ui/Switch';
import './report-panels.css';

/** 阈值档位（与 A2A DONUT_THRESHOLDS 逐值一致） */
export const DONUT_THRESHOLDS = [0.1, 0.5, 1, 2, 3, 5] as const;

export interface DonutSettings {
  /** 是否按阈值合并小项 */
  enabled: boolean;
  /** 占比低于该百分比（%）的扇区归入「其他」 */
  threshold: number;
}

/** A2A DEFAULT_DONUT_SETTINGS = { enabled: true, threshold: 1 } */
export const DEFAULT_DONUT_SETTINGS: DonutSettings = { enabled: true, threshold: 1 };

const SETTINGS_KEY = 'aigx-report-donut-settings';

/** 读设置：存坏/越界一律回落默认（阈值不在档位表里就是存坏，不猜一个相近值） */
export function readDonutSettings(): DonutSettings {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (!raw) return DEFAULT_DONUT_SETTINGS;
    const saved: unknown = JSON.parse(raw);
    if (!saved || typeof saved !== 'object') return DEFAULT_DONUT_SETTINGS;
    const rec = saved as Record<string, unknown>;
    const threshold = DONUT_THRESHOLDS.find((v) => v === rec.threshold) ?? DEFAULT_DONUT_SETTINGS.threshold;
    return {
      enabled: typeof rec.enabled === 'boolean' ? rec.enabled : DEFAULT_DONUT_SETTINGS.enabled,
      threshold,
    };
  } catch {
    return DEFAULT_DONUT_SETTINGS;
  }
}

/** 写设置：存储不可用只影响下次打开，本次会话照常生效（调用方用内存态渲染） */
export function saveDonutSettings(next: DonutSettings): void {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(next));
  } catch {
    /* 忽略持久化失败 */
  }
}

/** 开关 + 阈值（挂在饼图面板标题右侧） */
export function DonutSettingsControl({
  settings,
  onChange,
}: {
  settings: DonutSettings;
  onChange: (next: DonutSettings) => void;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="donut-settings">
      <span>{t('合并小项')}</span>
      <Switch
        checked={settings.enabled}
        onChange={(enabled) => onChange({ ...settings, enabled })}
        ariaLabel={t('合并小项')}
      />
      <select
        className="donut-settings-select"
        value={String(settings.threshold)}
        disabled={!settings.enabled}
        aria-label={t('小项合并阈值')}
        title={t('占比低于该阈值的扇区归入「其他」；右侧列表仍完整显示每一项')}
        onChange={(e) => {
          const threshold = DONUT_THRESHOLDS.find((v) => String(v) === e.target.value);
          if (threshold !== undefined) onChange({ ...settings, threshold });
        }}
      >
        {DONUT_THRESHOLDS.map((v) => (
          <option key={v} value={String(v)}>{`${v}%`}</option>
        ))}
      </select>
    </div>
  );
}

/** 设置 → DonutChart 的 minPercent（关掉时传 0，回到纯 maxSlices 行为） */
export function donutMinPercent(settings: DonutSettings): number {
  return settings.enabled ? settings.threshold : 0;
}
