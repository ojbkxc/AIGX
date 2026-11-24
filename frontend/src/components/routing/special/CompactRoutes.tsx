/**
 * CompactRoutes —— 压缩上下文路由面板（对照 A2A routing-special-panels 的
 * CompactSpecialPanel + AIGX `route_special::compact`）。
 *
 * 形状：一份规则列表 —— 「[客户端] 的压缩请求：原模型 → 压缩模型」。
 * 命中规则的压缩请求会改用「压缩模型」重发一次，改写之后按新模型自己的
 * 路由挑账号（本面板只改写模型，不改选账号规则）。
 *
 * 后端契约（canonical 对齐 Agent2API v3.5.0-beta.1）：
 *   GET  /api/models/compact-routes     → { config, revision, clients[], models[], maxRules }
 *   PUT  /api/models/compact-routes     ← { config: { enabled, rules[] }, revision } → 同 GET
 *   冲突 409；模型不可用 400（保留「上次已保存但当前不可用」的旧规则）。
 * 不做路径回退：端点未注册 → 直接报错可见（见 api/index.ts）。
 */
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../../api';
import {
  modelChoices,
  normalizeCompactConfig,
  type CompactRoutesConfig,
  type CompactRouteRule,
} from '../../../types/routing-workspace';
import { useSpecialRouteEditor } from './useSpecialRouteEditor';
import RuleDialog from './RuleDialog';
import { AddRuleButton, ModelSelect, RulePanel, RuleRow, RuleSectionHead, RuleSwitchRow } from './shared';

/** 规则行内编辑态：index = -1 表示新增 */
interface Editing {
  rule: CompactRouteRule;
  index: number;
}

export default function CompactRoutes({ active }: { active: boolean }): JSX.Element {
  const { t } = useTranslation();
  const editor = useSpecialRouteEditor<CompactRoutesConfig, import('../../../types/routing-workspace').CompactRoutesView>(
    {
      load: () => api.getCompactRoutes(),
      save: (payload) =>
        api.saveCompactRoutes(payload.config as CompactRoutesConfig, payload.revision),
    },
    normalizeCompactConfig,
    active,
  );
  const { view, draft, saving } = editor;
  const [editing, setEditing] = useState<Editing | null>(null);

  const clients = useMemo(
    () =>
      (Array.isArray(view?.clients) ? view?.clients ?? [] : []).map((c) => ({
        id: String(c.id),
        label: String(c.label ?? c.id),
      })),
    [view],
  );
  const models = useMemo(() => modelChoices(view?.models), [view]);
  const clientLabel = (id: string): string => clients.find((c) => c.id === id)?.label ?? id;
  const maxRules = Number.isFinite(view?.maxRules) ? Number(view?.maxRules) : 0;
  const rules = draft?.rules ?? [];
  const full = maxRules > 0 && rules.length >= maxRules;

  const blankRule = (): CompactRouteRule => ({
    client: clients[0]?.id ?? '',
    source: '',
    target: '',
    enabled: true,
  });

  const write = (index: number, rule: CompactRouteRule): void => {
    editor.update((current) => ({
      ...current,
      rules:
        index < 0
          ? [...current.rules, rule]
          : current.rules.map((item, at) => (at === index ? rule : item)),
    }));
  };

  const remove = (index: number): void => {
    editor.update((current) => ({
      ...current,
      rules: current.rules.filter((_, at) => at !== index),
    }));
    setEditing(null);
  };

  return (
    <RulePanel
      title={t('压缩上下文')}
      lede={t('命中下面规则的上下文压缩请求，会改用指定的模型压缩一次；账号仍按新模型自己的路由挑。')}
      editor={editor}
      serverError={view?.error ?? null}
    >
      <RuleSectionHead
        title={t('规则')}
        count={t('{{n}} 条', { n: rules.length })}
        actions={
          <AddRuleButton
            disabled={saving || full || !models.length}
            onClick={() => setEditing({ rule: blankRule(), index: -1 })}
          />
        }
      />
      {!rules.length ? (
        <div className="rt-state-note rt-state-inherit">
          {t('还没有规则。压缩请求会按原模型照常转发。')}
        </div>
      ) : null}

      {rules.map((rule, index) => (
        <RuleRow
          key={`row-${index}`}
          off={!rule.enabled}
          title={t('{{client}} 的压缩请求', { client: clientLabel(rule.client) })}
          detail={
            <>
              <code>{rule.source || t('还没选原模型')}</code>
              <span aria-hidden="true">→</span>
              <code>{rule.target || t('还没选压缩模型')}</code>
            </>
          }
          meta={rule.enabled ? null : t('已停用')}
          actions={null}
          onOpen={saving ? undefined : () => setEditing({ rule: { ...rule }, index })}
        />
      ))}

      {!models.length ? (
        <div className="rt-notice rt-notice-warn">
          {t('没有可选的对话模型，请先启用模型和对应账号。')}
        </div>
      ) : null}
      {full ? (
        <div className="rt-notice rt-notice-warn">
          {t('规则最多 {{n}} 条。', { n: maxRules })}
        </div>
      ) : null}

      {editing ? (
        <RuleDialog
          title={editing.index < 0 ? t('添加规则') : t('编辑规则')}
          subtitle={t('命中这条规则的压缩请求会改用「压缩模型」重发一次；改写之后按新模型自己的路由挑账号。')}
          saving={saving}
          onClose={() => setEditing(null)}
          onApply={() => {
            write(editing.index, editing.rule);
            setEditing(null);
          }}
          onDelete={editing.index < 0 ? undefined : () => remove(editing.index)}
        >
          <RuleEditor
            value={editing.rule}
            clients={clients}
            models={models}
            saving={saving}
            onChange={(next) => setEditing({ ...editing, rule: next })}
          />
        </RuleDialog>
      ) : null}
    </RulePanel>
  );
}

