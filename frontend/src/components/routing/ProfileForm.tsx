/**
 * ProfileForm —— 单层路由档案的字段编辑器（#40 契约 §1/§2.1）。
 *
 * 字段面按 Agent2API v3.5.1 `routing-config-panel.tsx` 的**顺序与分组**对齐：
 *
 *   路由策略 · 会话粘滞（双列）
 *   → 参与名单（参与方式 → 提供商卡片列（搜索 / 参与 / 排除 / 上下移）→ 账号 id 名单）
 *   → 能力需求
 *   → 排序规则
 *   → 默认路由组
 *   → [可选] 模型筛选（A2A 只影响展示，不改配置）
 *   → [可选] 全局保护设置摘要（A2A routing-protection-summary）
 *
 * 六个字段全部经 [`TriStateField`] 呈现三态（枚举类两态）。每个字段的
 * 「清空」含义各不相同，故清空说明逐字段给死 —— 用户不该去猜。三态是
 * **AIGX 独有项**（A2A 没有档案分层），不对齐、不削弱。
 *
 * ⚠ 本组件只负责「本层怎么写」，**不负责算生效结果**：合并后的生效值与
 * 逐字段来源一律以后端 `resolve_profile` / `plan_route` 为唯一口径
 * （本仓 #31 的教训：界面不许自己再算一遍）。这里的 `inherited` 仅用于
 * 「不表态时会得到什么」的输入提示。
 */
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Plus } from 'lucide-react';
import TriStateField from './TriStateField';
import { RoutingProviderList } from './routing-provider-card';
import { RoutingModelFilter } from './routing-model-filter';
import {
  EMPTY_NEEDS,
  EMPTY_SCOPE,
  ROUTING_CAPS,
  fieldState,
  itemsToLines,
  linesToItems,
  setFieldState,
  type AccountSortRule,
  type Layer,
  type ProfileField,
  type ProfileProvenance,
  type RequestNeeds,
  type RoutingProfile,
  type RoutingScope,
  type SortDir,
  type SortOn,
} from '../../types/routing-profile';
import type { RouteStrategy, StickyMode } from '../../types';

const STRATEGY_LABELS: Record<RouteStrategy, string> = {
  weighted: '加权随机',
  round_robin: '组内轮询',
  ranked: '多因子调度',
};

const STICKY_LABELS: Record<StickyMode, string> = {
  auto: '自动',
  session: '会话内',
  turn: '每轮',
  off: '关闭',
};

/** 排序依据 → 中文（与后端 `AccountSortRule` 变体一一对应） */
export const SORT_ON_LABELS: Record<SortOn, string> = {
  balance: '按余额',
  added: '按加入时间',
  expires: '按凭证到期',
  quota_reset: '按配额重置时刻',
};

const SORT_ON_ORDER: SortOn[] = ['balance', 'added', 'expires', 'quota_reset'];

function scopeSummary(t: (s: string, o?: Record<string, unknown>) => string, s: RoutingScope | null): string {
  if (!s) return t('（下层未设置）');
  const parts: string[] = [];
  parts.push(
    s.provider_scope === 'listed'
      ? t('仅列出的 {{n}} 个提供商', { n: s.providers.length })
      : t('全部提供商'),
  );
  if (s.disabled_providers.length) parts.push(t('排除 {{n}} 个', { n: s.disabled_providers.length }));
  if (s.account_order.length) parts.push(t('账号优先序 {{n}} 条', { n: s.account_order.length }));
  if (s.excluded_accounts.length) parts.push(t('排除账号 {{n}} 个', { n: s.excluded_accounts.length }));
  return parts.join(' · ');
}

function needsSummary(t: (s: string, o?: Record<string, unknown>) => string, n: RequestNeeds | null): string {
  if (!n) return t('（下层未设置）');
  const on: string[] = [];
  if (n.tools) on.push(t('工具调用'));
  if (n.images) on.push(t('图片理解'));
  if (n.video) on.push(t('视频'));
  return on.length ? t('要求：{{list}}', { list: on.join('、') }) : t('不要求任何能力');
}

