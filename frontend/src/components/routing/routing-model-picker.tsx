/**
 * 模型选择器（对齐 Agent2API v3.5.1 `routing-model-picker.tsx`，交互逐项同义）。
 *
 * 结构：左「提供商」列（搜索 + 全部 + 各提供商计数） / 右「模型」列
 * （当前提供商或「全部」+ 搜索 + 复选行） / 底部已选计数 + 清空选择 + 上限提示。
 *
 * A2A 的这一屏是「跨提供商、跨模型多选」；选中态用 `${provider}\0${model}` 键，
 * 与建组时的成员键同源（`routeModelKey`），所以「已选 N / 总数」不会因为
 * 大小写不同的模型名算成两条。
 *
 * AIGX 侧的数据缺口（如实登记，不伪造）：模型目录（provider + model 候选）
 * 与「最近使用时间」目前没有接进路由页 —— 组件按 A2A 契约收 `choices` /
 * `lastUsed`，传空数组时渲染的正是 A2A 的空态「没有可添加的模型」。
 */
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Search } from 'lucide-react';
import { RoutingProviderMark } from './routing-actions';
import { routeModelKey, ROUTING_MEMBER_LIMIT } from './routing-group-form';
import { sortRoutingModels } from './routing-model-order';

export interface RouteModelChoice {
  provider: string;
  /** 展示名（缺省 = provider id） */
  providerLabel?: string;
  model: string;
  /** 模型展示名（缺省 = model id） */
  name?: string;
}

const providerLabelOf = (choice: RouteModelChoice): string => choice.providerLabel || choice.provider;
const choiceLabelOf = (choice: RouteModelChoice): string => choice.name || choice.model;

