/**
 * VisionCompat —— 视觉兼容面板（对照 A2A VisionSpecialPanel + AIGX
 * `image_compat::vision`）。
 *
 * 形状：一份规则列表 —— 「[哪些模型] 看不了图时 → 辅助识图 / 整体接管」。
 * 两种方式各自的附加项都挂在同一条规则上（`mode` 决定用哪一组）：
 *   · 辅助识图（assist）：`source`（沿用工具服务的视觉模型 / 指定）/ `target` / `reuseResults`
 *   · 整体接管（reroute）：`targets[]`（首选 + 备选，按序尝试）。
 *     `preferCurrentRoute` 是结构性缺口（AIGX 选路前裁决）：本面板不提供开关、
 *     提交时恒为 false（后端 PUT 拒绝 true、GET 恒回 false；后端默认值也已是
 *     false，此处保留仅为显式化）。`fallback` 固定 designated —— 仅为兼容老形状，
 *     不参与裁决（后端只要求 targets 非空）。
 * 另一组字段照样随规则保存（切回该方式时之前填的还在）——与后端同构。
 *
 * 后端契约（be-vision）：
 *   GET /api/vision-compatibility  → { config, revision, models[{provider,providerLabel,model,name,capabilities}], sharedTarget, maxTargets, maxModels }
 *   PUT /api/vision-compatibility  ← { config, revision }
 * 视觉模型按 provider+model 引用（同名模型跨提供商时靠 provider 区分）。
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Plus } from 'lucide-react';
import { Button } from '../../ui';
import { api } from '../../../api';
import {
  foldVisionBlocks,
  normalizeVisionConfig,
  visionModelChoices,
  type VisionCompatView,
  type VisionModelChoice,
  type VisionRule,
  type VisionSettingsConfig,
  type VisionTarget,
} from '../../../types/routing-workspace';
import { useSpecialRouteEditor } from './useSpecialRouteEditor';
import RuleDialog from './RuleDialog';
import { AddRuleButton, RulePanel, RuleRow, RuleSectionHead, RuleSwitchRow } from './shared';

interface Editing {
  rule: VisionRule;
  index: number;
}

/** provider+model 组合键（同一模型 ID 可能来自多家） */
const refKey = (ref: { provider: string; model: string }): string =>
  `${ref.provider.toLowerCase()}\u0000${ref.model.toLowerCase()}`;

const refLabel = (models: VisionModelChoice[], ref: { provider: string; model: string } | null): string => {
  if (!ref) return '';
  const found = models.find((m) => refKey(m) === refKey(ref));
  return found ? `${found.providerLabel || found.provider} · ${found.model}` : `${ref.provider} · ${ref.model}`;
};