/** 弹窗内表单：启用开关 + 客户端 + 原模型 + 压缩模型（受控，提交由弹窗底栏负责） */
function RuleEditor({
  value,
  clients,
  models,
  saving,
  onChange,
}: {
  value: CompactRouteRule;
  clients: Array<{ id: string; label: string }>;
  models: Array<{ id: string; label: string }>;
  saving: boolean;
  onChange: (next: CompactRouteRule) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const patch = (part: Partial<CompactRouteRule>): void => onChange({ ...value, ...part });
  // 对齐 A2A `CompactRuleDialog`：原模型 / 压缩模型互相排除（同一只模型压缩自己没有意义，
  // 列出来只会让用户配出一条永远原地转发的规则）。
  const without = (id: string): Array<{ id: string; label: string }> =>
    models.filter((m) => m.id !== id);

  return (
    <div className="rt-dialog-fields">
      <RuleSwitchRow
        enabled={value.enabled}
        disabled={saving}
        onChange={(enabled) => patch({ enabled })}
      />
      <div className="rt-special-editor-fields">
        <div className="rt-subfield">
          <label>{t('触发客户端')}</label>
          <select
            className="form-input"
            aria-label={t('触发客户端')}
            value={value.client}
            disabled={saving}
            onChange={(e) => patch({ client: e.target.value })}
          >
            {clients.map((c) => (
              <option key={c.id} value={c.id}>
                {c.label}
              </option>
            ))}
          </select>
        </div>
        <div className="rt-subfield">
          <label>{t('原模型')}</label>
          <ModelSelect
            value={value.source}
            options={without(value.target)}
            disabled={saving}
            ariaLabel={t('原模型')}
            onChange={(source) => patch({ source })}
          />
        </div>
        <div className="rt-subfield">
          <label>{t('压缩模型')}</label>
          <ModelSelect
            value={value.target}
            options={without(value.source)}
            disabled={saving}
            ariaLabel={t('压缩模型')}
            onChange={(target) => patch({ target })}
          />
        </div>
      </div>
    </div>
  );
}