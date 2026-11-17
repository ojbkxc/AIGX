// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
// 能力位三态编辑弹窗（provider × model）—— 对照 A2A model-capability-dialog.tsx /
// model-capability.ts。
//
// 契约：POST /api/models/capabilities `{provider, id, capabilities: {...}}`
//   - 键集：maxInputTokens / maxOutputTokens / supportsToolCall / supportsImages /
//     supportsVideo / supportsReasoning（core/capability.rs KEYS）
//   - **三态**：未设置（null = 继承/未测）/ 开（true）/ 关（false）
//     三态在界面上用「继承 / 是 / 否」分段，绝不用两态开关表达（否则「关」和
//     「没测过」在语义上会被抹平，下游按错误量级推断）。
//   - token 键取值 1..1 亿；null 是合法的「清除」信号。

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Loader2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from './Toast';
import { Button, Input } from './ui';
import type { ManagedModel } from '../types';
import './ModelCapabilityDialog.css';

/** 布尔能力位（键 → 展示文案由 t() 字面量登记） */
const BOOL_KEYS = [
  'supportsToolCall',
  'supportsImages',
  'supportsVideo',
  'supportsReasoning',
] as const;

type BoolKey = (typeof BOOL_KEYS)[number];
/** 三态：undefined = 未设置（继承），true / false = 显式开/关 */
type TriState = boolean | undefined;

function boolLabel(t: (s: string) => string, key: BoolKey): string {
  switch (key) {
    case 'supportsToolCall': return t('工具调用');
    case 'supportsImages': return t('图像输入');
    case 'supportsVideo': return t('视频输入');
    default: return t('思考 / 推理');
  }
}

interface ModelCapabilityDialogProps {
  model: ManagedModel;
  onClose: () => void;
  onSaved: () => void;
}

/**
 * ModelCapabilityDialog — 按 (provider, model) 编辑能力位。
 *
 * 三态分段（继承 / 是 / 否）而不是开关：A2A 的语义是「null = 没测过、沿用推断」，
 * 和「明确不可用」是两件事，下游客户端会据此调整行为。
 */
export default function ModelCapabilityDialog({
  model,
  onClose,
  onSaved,
}: ModelCapabilityDialogProps): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const initial = useMemo(() => {
    const caps = model.capabilities ?? [];
    const has = (needle: RegExp): TriState => (caps.length === 0 ? undefined : caps.some((c) => needle.test(c)));
    return {
      supportsToolCall: has(/tool|function/i),
      supportsImages: has(/vision|image/i),
      supportsVideo: has(/video/i),
      supportsReasoning: has(/reason|think/i),
    } as Record<BoolKey, TriState>;
  }, [model]);

  const [tri, setTri] = useState<Record<BoolKey, TriState>>(initial);
  const [maxInput, setMaxInput] = useState('');
  const [maxOutput, setMaxOutput] = useState('');
  /** 未设置（继承）的 token 键不提交；填了值才提交 */
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  const save = async (): Promise<void> => {
    setError('');
    const parseTokens = (raw: string, label: string): number | null | undefined => {
      if (!raw.trim()) return undefined;
      const n = Number(raw.trim());
      if (!Number.isFinite(n) || !Number.isInteger(n) || n < 1 || n > 100_000_000) {
        setError(t('{{label}} 需为 1–100000000 的整数', { label }));
        return null;
      }
      return n;
    };
    const input = parseTokens(maxInput, t('最大输入 tokens'));
    if (input === null) return;
    const output = parseTokens(maxOutput, t('最大输出 tokens'));
    if (output === null) return;

    setSaving(true);
    try {
      const body: Record<string, boolean | number | null> = {};
      for (const key of BOOL_KEYS) {
        const value = tri[key];
        body[key] = value === undefined ? null : value;
      }
      if (input !== undefined) body.maxInputTokens = input;
      if (output !== undefined) body.maxOutputTokens = output;
      await api.setModelCapabilities(model.provider, model.id, body);
      addToast(t('能力声明已保存'));
      onSaved();
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const triSeg = (key: BoolKey): JSX.Element => (
    <div className="mcd-seg" role="group" aria-label={boolLabel(t, key)}>
      {(
        [
          [undefined, t('继承')],
          [true, t('是')],
          [false, t('否')],
        ] as const
      ).map(([value, label]) => (
        <button
          key={String(value)}
          type="button"
          className={`mcd-seg-item${tri[key] === value ? ' is-active' : ''}`}
          onClick={() => setTri((prev) => ({ ...prev, [key]: value }))}
        >
          {label}
        </button>
      ))}
    </div>
  );

  return (
    <div className="mcd-overlay" role="dialog" aria-modal="true" aria-label={t('能力位三态')}>
      <div className="mcd-panel">
        <div className="mcd-head">
          <div>
            <h2>{t('能力位三态')}</h2>
            <p className="mcd-sub">
              <code>{model.provider}</code> · <code>{model.id}</code>
            </p>
          </div>
          <Button variant="outline" size="sm" onClick={onClose}>{t('关闭')}</Button>
        </div>

        <div className="mcd-body">
          <p className="mcd-hint">
            {t('「继承」= 没测过、沿用推断；「是 / 否」= 明确声明。下游客户端会按这里调整行为。')}
          </p>
          {BOOL_KEYS.map((key) => (
            <div key={key} className="mcd-row">
              <span className="mcd-label">{boolLabel(t, key)}</span>
              {triSeg(key)}
            </div>
          ))}
          <div className="mcd-row">
            <span className="mcd-label">{t('最大输入 tokens')}</span>
            <Input
              type="number"
              min={1}
              max={100_000_000}
              value={maxInput}
              onChange={(e) => setMaxInput(e.target.value)}
              placeholder={t('留空 = 继承')}
              style={{ width: 180 }}
            />
          </div>
          <div className="mcd-row">
            <span className="mcd-label">{t('最大输出 tokens')}</span>
            <Input
              type="number"
              min={1}
              max={100_000_000}
              value={maxOutput}
              onChange={(e) => setMaxOutput(e.target.value)}
              placeholder={t('留空 = 继承')}
              style={{ width: 180 }}
            />
          </div>
          {error && <p className="mcd-err">{error}</p>}
        </div>

        <div className="mcd-foot">
          <Button variant="outline" size="sm" onClick={onClose}>{t('取消')}</Button>
          <Button variant="primary" size="sm" disabled={saving} onClick={() => void save()}>
            {saving ? <Loader2 size={12} className="mcd-spin" /> : null}
            {t('保存')}
          </Button>
        </div>
      </div>
    </div>
  );
}