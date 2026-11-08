/**
 * TriStateField —— 路由档案字段的**三态**控件（#40 契约的核心，别退化成两态）。
 *
 * 为什么不能用普通 checkbox / 输入框：那只有两态（有值 / 无值），
 * 而档案字段需要区分三种意图：
 *
 *   继承下层 | 显式覆盖（本层说了算） | 显式清空（本层说了算：就是空）
 *
 * 缺了「显式清空」，模型级就无法「不用 global 的参与名单」；缺了「继承」，
 * 改 global 时模型级不会跟着变（静默的规则漂移，配置界面看不出）。
 *
 * 界面保证：
 * 1. 三档分段控件始终可见 —— 当前处于哪一态一眼可辨，不靠「输入框空着」猜；
 * 2. **只有覆盖态才显示编辑控件**（继承/清空态显示的是「会发生什么」的说明）；
 * 3. 清空态必须给出该字段「清空成什么」的白话说明（不同字段清空含义不同）；
 * 4. 继承态显示下层推算值 —— 否则用户不知道「不表态」最终会得到什么；
 * 5. 右上角 provenance 徽章显示该字段**权威来源**（后端解析结果），
 *    未拿到后端来源时显示「来源未知」而**不猜**（本仓 #31 的教训：界面不许撒谎）。
 *
 * 枚举类字段（strategy / sticky）没有「空」语义，用 `allowClear={false}` 只给两档。
 */
import React from 'react';
import { useTranslation } from 'react-i18next';
import type { FieldState, Layer } from '../../types/routing-profile';

/** 层 → 人类可读名（唯一映射处，别在别处再写一遍） */
export function layerLabel(t: (s: string) => string, layer: Layer): string {
  switch (layer) {
    case 'global':
      return t('全局');
    case 'model':
      return t('模型级');
    case 'group':
      return t('路由组');
  }
}

/** 来源徽章：值由哪一层提供（后端 provenance 的呈现） */
export function LayerChip({
  layer,
  size = 'sm',
}: {
  /** `undefined` = 后端还没给来源（别猜，显示「来源未知」）；`null` = 没有任何层提供（走内置默认） */
  layer: Layer | null | undefined;
  size?: 'sm' | 'md';
}): JSX.Element {
  const { t } = useTranslation();
  if (layer === undefined) {
    return (
      <span className={`rt-chip rt-chip-unknown rt-chip-${size}`} title={t('后端未返回该字段的来源')}>
        {t('来源未知')}
      </span>
    );
  }
  if (layer === null) {
    return (
      <span className={`rt-chip rt-chip-default rt-chip-${size}`} title={t('没有任何档案层设置该字段，走内置默认值')}>
        {t('内置默认')}
      </span>
    );
  }
  return (
    <span
      className={`rt-chip rt-chip-${layer} rt-chip-${size}`}
      title={t('该字段的值来自「{{layer}}」层', { layer: layerLabel(t, layer) })}
    >
      {layerLabel(t, layer)}
    </span>
  );
}

/** 三态说明文案的键（各字段清空含义不同，故必须逐字段给） */
export interface TriStateFieldProps {
  label: React.ReactNode;
  /** 字段用途说明（一句话） */
  hint?: React.ReactNode;
  state: FieldState;
  onStateChange: (next: FieldState) => void;
  /** false = 该字段无「空」语义（枚举），只给继承 / 覆盖两档 */
  allowClear?: boolean;
  /** 清空态的白话说明：清空成什么 */
  clearNote: React.ReactNode;
  /** 继承态的说明：不表态时最终会得到什么（通常带下层推算值） */
  inheritNote: React.ReactNode;
  /** 该字段的权威来源（后端 provenance）；`undefined` = 后端未提供该来源 */
  provenance?: Layer | null;
  /** 覆盖态下渲染的编辑控件 */
  children?: React.ReactNode;
}

export default function TriStateField({
  label,
  hint,
  state,
  onStateChange,
  allowClear = true,
  clearNote,
  inheritNote,
  provenance,
  children,
}: TriStateFieldProps): JSX.Element {
  const { t } = useTranslation();
  const options: Array<{ key: FieldState; label: string; title: string }> = [
    { key: 'inherit', label: t('继承'), title: t('本层不表态：该字段沿用下层的值，下层改了这里跟着变') },
    { key: 'override', label: t('覆盖'), title: t('本层说了算：显式设为下面填写的值，下层改了这里不受影响') },
  ];
  if (allowClear) {
    options.push({
      key: 'clear',
      label: t('清空'),
      title: t('本层说了算：显式置为空，不再沿用下层（清空成什么见下方说明）'),
    });
  }

  return (
    <div className={`rt-field rt-field-${state}`}>
      <div className="rt-field-head">
        <div className="rt-field-title">
          <span className="rt-field-label">{label}</span>
          {/* 来源徽章只在**确实拿到了后端来源**时显示：没拿到就不占位，
              免得六个字段常年挂着「来源未知」把徽章变成噪音（权威来源的
              「未知」提示在预览表里给，那里用户正要看来源）。 */}
          {provenance !== undefined ? <LayerChip layer={provenance} /> : null}
        </div>
        <div className="rt-seg" role="radiogroup" aria-label={typeof label === 'string' ? label : undefined}>
          {options.map((o) => (
            <button
              key={o.key}
              type="button"
              role="radio"
              aria-checked={state === o.key}
              title={o.title}
              className={`rt-seg-btn${state === o.key ? ' is-on' : ''}`}
              onClick={() => onStateChange(o.key)}
            >
              {o.label}
            </button>
          ))}
        </div>
      </div>

      {hint ? <div className="form-hint rt-field-hint">{hint}</div> : null}

      <div className="rt-field-body">
        {state === 'override' ? (
          children
        ) : state === 'clear' ? (
          <div className="rt-state-note rt-state-clear">
            <strong>{t('清空：')}</strong>
            {clearNote}
          </div>
        ) : (
          <div className="rt-state-note rt-state-inherit">
            <strong>{t('继承：')}</strong>
            {inheritNote}
          </div>
        )}
      </div>
    </div>
  );
}