export default function VisionCompat({ active }: { active: boolean }): JSX.Element {
  const { t } = useTranslation();
  const editor = useSpecialRouteEditor<VisionSettingsConfig, VisionCompatView>(
    {
      load: () => api.getVisionCompatibility(),
      save: (payload) =>
        api.saveVisionCompatibility(
          foldVisionBlocks(payload.config as VisionSettingsConfig),
          payload.revision,
        ),
    },
    normalizeVisionConfig,
    active,
  );
  const { view, draft, saving } = editor;
  const [editing, setEditing] = useState<Editing | null>(null);
  /**
   * 弹窗底栏「保存规则」的提交口：表单草稿住在 RuleEditor 内部（24 处字段引用），
   * 由它把 `() => onSave(draft)` 注册到这里，底栏按钮再触发 —— 避免把整张表单
   * 改成受控而牵动全部字段（也避免把弹窗状态与表单状态耦合）。
   */
  const submitRef = useRef<(() => void) | null>(null);

  const models = useMemo(() => visionModelChoices(view?.models), [view]);
  const sharedTarget = view?.sharedTarget ?? null;
  const rules = draft?.rules ?? [];
  const maxTargets = Number.isFinite(view?.maxTargets) ? Number(view?.maxTargets) : 8;

  const blankRule = (): VisionRule => ({
    enabled: true,
    models: [],
    mode: 'assist',
    source: 'shared',
    target: null,
    reuseResults: true,
    // 结构性缺口：后端拒绝 true（选路前裁决），新建规则固定 false
    preferCurrentRoute: false,
    fallback: 'stop',
    targets: [],
  });

  const ruleDetail = (rule: VisionRule): JSX.Element => {
    if (rule.mode === 'assist') {
      const reader =
        rule.source === 'shared'
          ? sharedTarget
            ? `${t('MCP & Skills 的视觉模型')}（${refLabel(models, sharedTarget) || `${sharedTarget.provider} · ${sharedTarget.model}`}）`
            : t('MCP & Skills 的视觉模型')
          : refLabel(models, rule.target) || t('还没选视觉模型');
      return (
        <>
          <span>{t('先读图再交回')}</span>
          <code>{reader}</code>
          <span>· {t('原模型继续回答')}</span>
        </>
      );
    }
    const primary = rule.targets[0] ?? null;
    // 对齐上游 detail()：preferCurrentRoute 为真时接管方是「当前路由内的视觉模型」。
    // AIGX 后端当前恒回 false（选路前裁决），故此分支暂不触发；后端补齐后即生效，
    // 无需再改本组件 —— 保留为真分支而非写死 targets，是为「一比一搬运」的口径。
    const taker = rule.preferCurrentRoute
      ? t('当前路由内的视觉模型')
      : refLabel(models, primary) || t('还没选视觉模型');
    const backups =
      !rule.preferCurrentRoute && rule.targets.length > 1
        ? t('· {{n}} 个备选', { n: rule.targets.length - 1 })
        : '';
    return (
      <>
        <span>{t('整次请求改交')}</span>
        <code>{taker}</code>
        {backups ? <span>{backups}</span> : null}
      </>
    );
  };

  return (
    <RulePanel
      title={t('图片兼容')}
      lede={t('模型看不了图片时，按下面的规则交给视觉模型；没配到的模型、以及本来就能看图的，一律原样放行。')}
      editor={editor}
    >
      <div className="rt-notice rt-notice-warn" role="note">
        <strong>{t('已知差距（与上游视觉兼容对照，避免误以为以下能力已生效）：')}</strong>
        <ul style={{ margin: '6px 0 0', paddingLeft: 18 }}>
          <li>{t('修订号非强制：仅在提交 revision 时校验，未提供则放行；AIGX 无配置版本 CAS。')}</li>
          <li>{t('旧版扁平配置不做模型目录展开（legacyExpanded 仅标记读到的是旧形状）。')}</li>
          <li>{t('不支持「沿用当前路由内的视觉模型」（prefer current route）：视觉裁决在选路前，保存该项会被拒；界面已按上游补上该开关但暂置为禁用（后端待补），开启后才会生效。')}</li>
        </ul>
      </div>

      <RuleSectionHead
        title={t('规则')}
        count={t('{{n}} 条', { n: rules.length })}
        actions={
          <>
            <span className="rt-count">
              {t('每规则最多 {{n}} 个接管目标', { n: maxTargets })}
            </span>
            <AddRuleButton
              disabled={saving || !models.length}
              onClick={() => setEditing({ rule: blankRule(), index: -1 })}
            />
          </>
        }
      />
      {!rules.length ? (
        <div className="rt-state-note rt-state-inherit">
          {t('还没有规则。图片会照常交给原模型，看不了的就会报错。')}
        </div>
      ) : null}

      {rules.map((rule, index) => (
        <RuleRow
          key={`row-${index}`}
          off={!rule.enabled}
          title={
            rule.models.length
              ? t('{{models}} 看不了图时', {
                  models: rule.models
                    .map((m) => refLabel(models, m) || `${m.provider} · ${m.model}`)
                    .join('、'),
                })
              : t('未选择模型')
          }
          detail={ruleDetail(rule)}
          meta={
            <>
              <span className="rt-chip rt-chip-model rt-chip-sm">
                {rule.mode === 'assist' ? t('辅助识图') : t('整体接管')}
              </span>
              {rule.enabled ? null : <span>{t('已停用')}</span>}
            </>
          }
          onOpen={saving ? undefined : () => setEditing({ rule: structuredClone(rule), index })}
        />
      ))}

      {!models.length ? (
        <div className="rt-notice rt-notice-warn">
          {t('还没有提供视觉能力信息的模型，请先启用模型和对应账号。')}
        </div>
      ) : null}

      {editing ? (
        <RuleDialog
          title={editing.index < 0 ? t('添加规则') : t('编辑规则')}
          subtitle={t('这条规则里的模型收到图片时按选定的方式处理；没被任何规则选中的模型一律原样放行。')}
          saving={saving}
          onClose={() => setEditing(null)}
          onApply={() => submitRef.current?.()}
          onDelete={
            editing.index < 0
              ? undefined
              : () =>
                  editor.update((current) => ({
                    ...current,
                    rules: current.rules.filter((_, at) => at !== editing.index),
                  }))
          }
        >
          <RuleEditor
            rule={editing.rule}
            models={models}
            sharedTarget={sharedTarget}
            maxTargets={maxTargets}
            saving={saving}
            registerSubmit={(submit) => {
              submitRef.current = submit;
            }}
            onSave={(next) => {
              editor.update((current) => ({
                ...current,
                rules:
                  editing.index < 0
                    ? [...current.rules, next]
                    : current.rules.map((item, at) => (at === editing.index ? next : item)),
              }));
              setEditing(null);
            }}
          />
        </RuleDialog>
      ) : null}
    </RulePanel>
  );
}

