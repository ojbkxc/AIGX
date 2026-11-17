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
import { Pencil } from 'lucide-react';
import { Button } from '../../ui';
import { api } from '../../../api';
import {
  modelChoices,
  normalizeCompactConfig,
  type CompactRoutesConfig,
  type CompactRouteRule,
} from '../../../types/routing-workspace';
import { useSpecialRouteEditor } from './useSpecialRouteEditor';
import { ModelSelect, RulePanel, RuleRow, RuleSectionHead, RowDeleteButton } from './shared';

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
      addLabel={t('添加规则')}
      addDisabled={saving || full || !clients.length}
      onAddRule={() => setEditing({ rule: blankRule(), index: -1 })}
    >
      <RuleSectionHead
        title={t('规则')}
        count={t('{{n}} 条', { n: rules.length })}
      />
      {!rules.length ? (
        <div className="rt-state-note rt-state-inherit">
          {t('还没有规则。压缩请求会按原模型照常转发。')}
        </div>
      ) : null}

      {rules.map((rule, index) =>
        editing && editing.index === index ? (
          <RuleEditor
            key={`edit-${index}`}
            rule={editing.rule}
            clients={clients}
            models={models}
            saving={saving}
            onCancel={() => setEditing(null)}
            onSave={(next) => {
              write(index, next);
              setEditing(null);
            }}
          />
        ) : (
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
            actions={
              <>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={saving}
                  onClick={() => setEditing({ rule: { ...rule }, index })}
                >
                  <Pencil size={13} />
                  {t('编辑')}
                </Button>
                <RowDeleteButton disabled={saving} onClick={() => remove(index)} />
              </>
            }
          />
        ),
      )}

      {!models.length ? (
        <div className="rt-notice rt-notice-warn">
          {t('没有可选的对话模型，请先启用模型和对应账号（当前仍可手填模型名保存）。')}
        </div>
      ) : null}
      {full ? (
        <div className="rt-notice rt-notice-warn">
          {t('规则已达上限 {{n}} 条。', { n: maxRules })}
        </div>
      ) : null}

      {editing && editing.index < 0 ? (
        <RuleEditor
          key="new"
          rule={editing.rule}
          clients={clients}
          models={models}
          saving={saving}
          onCancel={() => setEditing(null)}
          onSave={(next) => {
            write(-1, next);
            setEditing(null);
          }}
        />
      ) : null}
    </RulePanel>
  );
}

/** 行内编辑器：客户端 + 原模型 + 压缩模型 + 启用开关 */
function RuleEditor({
  rule,
  clients,
  models,
  saving,
  onCancel,
  onSave,
}: {
  rule: CompactRouteRule;
  clients: Array<{ id: string; label: string }>;
  models: Array<{ id: string; label: string }>;
  saving: boolean;
  onCancel: () => void;
  onSave: (next: CompactRouteRule) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<CompactRouteRule>(rule);
  const patch = (part: Partial<CompactRouteRule>): void => setDraft((d) => ({ ...d, ...part }));

  return (
    <div className="rt-special-editor">
      <div className="rt-special-editor-fields">
        <div className="rt-subfield">
          <label>{t('触发客户端')}</label>
          <select
            className="form-input"
            aria-label={t('触发客户端')}
            value={draft.client}
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
            value={draft.source}
            options={models}
            disabled={saving}
            ariaLabel={t('原模型')}
            onChange={(source) => patch({ source })}
          />
        </div>
        <div className="rt-subfield">
          <label>{t('压缩模型')}</label>
          <ModelSelect
            value={draft.target}
            options={models}
            disabled={saving}
            ariaLabel={t('压缩模型')}
            onChange={(target) => patch({ target })}
          />
        </div>
      </div>
      <div className="rt-special-editor-foot">
        <label className="rt-special-check">
          <input
            type="checkbox"
            checked={draft.enabled}
            onChange={(e) => patch({ enabled: e.target.checked })}
          />
          <span>{t('启用这条规则')}</span>
        </label>
        <div className="rt-special-editor-actions">
          <Button variant="outline" size="sm" disabled={saving} onClick={onCancel}>
            {t('取消')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            disabled={saving || !draft.client || !draft.source || !draft.target}
            onClick={() => onSave(draft)}
          >
            {t('应用')}
          </Button>
        </div>
      </div>
      {!draft.enabled ? (
        <div className="form-hint">{t('停用的规则不参与匹配，但仍保留在列表里。')}</div>
      ) : null}
    </div>
  );
}