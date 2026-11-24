/**
 * 模型筛选（对齐 Agent2API v3.5.1 `routing-model-filter.tsx` 的展示位）。
 *
 * A2A 的这一块只做一件事：**按模型过滤右侧账号列表** —— 不改路由配置、
 * 不替模型工作区。空值 = 全部。
 *
 * A2A 用 Combobox 做可搜索下拉；AIGX 的原生 `select` 自带键盘首字母跳选，
 * 候选量级（一个网关的模型数）也不到需要虚拟滚动的程度，故落成
 * 「标签 + RoutingSelect + 同一句说明」，语义与 A2A 一致。
 *
 * 候选来自 `workspace.models`（AIGX 侧由页面传入 `models`）；最近使用时间
 * 只影响排序，不影响可选集合。
 */
import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { RoutingSelect } from './routing-select';
import { sortRoutingModels } from './routing-model-order';

export function RoutingModelFilter({ value, models, lastUsed, disabled = false, onChange }: {
  /** 空串 = 全部 */
  value: string;
  models: string[];
  lastUsed?: Record<string, number>;
  disabled?: boolean;
  onChange(model: string): void;
}): JSX.Element {
  const { t } = useTranslation();
  const ordered = useMemo(() => sortRoutingModels(models, (model) => model, lastUsed), [models, lastUsed]);
  const options: Array<[string, string]> = [
    ['', t('全部')],
    ...ordered.map((model): [string, string] => [model, model]),
  ];
  return (
    <div className="rt-model-filter">
      <span className="rt-model-filter-label">{t('筛选模型')}</span>
      <RoutingSelect
        className="rt-model-filter-select"
        label={t('筛选模型')}
        value={value}
        disabled={disabled || !models.length}
        options={options}
        onChange={onChange}
      />
      <p className="rt-config-note">{t('仅筛选右侧账号列表，不改变路由配置。')}</p>
      {!models.length ? (
        <p className="rt-config-note">{t('尚未读取到模型列表，暂时无法筛选。')}</p>
      ) : null}
    </div>
  );
}