/**
 * A2A（Agent2API v3.5.1）Agent 页模型列表区。
 *
 * 对齐 `agent-model-fields.tsx`（ModelsFields 的 stack / split 两种排布）、
 * `agent-model-picker.tsx`（AgentModelPicker 候选多选）、
 * `agent-model-config.ts`（modelFields / modelLevels / modelFieldNote）。
 *
 * AIGX 后端缺「网关模型目录」数据源：目录恒为空，页面按 A2A 的空态渲染
 * （没有可添加的模型），目录就绪后由 `catalog` 参数接入。
 */

import { useState } from 'react';
import { ChevronDown } from 'lucide-react';
import { Button } from '../components/ui';
import { A2A_TEXT } from './agent-a2a-types';
import type { A2aClient, A2aModel, A2aProfile } from './agent-a2a-types';

export const A2A_MODEL_LIMIT = 200;

/** 目录模型（A2A CatalogModel） */
export type A2aCatalogModel = A2aModel & { label: string };

/** 单个客户端可写的模型字段（对齐 agent-model-config.ts 的 modelFields） */
export function modelFields(client: A2aClient): Array<keyof A2aModel> {
  if (client.modelFields) return client.modelFields;
  if (client.id === 'claude-code') return [];
  return ['contextWindow', 'maxOutputTokens', 'supportsTools', 'supportsImages', 'supportsReasoning'];
}

/** 支持的思考档位（对齐 agent-model-config.ts 的 modelLevels 的通用分支） */
export function modelLevels(client: A2aClient, levels: string[]): string[] {
  const supported: Record<string, string[]> = {
    'claude-code': ['low', 'medium', 'high', 'xhigh'],
  };
  const allowed = supported[client.id];
  return [...new Set(levels.filter((level) => !allowed || allowed.includes(level)))];
}

export function capLabel(key: string): string {
  return {
    supportsTools: A2A_TEXT.capabilityTools,
    supportsImages: A2A_TEXT.capabilityImages,
    supportsVideo: A2A_TEXT.capabilityVideo,
    supportsReasoning: A2A_TEXT.capabilityReasoning,
  }[key] || key;
}

/** 一个模型的能力摘要（对齐 A2A 的 Ability 三种符号） */
function Ability({ label, value, tone }: { label: string; value: string; tone: 'ok' | 'value' | 'no' | 'none' }): JSX.Element {
  return (
    <span className={`a2a-ability a2a-ability-${tone}`}>
      <span className="a2a-ability-label">{label} </span>
      <span className="a2a-ability-value">{value}</span>
    </span>
  );
}

function boolAbility(label: string, value?: boolean | null): JSX.Element {
  if (value === true) return <Ability label={label} value="✓" tone="ok" />;
  if (value === false) return <Ability label={label} value="✕" tone="no" />;
  return <Ability label={label} value="—" tone="none" />;
}

function numberAbility(label: string, value?: number | null): JSX.Element {
  if (value == null) return <Ability label={label} value="—" tone="none" />;
  return <Ability label={label} value={value.toLocaleString()} tone="value" />;
}

