import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import './CostPricingEditor.css';
import { parsePerMillionInput, toPerMillion } from '../lib/pricingUnits';

interface CostPrice {
  input_price?: number;
  output_price?: number;
  price_type?: string;
}

interface CostPricingEditorProps {
  value: Record<string, CostPrice>;
  /** 映射目标模型名列表（来自 model_mapping 的 values）——只对这些展示成本价输入 */
  mappingTargets: string[];
  onChange: (value: Record<string, CostPrice>) => void;
  disabled?: boolean;
}

/**
 * 成本价编辑器 —— **单位：元 / 百万 Token**。
 *
 * 后端存的是每 1K token（`price:xxx` KV，计费公式 `cost = (...)/1000` 直接依赖它），
 * 所以这里只做换算：显示值 = 存储值 × 1000，写回值 = 显示值 ÷ 1000。
 * **后端契约一个字节都没改**，存出去的仍是每 1K。
 *
 * 用户看到的 0.0003 是「每 1K 0.0003 元」，换算成每百万就是 0.3 元——改单位的原因
 * 正是「每 1K 的小数读数看起来像每 token 的分钱数」。
 */
export default function CostPricingEditor(props: CostPricingEditorProps): JSX.Element {
  const { t } = useTranslation();
  // <unique targets, preserve order
  const targets = Array.from(new Set(props.mappingTargets.filter((m) => m.trim() !== '')));

  /**
   * 每行每字段的「界面输入串」。
   *
   * 用本地草稿而不是每次从 `props.value` 反推显示串：键位固定后，用户逐字符输入
   * 「0.」时 `Number('0.')` 为 0，若直接回写 `props.value` 再由它驱动显示，
   * 输入框会一直被规整成 0、打不出小数点。草稿保存原始串，只在 onChange 里提交换算后的每 1K 值。
   */
  const [drafts, setDrafts] = useState<Record<string, Record<'input_price' | 'output_price', string>>>({});

  useEffect(() => {
    // 外部值变化（打开弹窗、重置表单）时按新值刷新草稿
    setDrafts((prev) => {
      let changed = false;
      const next: Record<string, Record<'input_price' | 'output_price', string>> = {};
      for (const m of targets) {
        const p = props.value[m] || {};
        const row: Record<'input_price' | 'output_price', string> = { ...prev[m] };
        if (row.input_price === undefined) row.input_price = p.input_price ? String(toPerMillion(p.input_price)) : '';
        if (row.output_price === undefined) row.output_price = p.output_price ? String(toPerMillion(p.output_price)) : '';
        next[m] = row;
      }
      if (JSON.stringify(next) !== JSON.stringify(prev)) changed = true;
      return changed ? next : prev;
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.value, targets.join('|')]);

  if (targets.length === 0) {
    return (
      <div className="cpe-empty">
        {t('先配置模型映射，再为映射目标填成本价')}
      </div>
    );
  }

  const handleField = (model: string, field: 'input_price' | 'output_price', raw: string): void => {
    setDrafts((prev) => ({ ...prev, [model]: { ...prev[model], [field]: raw } }));
    const stored = parsePerMillionInput(raw);
    const prev = props.value[model] || { input_price: 0, output_price: 0, price_type: 'token' };
    props.onChange({ ...props.value, [model]: { ...prev, [field]: stored } });
  };

  return (
    <div className="cpe-wrap">
      <div className="cpe-row cpe-row-head">
        <div>{t('上游模型')}</div>
        <div>{t('输入价格（元 / 百万 Token）')}</div>
        <div>{t('输出价格（元 / 百万 Token）')}</div>
      </div>
      <div className="cpe-unit-note">
        {t('单位是每百万 Token：填 0.3 即「每百万 0.3 元」，后端仍按每 1K token 存储（显示值已自动换算）。')}
      </div>
      {targets.map((m) => {
        const p = props.value[m] || { input_price: 0, output_price: 0, price_type: 'token' };
        const draft = drafts[m] || {};
        return (
          <div key={m} className="cpe-row">
            <div className="cpe-model-name" title={m}>{m}</div>
            <input
              type="number"
              step="0.0001"
              min="0"
              placeholder="0.3"
              value={draft.input_price ?? (p.input_price ? String(toPerMillion(p.input_price)) : '')}
              onChange={(e) => handleField(m, 'input_price', e.target.value)}
              disabled={props.disabled}
            />
            <input
              type="number"
              step="0.0001"
              min="0"
              placeholder="0.6"
              value={draft.output_price ?? (p.output_price ? String(toPerMillion(p.output_price)) : '')}
              onChange={(e) => handleField(m, 'output_price', e.target.value)}
              disabled={props.disabled}
            />
          </div>
        );
      })}
    </div>
  );
}