/** 「哪些模型」多选（按 provider+model 引用，勾选保留提供商区分） */
function ModelRefPicker({
  value,
  options,
  disabled,
  onChange,
}: {
  value: Array<{ provider: string; model: string }>;
  options: VisionModelChoice[];
  disabled?: boolean;
  onChange: (next: Array<{ provider: string; model: string }>) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [candidate, setCandidate] = useState('');
  const selected = new Set(value.map((v) => refKey(v)));
  const add = (key: string): void => {
    const found = options.find((m) => refKey(m) === key);
    if (!found) return;
    const next = [...value, { provider: found.provider, model: found.model }];
    onChange(next.filter((v, i, arr) => arr.findIndex((x) => refKey(x) === refKey(v)) === i));
    setCandidate('');
  };
  if (!options.length) {
    return <span className="form-hint">{t('没有候选模型。')}</span>;
  }
  return (
    <div className="rt-subfield">
      <div className="rt-special-checks">
        {value.map((ref) => (
          <span key={refKey(ref)} className="rt-chip rt-chip-model rt-chip-md">
            <span>{refLabel(options, ref)}</span>
            <button
              type="button"
              className="rt-chip-x"
              aria-label={t('移除 {{name}}', { name: refLabel(options, ref) })}
              disabled={disabled}
              onClick={() => onChange(value.filter((v) => refKey(v) !== refKey(ref)))}
            >
              ×
            </button>
          </span>
        ))}
      </div>
      <div className="rt-special-add-row">
        <select
          className="form-input"
          aria-label={t('选择一个或多个模型')}
          value={candidate}
          disabled={disabled}
          onChange={(e) => {
            setCandidate(e.target.value);
            if (e.target.value) add(e.target.value);
          }}
        >
          <option value="">{t('选择一个或多个模型')}</option>
          {options
            .filter((m) => !selected.has(refKey(m)))
            .map((m) => (
              <option key={refKey(m)} value={refKey(m)}>
                {refLabel(options, m)}
              </option>
            ))}
        </select>
      </div>
    </div>
  );
}

function RuleEditor({
  rule,
  models,
  sharedTarget,
  maxTargets,
  saving,
  registerSubmit,
  onSave,
}: {
  rule: VisionRule;
  models: VisionModelChoice[];
  sharedTarget?: VisionTarget | null;
  maxTargets: number;
  saving: boolean;
  /** 把「提交当前草稿」注册给弹窗底栏 */
  registerSubmit: (submit: () => void) => void;
  onSave: (next: VisionRule) => void;
}): JSX.Element {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<VisionRule>(rule);
  const patch = (part: Partial<VisionRule>): void => setDraft((d) => ({ ...d, ...part }));

  // 每次草稿变化都重新注册，保证底栏提交拿到的是最新草稿（不是首帧闭包）
  useEffect(() => {
    const ready =
      !saving &&
      draft.models.length > 0 &&
      !(draft.mode === 'assist' && draft.source === 'custom' && !draft.target) &&
      !(draft.mode === 'reroute' && draft.targets.length === 0);
    registerSubmit(() => {
      if (!ready) return;
      onSave(draft);
    });
  }, [draft, onSave, registerSubmit, saving]);

  /**
   * 两个候选面（照 A2A `VisionRuleDialog` 的取法，刻意不对称）：
   *   · 「哪些模型用这种方式」= 看不了图 / 能力位未声明的模型（`supportsImages !== true`）；
   *   · 「用哪只模型识图 / 接管回答」= 只有明确声明能看图的模型（`supportsImages === true`）。
   * 不对称是对的：能力位缺失时不能拿来当视觉目标（那会把图片丢给一只看不了图的模型），
   * 但它仍然该出现在「需要兼容」的名单里。
   */
  const [listModels, targetModels] = useMemo(
    () => [
      models.filter((m) => m.supportsImages !== true),
      models.filter((m) => m.supportsImages === true),
    ],
    [models],
  );

  const targetKey = (target: VisionTarget): string =>
    `${target.provider.toLowerCase()}\u0000${target.model.toLowerCase()}`;
  const pickTarget = (key: string, template?: VisionTarget): VisionTarget | null => {
    const found = targetModels.find((m) => refKey(m) === key);
    return found
      ? { provider: found.provider, model: found.model, reasoning: template?.reasoning ?? '' }
      : null;
  };

  const setPrimary = (key: string): void => {
    const t0 = pickTarget(key, draft.targets[0]);
    if (!t0) return;
    patch({ targets: [t0, ...draft.targets.slice(1)], fallback: 'designated' });
  };
  const addBackup = (key: string): void => {
    const t0 = pickTarget(key);
    if (!t0 || draft.targets.length >= maxTargets) return;
    patch({ targets: [...draft.targets, t0], fallback: 'designated' });
  };
  const move = (index: number, delta: number): void => {
    const to = index + delta;
    if (to < 0 || to >= draft.targets.length) return;
    const next = [...draft.targets];
    [next[index], next[to]] = [next[to], next[index]];
    patch({ targets: next, fallback: 'designated' });
  };

  const [backupChoice, setBackupChoice] = useState('');
  const primary = draft.targets[0] ?? null;

  const sourceValue =
    draft.source === 'shared' ? '__shared' : (draft.target ? refKey(draft.target) : '');
  const setSource = (key: string): void => {
    if (key === '__shared') {
      patch({ source: 'shared' });
      return;
    }
    const found = pickTarget(key, draft.target ?? undefined);
    if (found) patch({ source: 'custom', target: found });
  };

  return (
    <div className="rt-special-editor">
      <RuleSwitchRow
        enabled={draft.enabled}
        disabled={saving}
        onChange={(enabled) => patch({ enabled })}
      />
      <div className="rt-subfield">
        <label>{t('哪些模型用这种方式')}</label>
        <ModelRefPicker
          value={draft.models}
          options={listModels}
          disabled={saving}
          onChange={(next) => patch({ models: next })}
        />
      </div>

      <div className="rt-subfield">
        <label>{t('处理方式')}</label>
        <select
          className="form-input"
          aria-label={t('处理方式')}
          value={draft.mode}
          disabled={saving}
          onChange={(e) => patch({ mode: e.target.value as VisionRule['mode'] })}
        >
          <option value="assist">{t('辅助识图')}</option>
          <option value="reroute">{t('整体接管')}</option>
        </select>
      </div>

      {draft.mode === 'assist' ? (
        <>
          <div className="rt-grid2">
            <div className="rt-subfield">
              <label>{t('用哪只模型识图')}</label>
              <select
                className="form-input"
                aria-label={t('用哪只模型识图')}
                value={sourceValue}
                disabled={saving}
                onChange={(e) => setSource(e.target.value)}
              >
                <option value="__shared">{t('沿用 MCP & Skills 的视觉模型')}</option>
                {targetModels.map((m) => {
                  const key = refKey(m);
                  return (
                    <option key={key} value={key}>
                      {refLabel(targetModels, { provider: m.provider, model: m.model })}
                    </option>
                  );
                })}
              </select>
              <span className="form-hint">{t('沿用 MCP & Skills = 使用该页配置里已有的视觉模型。')}</span>
            </div>
            <div className="rt-subfield">
              <label>{t('复用相同图片的识读结果')}</label>
              <label className="rt-special-check" style={{ height: 36, alignItems: 'center' }}>
                <input
                  type="checkbox"
                  checked={draft.reuseResults}
                  disabled={saving}
                  onChange={(e) => patch({ reuseResults: e.target.checked })}
                />
                <span>{t('同会话同问题复用；追问新细节会重新识读')}</span>
              </label>
            </div>
          </div>
        </>
      ) : (
        <>
          <div className="rt-notice rt-notice-info">
            {t('AIGX 的视觉兼容在选路前裁决，拿不到「当前路由内的视觉模型」——整体接管必须指定接管模型（按顺序尝试）。')}
          </div>

          {/* H1 对齐上游 routing-special-panels.tsx:351-357 的 preferCurrentRoute 开关。
              字段在后端已存在（`VisionRuleItem.prefer_current_route`），但 AIGX 是结构性缺口：
              视觉裁决在选路前，后端 `vision::validate` 拒绝保存 true、GET 恒回 false
              （`src/api/admin/vision_compat.rs:155-158`）。故此处补上开关的 UI 槽位与之一比一，
              但**禁用**并标「后端待补」——开一个存了也不生效的开关比不提供更糟。
              后端若改为选路后裁决并放行 true，去掉 disabled 即可生效，无需改本组件其余逻辑。 */}
          <div className="rt-subfield">
            <label>{t('优先使用当前路由内的视觉模型')}</label>
            <label className="rt-special-check" style={{ height: 36, alignItems: 'center' }}>
              <input
                type="checkbox"
                checked={draft.preferCurrentRoute}
                disabled
                aria-label={t('优先使用当前路由内的视觉模型')}
                onChange={(e) => patch({ preferCurrentRoute: e.target.checked })}
              />
              <span>
                {t('先尝试当前路由内的视觉模型，不可用时再按顺序尝试指定模型；指定模型可跨路由池。')}
              </span>
            </label>
            <span className="form-hint" style={{ color: 'var(--rt-warn, inherit)' }}>
              {t('（后端待补：AIGX 视觉裁决在选路前，无「当前路由内的视觉模型」概念，保存该项会被拒，故本开关暂禁用。）')}
            </span>
          </div>

          <div className="rt-grid2">
            <div className="rt-subfield">
              <label>{t('接管回答的模型')}</label>
              <select
                className="form-input"
                aria-label={t('接管回答的模型')}
                value={primary ? targetKey(primary) : ''}
                disabled={saving}
                onChange={(e) => setPrimary(e.target.value)}
              >
                <option value="">{t('请选择模型')}</option>
                {targetModels.map((m) => (
                  <option key={refKey(m)} value={refKey(m)}>
                    {refLabel(targetModels, { provider: m.provider, model: m.model })}
                  </option>
                ))}
              </select>
            </div>
            <div className="rt-subfield">
              <label>{t('备选模型')}</label>
              <div className="rt-special-add-row">
                <select
                  className="form-input"
                  aria-label={t('备选模型')}
                  value={backupChoice}
                  disabled={saving || draft.targets.length >= maxTargets}
                  onChange={(e) => setBackupChoice(e.target.value)}
                >
                  <option value="">{t('请选择模型')}</option>
                  {targetModels
                    .filter((m) => !draft.targets.some((t0) => targetKey(t0) === refKey(m)))
                    .map((m) => (
                      <option key={refKey(m)} value={refKey(m)}>
                        {refLabel(targetModels, { provider: m.provider, model: m.model })}
                      </option>
                    ))}
                </select>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={saving || !backupChoice || draft.targets.length >= maxTargets}
                  onClick={() => {
                    addBackup(backupChoice);
                    setBackupChoice('');
                  }}
                >
                  <Plus size={13} />
                  {t('加入备选')}
                </Button>
              </div>
              {draft.targets.slice(1).map((backup, index) => (
                <div key={targetKey(backup)} className="rt-special-backup-row">
                  <span>{refLabel(targetModels, backup)}</span>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={saving}
                    onClick={() => move(index + 1, -1)}
                  >
                    {t('上移')}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={saving}
                    onClick={() => move(index + 1, 1)}
                  >
                    {t('下移')}
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={saving}
                    onClick={() =>
                      patch({ targets: draft.targets.filter((_, at) => at !== index + 1), fallback: 'designated' })
                    }
                  >
                    {t('移除')}
                  </Button>
                </div>
              ))}
              <span className="form-hint">
                {t('备选按顺序尝试；至少需要一个接管模型。')}
              </span>
            </div>
          </div>
        </>
      )}

      {draft.mode === 'assist' && draft.source === 'custom' && !draft.target ? (
        <div className="rt-notice rt-notice-warn">
          {t('已选「指定视觉模型」但还没选具体模型。')}
        </div>
      ) : null}
      {draft.mode === 'assist' && draft.source === 'shared' ? (
        <div className="rt-notice rt-notice-info">
          {t('沿用 MCP & Skills 的视觉模型：视觉目标由「MCP & Skills」的视觉配置决定，本规则只负责把请求路由到识图环节。')}
          {sharedTarget ? (
            <div className="form-hint">
              {t('当前共享视觉模型：{{model}}', {
                model: refLabel(models, sharedTarget) || `${sharedTarget.provider} · ${sharedTarget.model}`,
              })}
            </div>
          ) : (
            <div className="form-hint">
              {t('MCP & Skills 当前未配置共享视觉模型；启用「沿用」前请先在该页配置。')}
            </div>
          )}
        </div>
      ) : null}

    </div>
  );
}