/** 候选模型多选（对齐 AgentModelPicker：一行一项，列表自滚动） */
export function A2aModelPicker({
  catalog,
  selected,
  onChange,
  disabled,
  limit = A2A_MODEL_LIMIT,
}: {
  catalog: A2aCatalogModel[];
  selected: string[];
  onChange: (ids: string[]) => void;
  disabled?: boolean;
  limit?: number;
}): JSX.Element {
  const [search, setSearch] = useState('');
  const picked = new Set(selected);
  const query = search.trim().toLowerCase();
  const matches = catalog.filter((model) => `${model.id} ${model.name}`.toLowerCase().includes(query));
  const remaining = Math.max(0, limit - picked.size);
  const available = matches.filter((model) => !picked.has(model.id));

  return (
    <div className="a2a-model-picker" role="group" aria-label={A2A_TEXT.selectModel}>
      <div className="a2a-model-picker-bar">
        <input
          type="search"
          className="form-input a2a-model-picker-search"
          autoComplete="off"
          value={search}
          disabled={disabled}
          aria-label="搜索模型"
          placeholder="搜索模型 ID / 展示名…"
          onChange={(event) => setSearch(event.currentTarget.value)}
        />
        <span className="a2a-model-picker-count">
          {A2A_TEXT.pickedCount.replace('{count}', String(picked.size)).replace('{total}', String(catalog.length))}
        </span>
        <Button variant="secondary" size="sm" disabled={disabled || !remaining || !available.length}
          onClick={() => onChange([...selected, ...available.slice(0, remaining).map((model) => model.id)])}>
          {A2A_TEXT.allSearch}
        </Button>
        <Button variant="secondary" size="sm" disabled={disabled || !selected.length}
          onClick={() => onChange([])}>
          {A2A_TEXT.clearPick}
        </Button>
      </div>
      <div className="a2a-model-picker-list">
        {matches.map((model) => {
          const checked = picked.has(model.id);
          return (
            <label key={model.id} className={`a2a-model-picker-row${checked ? ' is-checked' : ''}`}
              title={`${model.name || model.id}${model.name && model.name !== model.id ? ` · ${model.id}` : ''}`}>
              <input
                type="checkbox"
                checked={checked}
                disabled={disabled || (!checked && !remaining)}
                aria-label={model.label}
                onChange={(event) => onChange(event.currentTarget.checked
                  ? [...selected, model.id]
                  : selected.filter((id) => id !== model.id))}
              />
              <span className="a2a-model-picker-name">{model.name || model.id}</span>
              {model.name && model.name !== model.id && <code className="a2a-model-picker-id">{model.id}</code>}
            </label>
          );
        })}
        {!matches.length && (
          <div className="a2a-model-picker-empty">{query ? A2A_TEXT.noMatchModels : A2A_TEXT.noModels}</div>
        )}
      </div>
      {!remaining && <p className="a2a-hint">{A2A_TEXT.maxModels.replace('{n}', String(A2A_MODEL_LIMIT))}</p>}
    </div>
  );
}

/** 已选模型卡片（展开可调整能力，对齐 A2A 的 agent-model-card） */
function A2aModelCard({
  client,
  model,
  index,
  open,
  onToggle,
  onRemove,
  disabled,
}: {
  client: A2aClient;
  model: A2aModel;
  index: number;
  open: boolean;
  onToggle: () => void;
  onRemove: () => void;
  disabled: boolean;
}): JSX.Element {
  const name = model.name || model.id || `模型 ${index + 1}`;
  const label = model.name || model.id || String(index + 1);
  return (
    <div className={`a2a-model-card${open ? ' is-open' : ''}`}>
      <div className="a2a-model-card-head">
        <button type="button" className="a2a-model-card-trigger" onClick={onToggle} aria-expanded={open}>
          <span className="a2a-model-card-main">
            <span className="a2a-model-card-title">
              <span className="a2a-model-card-name">{name}</span>
              {model.id && model.name && model.name !== model.id && (
                <code className="a2a-model-card-id">{model.id}</code>
              )}
            </span>
            <span className="a2a-model-card-abilities">
              {numberAbility(A2A_TEXT.contextWindow, model.contextWindow)}
              {numberAbility(A2A_TEXT.outputLabel, model.maxOutputTokens)}
              {['supportsTools', 'supportsImages', 'supportsReasoning'].map((key) =>
                boolAbility(capLabel(key), model[key as keyof A2aModel] as boolean | null | undefined),
              )}
            </span>
          </span>
          <ChevronDown size={14} className={`a2a-model-card-chevron${open ? ' is-open' : ''}`} />
        </button>
        <Button variant="secondary" size="sm" className="a2a-danger"
          title={A2A_TEXT.removeModel.replace('{name}', label)}
          aria-label={A2A_TEXT.removeModel.replace('{name}', label)}
          disabled={disabled} onClick={onRemove}>
          {A2A_TEXT.remove}
        </Button>
      </div>
      {open && (
        <div className="a2a-model-card-body">
          <p className="a2a-hint">这里只调整此模型，不影响其它模型。</p>
          <div className="a2a-grid-2">
            <label className="a2a-field">
              <span className="a2a-label">{A2A_TEXT.modelId}<code className="a2a-field-key">id</code></span>
              <input className="form-input" value={model.id} disabled={disabled} readOnly />
            </label>
            <label className="a2a-field">
              <span className="a2a-label">{A2A_TEXT.modelName}<code className="a2a-field-key">name</code></span>
              <input className="form-input" value={model.name} disabled={disabled} readOnly />
            </label>
            <label className="a2a-field">
              <span className="a2a-label">{A2A_TEXT.contextWindow}<code className="a2a-field-key">contextWindow</code></span>
              <input className="form-input" type="number" value={model.contextWindow ?? ''} disabled={disabled} readOnly />
            </label>
            <label className="a2a-field">
              <span className="a2a-label">{A2A_TEXT.maxOutput}<code className="a2a-field-key">maxOutputTokens</code></span>
              <input className="form-input" type="number" value={model.maxOutputTokens ?? ''} disabled={disabled} readOnly />
            </label>
          </div>
          <p className="a2a-hint">
            {`每个模型独立保存能力；展开可调整，未声明的数据不会猜测填写。（客户端 ${client.name}）`}
          </p>
        </div>
      )}
    </div>
  );
}