export function RoutingModelPicker({ choices, selected, onChange, disabled = false, limit = ROUTING_MEMBER_LIMIT, lastUsed }: {
  choices: RouteModelChoice[];
  /** 已选项的 `routeModelKey` 列表 */
  selected: string[];
  onChange(selected: string[]): void;
  disabled?: boolean;
  limit?: number;
  lastUsed?: Record<string, number>;
}): JSX.Element {
  const { t } = useTranslation();
  const [provider, setProvider] = useState('');
  const [providerSearch, setProviderSearch] = useState('');
  const [modelSearch, setModelSearch] = useState('');
  /** 打开期间固定排序：后台刷新不能挪走用户正要点击的那一行 */
  const [usageSnapshot] = useState(() => lastUsed);
  const orderedChoices = useMemo(
    () => sortRoutingModels(choices, (choice) => choice.model, usageSnapshot),
    [choices, usageSnapshot],
  );
  const picked = useMemo(() => new Set(selected), [selected]);

  const providerIds = [...new Set(choices.map((choice) => choice.provider))];
  const allProviders = providerIds
    .map((id) => ({
      id,
      label: providerLabelOf(choices.find((choice) => choice.provider === id) as RouteModelChoice),
      count: choices.filter((choice) => choice.provider === id).length,
    }))
    .sort((left, right) => left.label.localeCompare(right.label) || left.id.localeCompare(right.id));
  const providerQuery = providerSearch.trim().toLowerCase();
  const providers = allProviders.filter((item) => `${item.label} ${item.id}`.toLowerCase().includes(providerQuery));
  const current = allProviders.find((item) => item.id === provider);
  const visible = new Set(providers.map((item) => item.id));
  const query = modelSearch.trim().toLowerCase();
  const models = orderedChoices.filter(
    (choice) =>
      (current ? choice.provider === current.id : visible.has(choice.provider))
      && (!query
        || choice.model.toLowerCase().includes(query)
        || choiceLabelOf(choice).toLowerCase().includes(query)),
  );
  const chooseProvider = (id: string): void => {
    setProvider(id);
    setModelSearch('');
  };
  const toggle = (choice: RouteModelChoice, on: boolean): void => {
    const key = routeModelKey(choice);
    if (disabled || (on && picked.size >= limit)) return;
    onChange(on ? [...new Set([...selected, key])] : selected.filter((value) => value !== key));
  };
  return (
    <div className="rt-model-picker">
      <div className="rt-model-columns">
        <aside className="rt-model-providers">
          <input
            className="form-input"
            value={providerSearch}
            disabled={disabled}
            placeholder={t('搜索提供商')}
            aria-label={t('搜索提供商')}
            onChange={(event) => {
              setProviderSearch(event.currentTarget.value);
              setProvider('');
            }}
          />
          <div className="rt-model-provider-list" aria-label={t('选择提供商')}>
            <button
              type="button"
              className={`rt-model-provider rt-model-provider-all${current ? '' : ' is-on'}`}
              disabled={disabled}
              title={t('显示全部')}
              onClick={() => {
                setProviderSearch('');
                chooseProvider('');
              }}
            >
              <span className="rt-model-provider-name">{t('全部')}</span>
              <span className="rt-count">{choices.length}</span>
            </button>
            {providers.map((item) => (
              <button
                key={item.id}
                type="button"
                className={`rt-model-provider${item.id === current?.id ? ' is-on' : ''}`}
                disabled={disabled}
                title={`${item.label} · ${t('{{n}} 个模型', { n: item.count })}`}
                onClick={() => chooseProvider(item.id)}
              >
                <RoutingProviderMark label={item.label} />
                <span className="rt-model-provider-name">{item.label}</span>
                <span className="rt-count">{item.count}</span>
              </button>
            ))}
            {!providers.length ? <p className="rt-picker-empty">{t('没有匹配的提供商或账号')}</p> : null}
          </div>
        </aside>
        <section className="rt-model-choices">
          <div className="rt-model-choices-head">
            <b>{current?.label || t('全部')}</b>
            <span className="rt-count">{t('{{n}} 个模型', { n: models.length })}</span>
          </div>
          <input
            className="form-input"
            value={modelSearch}
            disabled={disabled}
            placeholder={t('搜索模型')}
            aria-label={t('搜索模型')}
            onChange={(event) => setModelSearch(event.currentTarget.value)}
          />
          <div className="rt-model-choice-list">
            {models.map((choice) => {
              const key = routeModelKey(choice);
              const checked = picked.has(key);
              return (
                <label
                  key={`${choice.provider}\u0000${choice.model}`}
                  className="rt-picker-row"
                  data-checked={checked ? '' : undefined}
                  title={`${providerLabelOf(choice)} · ${choice.model}`}
                >
                  <input
                    type="checkbox"
                    checked={checked}
                    disabled={disabled || (!checked && picked.size >= limit)}
                    aria-label={`${providerLabelOf(choice)} · ${choice.model}`}
                    onChange={(event) => toggle(choice, event.currentTarget.checked)}
                  />
                  <span className="rt-picker-model-id">{providerLabelOf(choice)} · {choice.model}</span>
                </label>
              );
            })}
            {!models.length ? (
              <p className="rt-picker-empty">{query ? t('没有匹配的模型') : t('没有可添加的模型')}</p>
            ) : null}
          </div>
        </section>
      </div>
      <div className="rt-model-selection-summary">
        <span>{t('已选择 {{count}} / {{total}} 个', { count: picked.size, total: choices.length })}</span>
        <button
          type="button"
          className="rt-link-btn"
          disabled={disabled || !picked.size}
          onClick={() => onChange([])}
        >
          {t('清空选择')}
        </button>
      </div>
      {picked.size >= limit ? (
        <p className="rt-config-note">{t('一个路由组最多允许 {{n}} 个成员', { n: ROUTING_MEMBER_LIMIT })}</p>
      ) : null}
      {!choices.length ? (
        <p className="rt-config-note rt-picker-note">
          <Search size={12} aria-hidden="true" />
          {t('模型目录未接入：当前没有可添加的候选，请先在模型管理里绑定提供商与模型。')}
        </p>
      ) : null}
    </div>
  );
}