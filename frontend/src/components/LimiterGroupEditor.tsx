import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AlertTriangle, Plus, Trash2 } from 'lucide-react';
import { Button, Input, Select, Switch } from './ui';
import { defaultCondition, isLocalMetric, validateGroups } from '../lib/limiterGroups';
import type {
  LimiterComparison,
  LimiterGroup,
  LimiterJoin,
  LimiterMetric,
  LimiterReset,
} from '../types/limiter';

/**
 * 条件组编辑器（#29 二级弹层）：组级（启用/动作/目标）+ 条件行
 * （join / 来源 / 模型 / 指标 / 比较符 / 阈值 / 窗口）。
 *
 * 保存前走 lib/limiterGroups.validateGroups（与后端 400 同文案，前端先拦）。
 * **tokens / requests 两类本地计数当前恒 unknown、不会命中**（计数器 A2 接入），
 * 指标选项与命中行都显式标「暂未生效」，不允许做成看起来能用的控件。
 * 窗口的 startsAt/endsAt/utcOffsetMinutes 为 v1 不暴露字段，编辑既有组时原样透传。
 */
interface Props {
  /** null = 新增组；否则编辑既有组 */
  initial: LimiterGroup | null;
  onSave: (group: LimiterGroup) => void;
  onClose: () => void;
}

/** 指标 → 展示名（tokens/requests 带暂未生效标注） */
const METRIC_LABELS: Record<LimiterMetric, string> = {
  remaining: '剩余额度',
  remaining_percent: '剩余额度 %',
  used: '已用额度',
  used_percent: '已用额度 %',
  tokens: 'Token 消耗（暂未生效）',
  requests: '请求数（暂未生效）',
};

const COMPARISON_LABELS: Record<LimiterComparison, string> = {
  lt: '＜',
  lte: '≤',
  gte: '≥',
};

const RESET_LABELS: Record<LimiterReset, string> = {
  fixed: '固定窗口',
  rolling: '滚动窗口',
  daily: '每日（自然日）',
  weekly: '每周',
  monthly: '每月',
  total: '累计（不重置）',
};