/** 模型列表区（对齐 ModelsFields：stack = 编辑弹窗，split = 一键接入） */
export function A2aModelsFields({
  client,
  draft,
  update,
  catalog,
  disabled,
  quick = false,
  layout = 'stack',
}: {
  client: A2aClient;
  draft: A2aProfile;
  update: (patch: Partial<A2aProfile>) => void;
  catalog: A2aCatalogModel[];
  disabled: boolean;
  quick?: boolean;
  layout?: 'stack' | 'split';
}): JSX.Element {
  const [expanded, setExpanded] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const isGateway = draft.kind === 'gateway';
  const split = layout === 'split';
  const keyword = query.trim().toLowerCase();
  const rows = draft.models
    .map((model, index) => ({ model, index }))
    .filter(({ model }) => !keyword || `${model.id} ${model.name}`.toLowerCase().includes(keyword));
  const readyCount = draft.models.filter((model) => model.id).length;

  const replaceModels = (models: A2aModel[]): void => {
    setExpanded(null);
    const picked = models.find((model) => model.id === draft.model)?.id || models[0]?.id || '';
    update({
      models,
      model: picked,
      effort: picked === draft.model ? draft.effort : '',
    });
  };

  const addModel = (): void => {
    if (disabled || draft.models.length >= A2A_MODEL_LIMIT) return;
    const blank: A2aModel = { id: '', name: '', reasoningLevels: [], effort: '' };
    update({ models: [...draft.models, blank] });
  };

  const removeModel = (index: number): void => {
    replaceModels(draft.models.filter((_, i) => i !== index));
  };

  const selectIds = (ids: string[]): void => {
    const existing = new Map(draft.models.map((model) => [model.id, model]));
    const byId = new Map(catalog.map((model) => [model.id, model]));
    const models = ids.flatMap((id) => {
      const previous = existing.get(id);
      if (previous) return [previous];
      const picked = byId.get(id);
      if (!picked) return [];
      const { label: _label, ...configured } = picked;
      return [{ ...configured, effort: '' }];
    });
    replaceModels([...models, ...draft.models.filter((model) => !model.id)]);
  };

  const selectedCatalog: A2aCatalogModel[] = [
    ...catalog,
    ...draft.models
      .filter((model) => model.id && !catalog.some((item) => item.id === model.id))
      .map((model) => ({ ...model, label: model.name || model.id })),
  ];

  const picker = (
    <A2aModelPicker
      catalog={selectedCatalog}
      selected={draft.models.map((model) => model.id).filter(Boolean)}
      onChange={selectIds}
      disabled={disabled}
      limit={A2A_MODEL_LIMIT - draft.models.filter((model) => !model.id).length}
    />
  );

  const queryField = draft.models.length > 3 || query !== '' ? (
    <input
      type="search"
      className="form-input"
      aria-label="搜索已选模型"
      placeholder="搜索已选模型"
      value={query}
      autoComplete="off"
      disabled={disabled}
      onChange={(event) => {
        setQuery(event.currentTarget.value);
        setExpanded(null);
      }}
    />
  ) : null;

  const list = (
    <div className="a2a-model-list">
      {rows.map(({ model, index }) => (
        <A2aModelCard
          key={`${model.id || 'blank'}:${index}`}
          client={client}
          model={model}
          index={index}
          open={expanded === `${model.id}:${index}`}
          onToggle={() => setExpanded(expanded === `${model.id}:${index}` ? null : `${model.id}:${index}`)}
          onRemove={() => removeModel(index)}
          disabled={disabled}
        />
      ))}
      {!rows.length && (
        <div className="a2a-model-list-empty">
          <span>{keyword ? A2A_TEXT.noMatchModels : A2A_TEXT.atLeastOneModel}</span>
          {keyword && (
            <Button variant="secondary" size="sm" onClick={() => setQuery('')}>{A2A_TEXT.clearSearch}</Button>
          )}
        </div>
      )}
    </div>
  );

  return (
    <section className="a2a-models-fields">
      <div className="a2a-models-head">
        <h3 className="a2a-section-title">{A2A_TEXT.modelList}</h3>
        <span className="a2a-model-count">{readyCount}</span>
        <span className="a2a-models-spacer" />
        <Button variant={isGateway ? 'secondary' : 'outline'} size="sm" disabled={disabled || draft.models.length >= A2A_MODEL_LIMIT}
          onClick={addModel}>
          {isGateway ? A2A_TEXT.manualAddModel : A2A_TEXT.addModel}
        </Button>
      </div>

      {split ? (
        <>
          {isGateway && (
            <p className="a2a-hint">可搜索并多选模型；上下文、输出上限和能力分别从网关目录自动填入。</p>
          )}
          <div className="a2a-models-split">
            <div className="a2a-models-col">
              <div className="a2a-models-col-head">
                <h4>可选模型</h4>
                <span className="a2a-models-col-note">{`网关目录共 ${selectedCatalog.length} 个`}</span>
              </div>
              {isGateway ? picker : (
                <div className="a2a-model-picker-empty">这个客户端不从网关目录选模型，用右侧的「添加模型」逐个填写。</div>
              )}
            </div>
            <div className="a2a-models-col">
              <div className="a2a-models-col-head">
                <h4>已选模型</h4>
                <span className="a2a-model-count">{readyCount}</span>
                <span className="a2a-models-col-note">展开可调整能力；— 表示客户端未声明</span>
              </div>
              {queryField}
              {list}
            </div>
          </div>
        </>
      ) : (
        <>
          {isGateway && (
            <p className="a2a-hint">可搜索并多选模型；上下文、输出上限和能力分别从网关目录自动填入。</p>
          )}
          {isGateway && picker}
          <p className="a2a-hint">每个模型独立保存能力；展开可调整，未声明的数据不会猜测填写。</p>
          {queryField}
          {list}
        </>
      )}

      {client.supportsDefault && (
        <label className="a2a-field">
          <span className="a2a-label">{A2A_TEXT.defaultModel}</span>
          <select
            className="form-input"
            value={draft.model}
            disabled={disabled}
            onChange={(event) => update({ model: event.currentTarget.value, effort: '' })}
          >
            {draft.models.filter((model) => model.id).length === 0 && (
              <option value="">{A2A_TEXT.atLeastOneModel}</option>
            )}
            {draft.models.filter((model) => model.id).map((model) => (
              <option key={model.id} value={model.id}>{model.name || model.id}</option>
            ))}
          </select>
          <p className="a2a-hint">
            {quick
              ? '接入后使用此默认模型，其它已选模型仍可在客户端切换。'
              : '使用此配置时设为默认，其他模型仍可在客户端选择。'}
          </p>
        </label>
      )}
    </section>
  );
}
