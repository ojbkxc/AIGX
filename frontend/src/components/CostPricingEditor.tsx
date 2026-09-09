import { useTranslation } from 'react-i18next';
import './CostPricingEditor.css';

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

export default function CostPricingEditor(props: CostPricingEditorProps): JSX.Element {
  const { t } = useTranslation();
  // �<unique targets, preserve order
  const targets = Array.from(new Set(props.mappingTargets.filter((m) => m.trim() !== '')));

  if (targets.length === 0) {
    return (
      <div className="cpe-empty">
        {t('先配置模型映射，再为映射目标填成本价')}
      </div>
    );
  }

  const handleField = (model: string, field: 'input_price' | 'output_price', raw: string): void => {
    const n = raw === '' ? 0 : Number(raw);
    const prev = props.value[model] || { input_price: 0, output_price: 0, price_type: 'token' };
    props.onChange({ ...props.value, [model]: { ...prev, [field]: n } });
  };

  return (
    <div className="cpe-wrap">
      <div className="cpe-row cpe-row-head">
        <div>{t('上游模型')}</div>
        <div>{t('输入 / 1k')}</div>
        <div>{t('输出 / 1k')}</div>
      </div>
      {targets.map((m) => {
        const p = props.value[m] || { input_price: 0, output_price: 0, price_type: 'token' };
        return (
          <div key={m} className="cpe-row">
            <div className="cpe-model-name" title={m}>{m}</div>
            <input
              type="number"
              step="0.0001"
              min="0"
              placeholder="0"
              value={p.input_price || ''}
              onChange={(e) => handleField(m, 'input_price', e.target.value)}
              disabled={props.disabled}
            />
            <input
              type="number"
              step="0.0001"
              min="0"
              placeholder="0"
              value={p.output_price || ''}
              onChange={(e) => handleField(m, 'output_price', e.target.value)}
              disabled={props.disabled}
            />
          </div>
        );
      })}
    </div>
  );
}