export default function LimiterGroupEditor({ initial, onSave, onClose }: Props): JSX.Element {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<LimiterGroup>(() => initial ?? {
    enabled: true,
    action: 'skip',
    target: 'models',
    conditions: [defaultCondition()],
  });
  /** 阈值 / 秒数 / 模型清单以字符串中转（输入中途允许为空） */
  const [thresholdInputs, setThresholdInputs] = useState<string[]>(() =>
    draft.conditions.map((c) => String(c.threshold)));
  const [secondsInputs, setSecondsInputs] = useState<string[]>(() =>
    draft.conditions.map((c) => String(c.window?.seconds ?? '')));
  const [modelInputs, setModelInputs] = useState<string[]>(() =>
    draft.conditions.map((c) => c.models.join(', ')));
  const [error, setError] = useState('');

  const patchCondition = (index: number, patch: Partial<LimiterGroup['conditions'][number]>): void => {
    setError('');
    setDraft((prev) => ({
      ...prev,
      conditions: prev.conditions.map((c, i) => (i === index ? { ...c, ...patch } : c)),
    }));
  };

  const addCondition = (): void => {
    setError('');
    setDraft((prev) => ({
      ...prev,
      conditions: [...prev.conditions, { ...defaultCondition(), join: 'and' }],
    }));
    setThresholdInputs((prev) => [...prev, '0']);
    setSecondsInputs((prev) => [...prev, '']);
    setModelInputs((prev) => [...prev, '']);
  };

  const removeCondition = (index: number): void => {
    setError('');
    if (draft.conditions.length <= 1) return;
    setDraft((prev) => ({ ...prev, conditions: prev.conditions.filter((_, i) => i !== index) }));
    setThresholdInputs((prev) => prev.filter((_, i) => i !== index));
    setSecondsInputs((prev) => prev.filter((_, i) => i !== index));
    setModelInputs((prev) => prev.filter((_, i) => i !== index));
  };

  const handleSave = (): void => {
    // 字符串草稿 → 数值/清单归一化（后端 validate 前置；窗口透传不暴露字段）
    const conditions = draft.conditions.map((c, i) => {
      const threshold = Number(thresholdInputs[i]);
      const isLocal = isLocalMetric(c.metric);
      let window = c.window;
      if (isLocal) {
        const seconds = Number(secondsInputs[i]);
        window = {
          ...(c.window ?? { reset: 'fixed' as LimiterReset, seconds: 0 }),
          reset: c.window?.reset ?? 'fixed',
          seconds: Number.isFinite(seconds) ? seconds : 0,
        };
      } else {
        window = null;
      }
      return {
        ...c,
        sourceId: c.sourceId.trim(),
        models: modelInputs[i].split(/[,，\n]/).map((m) => m.trim()).filter(Boolean),
        threshold: Number.isFinite(threshold) ? threshold : Number.NaN,
        window,
      };
    });
    const next: LimiterGroup = { ...draft, conditions };
    const err = validateGroups([next]);
    if (err) {
      setError(err);
      return;
    }
    onSave(next);
  };

  return (
    <div className="modal-overlay as-lim-overlay">
      <div className="modal as-lim-modal" role="dialog" aria-modal="true">
        <div className="modal-header">
          <h3>{initial ? t('编辑条件组') : t('新增条件组')}</h3>
          <button type="button" className="modal-close" onClick={onClose}>&times;</button>
        </div>
        <div className="modal-body">
          <div className="as-lim-row">
            <span className="as-lim-label">{t('启用')}</span>
            <Switch
              checked={draft.enabled}
              onChange={(v) => setDraft((prev) => ({ ...prev, enabled: v }))}
              ariaLabel={t('启用条件组')}
              title={t('启用条件组')}
            />
          </div>
          <div className="as-lim-row">
            <span className="as-lim-label">{t('命中后')}</span>
            <div className="as-lim-cond">
              <Select
                value={draft.action}
                onChange={(e) => setDraft((prev) => ({ ...prev, action: e.target.value as LimiterGroup['action'] }))}
              >
                <option value="skip">{t('跳过本次（自动恢复）')}</option>
                <option value="disable">{t('停用账号（需手动启用）')}</option>
              </Select>
              <Select
                value={draft.target}
                onChange={(e) => setDraft((prev) => ({ ...prev, target: e.target.value as LimiterGroup['target'] }))}
              >
                <option value="models">{t('仅拦截条件中的模型')}</option>
                <option value="account">{t('拦截整个账号')}</option>
              </Select>
            </div>
          </div>

          {draft.conditions.map((c, i) => (
            <div key={i} className="as-lg-cond">
              <div className="as-lg-cond-head">
                {i === 0 ? (
                  <span className="as-lg-join">{t('当满足')}</span>
                ) : (
                  <Select
                    className="as-lg-join-select"
                    value={c.join}
                    onChange={(e) => patchCondition(i, { join: e.target.value as LimiterJoin })}
                    aria-label={t('与上一条条件的组合方式')}
                  >
                    <option value="and">{t('且满足（同一分支）')}</option>
                    <option value="or">{t('或满足（新分支）')}</option>
                  </Select>
                )}
                <button
                  type="button"
                  className="as-act as-act-danger"
                  onClick={() => removeCondition(i)}
                  disabled={draft.conditions.length <= 1}
                  title={t('删除条件')}
                >
                  <Trash2 size={13} />
                </button>
              </div>
              <div className="as-lg-cond-grid">
                <label className="as-lg-field">
                  <span>{t('读数指标')}</span>
                  <Select
                    value={c.metric}
                    onChange={(e) => {
                      const metric = e.target.value as LimiterMetric;
                      // 切指标**不动 window 对象本身**：startsAt/endsAt/utcOffsetMinutes
                      // 是 v1 不暴露的透传字段，置 null 会把既有值抹掉（复核缺口 3）；
                      // 仅在 local 类且无 window 时给默认窗口。保存归一时非 local
                      // 仍按后端要求置 null（handleSave），此处不重复承担该职责。
                      patchCondition(i, {
                        metric,
                        window: c.window ?? (isLocalMetric(metric) ? { reset: 'fixed', seconds: 86_400 } : null),
                      });
                    }}
                  >
                    {(Object.keys(METRIC_LABELS) as LimiterMetric[]).map((m) => (
                      <option key={m} value={m}>{t(METRIC_LABELS[m])}</option>
                    ))}
                  </Select>
                </label>
                <label className="as-lg-field as-lg-field-narrow">
                  <span>{t('比较')}</span>
                  <Select
                    value={c.comparison}
                    onChange={(e) => patchCondition(i, { comparison: e.target.value as LimiterComparison })}
                  >
                    {(Object.keys(COMPARISON_LABELS) as LimiterComparison[]).map((k) => (
                      <option key={k} value={k}>{COMPARISON_LABELS[k]}</option>
                    ))}
                  </Select>
                </label>
                <label className="as-lg-field">
                  <span>{t('阈值')}</span>
                  <Input
                    type="text"
                    value={thresholdInputs[i]}
                    onChange={(e) => {
                      const v = e.target.value;
                      setThresholdInputs((prev) => prev.map((x, j) => (j === i ? v : x)));
                      setError('');
                    }}
                  />
                </label>
              </div>
              {isLocalMetric(c.metric) ? (
                <div className="as-lg-warning">
                  <AlertTriangle size={13} />
                  <span>{t('本地计数暂未生效：计数器尚未接入，该条件在接入前不会命中')}</span>
                </div>
              ) : null}
              {isLocalMetric(c.metric) ? (
                <div className="as-lg-cond-grid">
                  <label className="as-lg-field">
                    <span>{t('计数窗口')}</span>
                    <Select
                      value={c.window?.reset ?? 'fixed'}
                      onChange={(e) => {
                        const reset = e.target.value as LimiterReset;
                        patchCondition(i, { window: { ...(c.window ?? { seconds: 0 }), reset } });
                      }}
                    >
                      {(Object.keys(RESET_LABELS) as LimiterReset[]).map((r) => (
                        <option key={r} value={r}>{t(RESET_LABELS[r])}</option>
                      ))}
                    </Select>
                  </label>
                  {(c.window?.reset ?? 'fixed') === 'fixed' || (c.window?.reset ?? 'fixed') === 'rolling' ? (
                    <label className="as-lg-field">
                      <span>{t('窗口秒数')}</span>
                      <Input
                        type="text"
                        placeholder="86400"
                        value={secondsInputs[i]}
                        onChange={(e) => {
                          const v = e.target.value;
                          setSecondsInputs((prev) => prev.map((x, j) => (j === i ? v : x)));
                          setError('');
                        }}
                      />
                    </label>
                  ) : null}
                </div>
              ) : (
                <label className="as-lg-field">
                  <span>{t('额度来源')}</span>
                  <Input
                    type="text"
                    placeholder={t('留空 = 账号主余额；钱包行填其名称或 id')}
                    value={c.sourceId}
                    onChange={(e) => patchCondition(i, { sourceId: e.target.value })}
                  />
                </label>
              )}
              <label className="as-lg-field">
                <span>{t('模型范围（逗号分隔，留空 = 不限）')}</span>
                <Input
                  type="text"
                  value={modelInputs[i]}
                  onChange={(e) => {
                    const v = e.target.value;
                    setModelInputs((prev) => prev.map((x, j) => (j === i ? v : x)));
                    setError('');
                  }}
              />
              </label>
            </div>
          ))}

          <div>
            <button type="button" className="btn btn-outline btn-sm" onClick={addCondition}>
              <Plus size={14} /> {t('添加条件')}
            </button>
            <div className="form-hint">
              {t('组合规则：相邻「且」归并为同一分支，分支内全部命中才算命中；任一「或」分支命中即触发动作。')}
            </div>
          </div>
        </div>
        <div className="modal-footer">
          {error ? <span className="as-dlg-err">{error}</span> : null}
          <Button variant="secondary" onClick={onClose}>{t('取消')}</Button>
          <Button onClick={handleSave}>{t('确定')}</Button>
        </div>
      </div>
    </div>
  );
}