/** 一行名单输入（textarea，一行一个）：带条数与上限提示 */
function ItemLines({
  label,
  hint,
  items,
  cap,
  onChange,
}: {
  label: string;
  hint?: string;
  items: string[];
  cap: number;
  onChange: (next: string[]) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const over = items.length > cap;
  return (
    <div className="rt-subfield">
      <label>{label}</label>
      <textarea
        className="form-input rt-lines"
        value={itemsToLines(items)}
        spellCheck={false}
        placeholder={t('一行一个，留空即无')}
        onChange={(e) => onChange(linesToItems(e.target.value))}
      />
      <span className={`rt-count${over ? ' rt-count-over' : ''}`}>
        {t('{{n}} / 上限 {{cap}}', { n: items.length, cap })}
        {over ? t('（超限将被后端拒绝写入）') : ''}
      </span>
      {hint ? <span className="form-hint">{hint}</span> : null}
    </div>
  );
}

/** 提供商卡片区：搜索 + 参与/排除两段 + 「添加提供商」 */
function ProviderScopeCards({
  scope,
  disabled,
  onChange,
}: {
  scope: RoutingScope;
  disabled: boolean;
  onChange(next: Partial<RoutingScope>): void;
}): JSX.Element {
  const { t } = useTranslation();
  const [search, setSearch] = useState('');
  const [input, setInput] = useState('');
  const [opened, setOpened] = useState<Record<string, boolean>>({});
  const included = scope.providers;
  const excluded = scope.disabled_providers;
  const join = (id: string, on: boolean): void => {
    if (on) {
      onChange({
        providers: included.includes(id) ? included : [...included, id],
        disabled_providers: excluded.filter((value) => value !== id),
      });
      return;
    }
    onChange({
      providers: included.filter((value) => value !== id),
      disabled_providers: excluded.includes(id) ? excluded : [...excluded, id],
    });
  };
  const move = (id: string, direction: number): void => {
    const next = [...included];
    const index = next.indexOf(id);
    const to = index + direction;
    if (index < 0 || to < 0 || to >= next.length) return;
    next[index] = next[to];
    next[to] = id;
    onChange({ providers: next });
  };
  const add = (): void => {
    const id = input.trim();
    if (!id) return;
    setInput('');
    onChange({
      providers: included.includes(id) ? included : [...included, id],
      disabled_providers: excluded.filter((value) => value !== id),
    });
  };
  return (
    <div className="rt-provider-scope">
      <div className="rt-subfield">
        <input
          className="form-input"
          value={search}
          placeholder={t('搜索提供商')}
          aria-label={t('搜索提供商')}
          onChange={(event) => setSearch(event.currentTarget.value)}
        />
      </div>
      <RoutingProviderList
        included={included}
        excluded={excluded}
        locked={disabled}
        query={search.trim().toLowerCase()}
        opened={opened}
        onOpen={(id, open) => setOpened((current) => ({ ...current, [id]: open }))}
        onJoin={join}
        onMove={move}
      />
      <div className="rt-account-add">
        <input
          className="form-input"
          value={input}
          placeholder={t('提供商 id，如 openai')}
          aria-label={t('添加提供商')}
          disabled={disabled}
          onChange={(event) => setInput(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter') {
              event.preventDefault();
              add();
            }
          }}
        />
        <button
          type="button"
          className="btn btn-secondary btn-sm"
          disabled={disabled || !input.trim() || included.length >= ROUTING_CAPS.MAX_PROVIDERS}
          onClick={add}
        >
          <Plus size={13} /> {t('添加提供商')}
        </button>
      </div>
    </div>
  );
}

export interface ProfileFormProps {
  value: RoutingProfile;
  onChange: (next: RoutingProfile) => void;
  /** 下层的合并结果（仅用于「继承会得到什么」的提示，非判定口径） */
  inherited: RoutingProfile;
  /**
   * 「继承」时实际来自谁的白话名：全局层写「内置默认」，模型级写「全局」。
   * 逐层给死而不是让本组件去猜 —— 猜错就是界面撒谎。
   */
  inheritSource: string;
  /** 可选默认组 id（datalist 候选） */
  groupIds: string[];
  /** 该字段的权威来源；`undefined` = 后端未提供（此时不显示徽章，也不猜） */
  provenance?: ProfileProvenance | null;
  /** 模型筛选候选（A2A routing-model-filter；缺省 = 不渲染该块） */
  models?: string[];
  /** 最近使用时间（排序用；缺省 = 字典序） */
  modelLastUsed?: Record<string, number>;
  /** 当前筛选的模型（空串 = 全部）；与 onModelChange 成对出现才渲染 */
  modelFilter?: string;
  onModelChange?(model: string): void;
  /** 全局保护设置摘要（A2A routing-protection-summary）；缺省 = 不渲染 */
  protection?: { labels: string[]; onOpen?(): void };
}

