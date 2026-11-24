/**
 * 签到中心的**提供商分类栏**（对齐 Agent2API `checkin-provider-rail.tsx`）。
 *
 * ── 它为什么跟着分区变 ──────────────────────────────────────
 * 「每日签到 / 新手福利 / 活动福利 / 成长任务」是**任务**，提供商是同一任务下的
 * **分类**。所以左栏不是固定的：切到某个任务时只列这个任务里真正有账号的家
 * —— 否则用户会在「成长任务」下面看到一堆点进去空空如也的家。
 *
 * ── 计数口径 ────────────────────────────────────────────────
 * 每家后面是 `已完成 / 总数`。计数由调用方按**当前分区**算好传进来（各分区的
 * 「完成」判据不同：签到看今日已签、福利看是否领完、成长看任务树），这里只负责
 * 画。没完成的用警示色，不额外挂徽标 —— 徽标会把提供商名字挤断。
 *
 * ── 没有账号的家收进折叠组 ──────────────────────────────────
 * A2A `ProviderRail` 的同一取舍：十几个「点了必然空转」的行会把真正能点的
 * 挤到看不见；折叠组默认收起，选中折叠组里的家时自动展开。
 */

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ChevronRight } from 'lucide-react';
import { providerIcon } from '../lib/providerIcons';

export interface ProviderOption {
  id: string;
  label: string;
  /** 已完成的账号数；`null` = 这一档没有「完成」概念（活动福利按领取算，逐行才知道） */
  done: number | null;
  /** 这一家在**当前分区**里的账号总数 */
  total: number;
  /** 仅由插件自动化任务挂载（无逐账号列表），不归入「暂无账号」折叠组 */
  isTask?: boolean;
}

/** 一家的小图标：有内置图标用图标，否则用首字母方块（与账号页同一观感） */
function RailLogo({ id, label }: { id: string; label: string }): JSX.Element {
  const icon = providerIcon(id);
  if (icon) return <img className="ck-rail-logo" src={icon} alt="" />;
  return <span className="ck-rail-logo ck-rail-letter">{(label || id || '·').slice(0, 1).toUpperCase()}</span>;
}

export default function CheckinProviderRail({
  options,
  selected,
  total,
  onSelect,
}: {
  options: ProviderOption[];
  selected: string;
  total: { done: number; all: number };
  onSelect: (id: string) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const isEmptyOption = (option: ProviderOption): boolean => option.isTask !== true && option.total === 0;
  const activeOptions = options.filter((option) => !isEmptyOption(option));
  const emptyOptions = options.filter(isEmptyOption);
  const [emptyExpanded, setEmptyExpanded] = useState(false);
  const hasSelectedEmpty = emptyOptions.some((option) => option.id === selected);

  // 选中折叠组里的家时自动展开：否则「选中了却看不见它」是最典型的困惑
  useEffect(() => {
    if (hasSelectedEmpty) setEmptyExpanded(true);
  }, [hasSelectedEmpty]);

  const renderOption = (option: ProviderOption): JSX.Element => (
    <button
      key={option.id}
      type="button"
      className={`ck-rail-item${selected === option.id ? ' is-active' : ''}`}
      onClick={() => onSelect(option.id)}
    >
      <RailLogo id={option.id} label={option.label} />
      <span className="ck-rail-name">{t(option.label)}</span>
      <span
        className={`ck-rail-count${
          option.done !== null && option.done < option.total ? ' pending' : ''
        }`}
      >
        {option.done === null ? option.total : `${option.done}/${option.total}`}
      </span>
    </button>
  );

  return (
    <aside className="ck-provider-rail" aria-label={t('提供商')}>
      <div className="ck-rail-head">
        <h3>{t('提供商')}</h3>
        <span>{t('{{n}} 家', { n: options.length })}</span>
      </div>
      <button
        type="button"
        className={`ck-rail-item${selected === '' ? ' is-active' : ''}`}
        onClick={() => onSelect('')}
      >
        <span className="ck-rail-logo ck-rail-letter">≡</span>
        <span className="ck-rail-name">{t('全部提供商')}</span>
        <span className={`ck-rail-count${total.done < total.all ? ' pending' : ''}`}>
          {total.done}/{total.all}
        </span>
      </button>
      {activeOptions.map(renderOption)}
      {emptyOptions.length > 0 && (
        <>
          <button
            type="button"
            className="ck-rail-item ck-rail-item-fold"
            aria-expanded={emptyExpanded}
            onClick={() => setEmptyExpanded((open) => !open)}
          >
            <ChevronRight size={13} className={`ck-prov-caret${emptyExpanded ? ' is-open' : ''}`} />
            <span className="ck-rail-name">{t('暂无账号')}</span>
            <span className="ck-rail-count">{emptyOptions.length}</span>
          </button>
          {emptyExpanded && emptyOptions.map(renderOption)}
        </>
      )}
      {emptyOptions.length === 0 && (
        <p className="ck-rail-note">{t('只列当前任务里有账号的提供商。')}</p>
      )}
    </aside>
  );
}
