/**
 * TitleRoutes —— 标题生成路由面板（对照 A2A TitleSpecialPanel + AIGX
 * `route_special::title`）。
 *
 * 形状：一份规则列表 —— 「[客户端们] 的标题请求 → 改用 [模型]，失败时
 * 退回原模型 / 直接报错」。规则按列表顺序匹配，第一条命中的生效；
 * 只改写被识别为标题生成的小请求，普通对话不受影响。
 *
 * 后端契约（canonical 对齐 Agent2API v3.5.0-beta.1）：
 *   GET  /api/models/title-routes       → { config, revision, clients[{id,label,supported}], models[], maxRules }
 *   PUT  /api/models/title-routes       ← { config: { enabled, rules[] }, revision }
 * 不做路径回退：端点未注册 → 直接报错可见（见 api/index.ts）。
 */
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Pencil } from 'lucide-react';
import { Button } from '../../ui';
import { api } from '../../../api';
import {
  modelChoices,
  normalizeTitleConfig,
  type TitleRouteRule,
  type TitleRoutesConfig,
  type TitleRoutesView,
} from '../../../types/routing-workspace';
import { useSpecialRouteEditor } from './useSpecialRouteEditor';
import { AddRuleButton, ModelSelect, RulePanel, RuleRow, RuleSectionHead, RowDeleteButton } from './shared';

interface Editing {
  rule: TitleRouteRule;
  index: number;
}

export default function TitleRoutes({ active }: { active: boolean }): JSX.Element {
  const { t } = useTranslation();
  const editor = useSpecialRouteEditor<TitleRoutesConfig, TitleRoutesView>(
    {
      load: () => api.getTitleRoutes(),
      save: (payload) => api.saveTitleRoutes(payload.config as TitleRoutesConfig, payload.revision),
    },
    normalizeTitleConfig,
    active,
  );
  const { view, draft, saving } = editor;
  const [editing, setEditing] = useState<Editing | null>(null);

  const clients = useMemo(
    () =>
      (Array.isArray(view?.clients) ? view?.clients ?? [] : []).map((c) => ({
        id: String(c.id),
        label: String(c.label ?? c.id),
        supported: c.supported !== false,
      })),
    [view],
  );
  const models = useMemo(() => modelChoices(view?.models), [view]);
  const clientLabel = (id: string): string => clients.find((c) => c.id === id)?.label ?? id;
  const maxRules = Number.isFinite(view?.maxRules) ? Number(view?.maxRules) : 0;
  const rules = draft?.rules ?? [];
  const full = maxRules > 0 && rules.length >= maxRules;

  const blankRule = (): TitleRouteRule => ({
    enabled: true,
    clients: [],
    model: '',
    fallback: 'original',
  });

  const write = (index: number, rule: TitleRouteRule): void => {
    editor.update((current) => ({
      ...current,
      rules:
        index < 0
          ? [...current.rules, rule]
          : current.rules.map((item, at) => (at === index ? rule : item)),
    }));
  };

  return (
    <RulePanel
      title={t('标题生成')}
      lede={t('只改写被识别为标题生成的小请求，普通对话不受影响。规则按列表顺序匹配，第一条命中的生效。')}
      editor={editor}
      serverError={view?.error ?? null}
    >
      <RuleSectionHead
        title={t('规则')}
        count={t('{{n}} 条', { n: rules.length })}
        actions={
          <AddRuleButton
            disabled={saving || full || !clients.length}
            onClick={() => setEditing({ rule: blankRule(), index: -1 })}
          />
        }
      />
      {!rules.length ? (
        <div className="rt-state-note rt-state-inherit">
          {t('还没有规则。标题请求会照常交给原模型。')}
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
            title={
              rule.clients.length
                ? t('{{clients}} 的标题请求', {
                    clients: rule.clients.map(clientLabel).join('、'),
                  })
                : t('未选择客户端')
            }
            detail={
              <>
                <span>{t('改用')}</span>
                <code>{rule.model || t('还没选标题模型')}</code>
                <span>·</span>
                <span>
                  {rule.fallback === 'original' ? t('失败时退回原模型') : t('失败时直接报错')}
                </span>
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
                <RowDeleteButton
                  disabled={saving}
                  onClick={() => {
                    editor.update((current) => ({
                      ...current,
                      rules: current.rules.filter((_, at) => at !== index),
                    }));
                    setEditing(null);
                  }}
                />
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

function RuleEditor({
  rule,
  clients,
  models,
  saving,
  onCancel,
  onSave,
}: {
  rule: TitleRouteRule;
  clients: Array<{ id: string; label: string; supported: boolean }>;
  models: Array<{ id: string; label: string }>;
  saving: boolean;
  onCancel: () => void;
  onSave: (next: TitleRouteRule) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<TitleRouteRule>(rule);
  const patch = (part: Partial<TitleRouteRule>): void => setDraft((d) => ({ ...d, ...part }));
  const toggleClient = (id: string, on: boolean): void => {
    patch({
      clients: on ? [...new Set([...draft.clients, id])] : draft.clients.filter((c) => c !== id),
    });
  };

  return (
    <div className="rt-special-editor">
      <div className="rt-subfield">
        <label>{t('识别客户端')}</label>
        <div className="rt-special-checks">
          {clients.map((c) => (
            <label key={c.id} className="rt-special-check" title={c.supported ? undefined : t('该客户端暂无识别判据，勾选也不会命中')}>
              <input
                type="checkbox"
                checked={draft.clients.includes(c.id)}
                onChange={(e) => toggleClient(c.id, e.target.checked)}
              />
              <span>{c.label}</span>
              {!c.supported ? <em className="rt-count">{t('无识别判据')}</em> : null}
            </label>
          ))}
        </div>
        <span className="form-hint">
          {t('一条规则可勾多个客户端；客户端名单由后端下发，界面不各写一份。')}
        </span>
      </div>

      <div className="rt-grid2">
        <div className="rt-subfield">
          <label>{t('标题模型')}</label>
          <ModelSelect
            value={draft.model}
            options={models}
            disabled={saving}
            ariaLabel={t('标题模型')}
            onChange={(model) => patch({ model })}
          />
        </div>
        <div className="rt-subfield">
          <label>{t('标题模型失败时')}</label>
          <select
            className="form-input"
            aria-label={t('标题模型失败时')}
            value={draft.fallback}
            onChange={(e) => patch({ fallback: e.target.value as TitleRouteRule['fallback'] })}
          >
            <option value="original">{t('退回原模型')}</option>
            <option value="error">{t('直接报错')}</option>
          </select>
          <span className="form-hint">
            {t('退回原模型 = 用客户端请求里的原模型重试一次；直接报错不改用原模型。')}
          </span>
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
            disabled={saving || !draft.clients.length || !draft.model}
            onClick={() => onSave(draft)}
          >
            {t('应用')}
          </Button>
        </div>
      </div>
    </div>
  );
}