export default function ProfileForm({
  value,
  onChange,
  inherited,
  inheritSource,
  groupIds,
  provenance,
  models,
  modelLastUsed,
  modelFilter,
  onModelChange,
  protection,
}: ProfileFormProps): JSX.Element {
  const { t } = useTranslation();

  const setField = (field: ProfileField, state: ReturnType<typeof fieldState>): void => {
    onChange(setFieldState(value, field, state, inherited));
  };
  const patch = (p: Partial<RoutingProfile>): void => onChange({ ...value, ...p });

  /** 取某字段的权威来源（后端没给就没有徽章，绝不按推算冒充） */
  const prov = (f: ProfileField): Layer | null | undefined =>
    provenance ? provenance[f] : undefined;

  return (
    <div className="rt-profile-form">
      {/* A2A 配置面板：策略与粘滞并排成两列 */}
      <div className="rt-field-grid">
        {/* 1. 路由策略（枚举 → 无「清空」语义） */}
        <TriStateField
          label={t('路由策略')}
          hint={t('同优先级组内怎么挑供应商。')}
          state={fieldState(value, 'strategy')}
          onStateChange={(s) => setField('strategy', s)}
          allowClear={false}
          clearNote=""
          inheritNote={t('沿用{{src}}的策略：当前为「{{v}}」。下层改了这里跟着变。', {
            src: inheritSource,
            v: t(STRATEGY_LABELS[inherited.strategy ?? 'weighted']),
          })}
          provenance={prov('strategy')}
        >
          <select
            className="form-input"
            value={value.strategy ?? inherited.strategy ?? 'weighted'}
            onChange={(e) => patch({ strategy: e.target.value as RouteStrategy })}
          >
            {(Object.keys(STRATEGY_LABELS) as RouteStrategy[]).map((k) => (
              <option key={k} value={k}>
                {t(STRATEGY_LABELS[k])}
              </option>
            ))}
          </select>
        </TriStateField>

        {/* 2. 会话粘滞（枚举 → 无「清空」语义） */}
        <TriStateField
          label={t('会话粘滞')}
          hint={t('同一会话是否跟住同一供应商（复用上游上下文缓存）。')}
          state={fieldState(value, 'sticky')}
          onStateChange={(s) => setField('sticky', s)}
          allowClear={false}
          clearNote=""
          inheritNote={t('沿用{{src}}的粘性档位：当前为「{{v}}」。', {
            src: inheritSource,
            v: t(STICKY_LABELS[inherited.sticky ?? 'auto']),
          })}
          provenance={prov('sticky')}
        >
          <select
            className="form-input"
            value={value.sticky ?? inherited.sticky ?? 'auto'}
            onChange={(e) => patch({ sticky: e.target.value as StickyMode })}
          >
            {(Object.keys(STICKY_LABELS) as StickyMode[]).map((k) => (
              <option key={k} value={k}>
                {t(STICKY_LABELS[k])}
              </option>
            ))}
          </select>
        </TriStateField>
      </div>

      {/* 3. 参与名单（A2A 参与方式 + 提供商卡片 + 账号 id 名单） */}
      <TriStateField
        label={t('参与名单')}
        hint={t('限定这个模型能走哪些提供商与账号；名单外的供应商在选路前就被裁掉。')}
        state={fieldState(value, 'scope')}
        onStateChange={(s) => setField('scope', s)}
        clearNote={t('不限制提供商与账号（空名单 = 谁都能参与）。')}
        inheritNote={t('沿用{{src}}的名单：{{v}}', {
          src: inheritSource,
          v: scopeSummary(t, inherited.scope),
        })}
        provenance={prov('scope')}
      >
        {(() => {
          const s = value.scope ?? EMPTY_SCOPE;
          const setScope = (next: Partial<RoutingScope>): void => patch({ scope: { ...s, ...next } });
          const noProvider = s.provider_scope === 'listed' && s.providers.length === 0;
          return (
            <>
              <div className="rt-subfield" style={{ marginBottom: 10 }}>
                <label>{t('参与方式')}</label>
                <select
                  className="form-input"
                  value={s.provider_scope}
                  onChange={(e) => setScope({ provider_scope: e.target.value as RoutingScope['provider_scope'] })}
                >
                  <option value="all">{t('全部提供商（自动并入新出现的）')}</option>
                  <option value="listed">{t('仅加入的参与')}</option>
                </select>
                <span className="form-hint">
                  {s.provider_scope === 'all'
                    ? t('新增提供商自动参与；移出本路由的不会自动加回。')
                    : t('只有加入的提供商参与，新增提供商需要手动加入。')}
                </span>
              </div>
              {noProvider ? (
                <div className="rt-notice rt-notice-warn" style={{ marginBottom: 10 }}>
                  {t('已选「仅列出的提供商」但列表为空 —— 该模型将没有任何候选供应商。槽位名请填供应商的「提供商」值（如 openai / anthropic）。')}
                </div>
              ) : null}
              <ProviderScopeCards scope={s} disabled={false} onChange={setScope} />
              <div className="rt-grid2" style={{ marginTop: 10 }}>
                <ItemLines
                  label={t('账号优先顺序')}
                  hint={t('按行序先试靠前的账号；不在名单里的账号排在后面。')}
                  items={s.account_order}
                  cap={ROUTING_CAPS.MAX_ACCOUNTS}
                  onChange={(v) => setScope({ account_order: v })}
                />
                <ItemLines
                  label={t('排除的账号')}
                  hint={t('这些账号在本层被禁用（不参与选路）。')}
                  items={s.excluded_accounts}
                  cap={ROUTING_CAPS.MAX_ACCOUNTS}
                  onChange={(v) => setScope({ excluded_accounts: v })}
                />
              </div>
            </>
          );
        })()}
      </TriStateField>

      {/* 4. 能力需求 */}
      <TriStateField
        label={t('能力需求')}
        hint={t('本层要求请求具备哪些能力，不具备的账号在该层被过滤掉。')}
        state={fieldState(value, 'needs')}
        onStateChange={(s) => setField('needs', s)}
        clearNote={t('不要求任何能力（等价于三个能力全不要求）。')}
        inheritNote={t('沿用{{src}}的需求：{{v}}', {
          src: inheritSource,
          v: needsSummary(t, inherited.needs),
        })}
        provenance={prov('needs')}
      >
        {(() => {
          const n = value.needs ?? EMPTY_NEEDS;
          return (
            <div style={{ display: 'flex', gap: 18, flexWrap: 'wrap' }}>
              {(
                [
                  ['tools', '工具调用'],
                  ['images', '图片理解'],
                  ['video', '视频'],
                ] as Array<[keyof RequestNeeds, string]>
              ).map(([k, label]) => (
                <label key={k} style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 12.5 }}>
                  <input
                    type="checkbox"
                    checked={n[k]}
                    onChange={(e) => patch({ needs: { ...n, [k]: e.target.checked } })}
                  />
                  <span>{t(label)}</span>
                </label>
              ))}
            </div>
          );
        })()}
      </TriStateField>

      {/* 5. 排序规则 */}
      <TriStateField
        label={t('账号排序规则')}
        hint={t('候选账号按什么顺序试（最多 2 条，后面的作为次级排序）。')}
        state={fieldState(value, 'sort_rules')}
        onStateChange={(s) => setField('sort_rules', s)}
        clearNote={t('不排序：用系统默认顺序。')}
        inheritNote={t('沿用{{src}}的排序：{{v}}', {
          src: inheritSource,
          v: inherited.sort_rules?.length
            ? inherited.sort_rules
              .map((r) => `${t(SORT_ON_LABELS[r.on])} ${r.dir === 'asc' ? t('升序') : t('降序')}`)
              .join(' → ')
            : t('（下层未设置）'),
        })}
        provenance={prov('sort_rules')}
      >
        {(() => {
          const rules = value.sort_rules ?? [];
          const setRule = (i: number, next: Partial<AccountSortRule>): void => {
            const copy = rules.map((r, idx) => (idx === i ? { ...r, ...next } : r));
            patch({ sort_rules: copy });
          };
          return (
            <>
              {rules.map((r, i) => (
                <div key={i} className="rt-member-row" style={{ marginBottom: 8 }}>
                  <select
                    className="form-input"
                    value={r.on}
                    onChange={(e) => setRule(i, { on: e.target.value as SortOn })}
                  >
                    {SORT_ON_ORDER.map((k) => (
                      <option key={k} value={k}>
                        {t(SORT_ON_LABELS[k])}
                      </option>
                    ))}
                  </select>
                  <select
                    className="form-input"
                    value={r.dir}
                    onChange={(e) => setRule(i, { dir: e.target.value as SortDir })}
                  >
                    <option value="asc">{t('升序')}</option>
                    <option value="desc">{t('降序')}</option>
                  </select>
                  <span aria-hidden="true" />
                  <span aria-hidden="true" />
                  <button
                    type="button"
                    className="btn btn-secondary btn-sm"
                    onClick={() => patch({ sort_rules: rules.filter((_, idx) => idx !== i) })}
                  >
                    {t('删除')}
                  </button>
                </div>
              ))}
              {rules.length < ROUTING_CAPS.MAX_SORT_RULES ? (
                <button
                  type="button"
                  className="btn btn-secondary btn-sm"
                  onClick={() => patch({ sort_rules: [...rules, { on: 'added', dir: 'asc' }] })}
                >
                  {t('添加排序规则')}
                </button>
              ) : (
                <div className="form-hint">
                  {t('已达上限 {{cap}} 条（后端超限会拒绝写入）。', { cap: ROUTING_CAPS.MAX_SORT_RULES })}
                </div>
              )}
              <div className="form-hint">
                {t('「按余额」基于账号最近一次余额快照；不同提供商的余额形态不一致，无法归一读数的账号不参与该排序（其余规则照常生效）。')}
              </div>
            </>
          );
        })()}
      </TriStateField>

      {/* 6. 默认路由组 */}
      <TriStateField
        label={t('默认路由组')}
        hint={t('本层为该模型指定默认路由组；请求与 API Key 都没指定组时用它。')}
        state={fieldState(value, 'default_group')}
        onStateChange={(s) => setField('default_group', s)}
        clearNote={t('不指定默认组：直接按上面的档案选路，不叠加组规则。')}
        inheritNote={t('沿用{{src}}的默认组：{{v}}', {
          src: inheritSource,
          v: inherited.default_group || t('（下层的默认组）'),
        })}
        provenance={prov('default_group')}
      >
        <input
          className="form-input"
          list="rt-group-ids"
          placeholder={t('路由组 id，如 gpt4o-fast')}
          value={value.default_group ?? ''}
          onChange={(e) => patch({ default_group: e.target.value })}
        />
        <datalist id="rt-group-ids">
          {groupIds.map((g) => (
            <option key={g} value={g} />
          ))}
        </datalist>
        <div className="form-hint">
          {t('必须是已存在的路由组 id（见下方「路由组」）；写错 = 找不到组，只会走 global。')}
        </div>
      </TriStateField>

      {/* A2A 配置面板：模型筛选（只影响展示） */}
      {models && onModelChange ? (
        <div className="rt-config-filter">
          <RoutingModelFilter
            value={modelFilter ?? ''}
            models={models}
            lastUsed={modelLastUsed}
            onChange={onModelChange}
          />
        </div>
      ) : null}

      {/* A2A 配置面板：全局保护设置摘要 */}
      {protection ? (
        <div className="rt-protection-summary">
          <span>{t('全局保护设置')}</span>
          <span className="rt-protection-badge">
            {protection.labels.length ? protection.labels.join(' · ') : t('关闭')}
          </span>
          {protection.onOpen ? (
            <button type="button" className="rt-link-btn" onClick={protection.onOpen}>
              {t('设置')}
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}