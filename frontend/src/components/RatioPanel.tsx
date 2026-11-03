import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Plus, Trash2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { Button, Combobox, SkeletonList } from '../components/ui';
import type { ManageView, GroupItem } from '../types';
import './RatioPanel.css';

/**
 * 倍率配置面板（模型倍率 / 分组倍率）。
 *
 * 从原 `/pricing` 独立页抽出来的可嵌入组件，作为「模型管理」的第三个 Tab：
 * 逐模型价格、别名、倍率都是同一条模型记录的不同侧面，分散在两页会让人
 * 在两处各改一遍。汇率与价格同步仍留在「系统设置 → 计费」。
 *
 * 交互沿用原页：主视图是表格（名称/倍率/增删行），保存时前端组装成
 * JSON 调既有 ratios 接口 —— 后端零改动。JSON 文本框保留为「高级模式」，
 * 供批量粘贴大批量倍率时使用。
 *
 * 候选来源：模型目录 ∪ 全局别名 ∪ 价格目录 ∪ 已配置倍率键（大小写不敏感去重）。
 */

interface RatiosState {
  model_ratio: Record<string, number>;
  group_ratio: Record<string, number>;
}

/** 表格里的一行（字符串态，提交时解析） */
interface RatioRow {
  key: string;
  value: string;
}

const toRows = (map: Record<string, number>): RatioRow[] =>
  Object.entries(map)
    .map(([key, value]) => ({ key, value: String(value) }))
    .sort((a, b) => a.key.localeCompare(b.key));

const fromRows = (rows: RatioRow[]): Record<string, number> => {
  const out: Record<string, number> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (!key) continue;
    const value = Number(row.value);
    if (!Number.isFinite(value)) continue;
    out[key] = value;
  }
  return out;
};

interface RatioTableProps {
  title: string;
  namePlaceholder: string;
  hint: string;
  rows: RatioRow[];
  err: string | null;
  options: string[];
  onChange: (index: number, patch: Partial<RatioRow>) => void;
  onAdd: () => void;
  onRemove: (index: number) => void;
}

/** 表格编辑区（模型/分组共用） */
function RatioTable({
  title,
  namePlaceholder,
  hint,
  rows,
  err,
  options,
  onChange,
  onAdd,
  onRemove,
}: RatioTableProps): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="form-group">
      <div className="ratio-panel-head">
        <label>{title}</label>
        <Button variant="outline" size="sm" onClick={onAdd} style={{ gap: 4 }}>
          <Plus size={12} /> {t('新增')}
        </Button>
      </div>
      {rows.length === 0 ? (
        <p className="ratio-hint ratio-panel-empty">
          {t('暂无条目，点「新增」添加。缺省倍率为 1.0。')}
        </p>
      ) : (
        <div className="table-wrapper ratio-panel-table">
          <table>
            <thead>
              <tr>
                <th>{t('名称')}</th>
                <th style={{ width: 140 }}>{t('倍率')}</th>
                <th style={{ width: 60 }} />
              </tr>
            </thead>
            <tbody>
              {rows.map((row, i) => (
                <tr key={i}>
                  <td>
                    <Combobox
                      value={row.key}
                      placeholder={namePlaceholder}
                      options={options}
                      disabledOptions={rows
                        .filter((_, j) => j !== i)
                        .map((r) => r.key.trim())
                        .filter(Boolean)}
                      onChange={(v) => onChange(i, { key: v })}
                    />
                  </td>
                  <td>
                    <input
                      className="form-input"
                      value={row.value}
                      inputMode="decimal"
                      style={err ? { borderColor: 'rgb(239,68,68)' } : undefined}
                      onChange={(e) => onChange(i, { value: e.target.value })}
                    />
                  </td>
                  <td>
                    <Button
                      variant="danger"
                      size="sm"
                      title={t('删除')}
                      onClick={() => onRemove(i)}
                    >
                      <Trash2 size={12} />
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <span className="form-hint">{hint}</span>
      {err && (
        <span className="form-hint ratio-panel-err">{err}</span>
      )}
    </div>
  );
}

export default function RatioPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();

  const [error, setError] = useState('');
  const [ratioLoading, setRatioLoading] = useState(true);
  const [savingRatios, setSavingRatios] = useState(false);

  // 表格主视图
  const [modelRows, setModelRows] = useState<RatioRow[]>([]);
  const [groupRows, setGroupRows] = useState<RatioRow[]>([]);
  const [modelError, setModelError] = useState<string | null>(null);
  const [groupError, setGroupError] = useState<string | null>(null);

  // 高级模式：JSON 文本
  const [advanced, setAdvanced] = useState(false);
  const [ratioText, setRatioText] = useState('{}');
  const [groupRatioText, setGroupRatioText] = useState('{}');

  // 候选数据：模型目录视图（含别名映射）、价格目录、用户分组
  const [manageView, setManageView] = useState<ManageView | null>(null);
  const [priceKeys, setPriceKeys] = useState<string[]>([]);
  const [groups, setGroups] = useState<GroupItem[]>([]);

  useEffect(() => {
    void loadRatios();
    void loadOptions();
    // 只在挂载时拉一次；保存后由 handleSaveRatios 自行回读
  }, []);

  /** 拉取下拉候选：失败静默（候选为空时仍可自由输入） */
  const loadOptions = async () => {
    try {
      const res = await api.listManageModels();
      if (res?.data) setManageView(res.data);
    } catch { /* 候选加载失败静默 */ }
    try {
      const res = await api.listPrices();
      const raw = Array.isArray(res) ? res : (res as { data?: Array<{ model?: string }> })?.data;
      if (Array.isArray(raw)) {
        setPriceKeys(raw.map((p) => p?.model).filter((v): v is string => Boolean(v)));
      }
    } catch { /* 候选加载失败静默 */ }
    try {
      const res = await api.listGroups();
      const raw = Array.isArray(res) ? res : (res as { data?: GroupItem[] })?.data;
      if (Array.isArray(raw)) setGroups(raw);
    } catch { /* 候选加载失败静默 */ }
  };

  const loadRatios = async () => {
    setRatioLoading(true);
    try {
      const res = await api.getRatios();
      const r = (res?.data ?? {}) as unknown as RatiosState;
      const model = r.model_ratio || {};
      const group = r.group_ratio || {};
      setModelRows(toRows(model));
      setGroupRows(toRows(group));
      setRatioText(JSON.stringify(model, null, 2));
      setGroupRatioText(JSON.stringify(group, null, 2));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setRatioLoading(false);
    }
  };

  /** 行编辑通用更新 */
  const updateRow = (
    which: 'model' | 'group',
    index: number,
    patch: Partial<RatioRow>,
  ): void => {
    const setter = which === 'model' ? setModelRows : setGroupRows;
    setter((rows) => rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
    if (which === 'model') setModelError(null);
    else setGroupError(null);
  };

  const addRow = (which: 'model' | 'group'): void => {
    (which === 'model' ? setModelRows : setGroupRows)((rows) => [
      ...rows,
      { key: '', value: '1' },
    ]);
  };

  const removeRow = (which: 'model' | 'group', index: number): void => {
    (which === 'model' ? setModelRows : setGroupRows)((rows) =>
      rows.filter((_, i) => i !== index),
    );
  };

  /** 模型倍率候选：模型目录 ∪ 全局别名 ∪ 价格目录 ∪ 已配置倍率键（大小写不敏感去重） */
  const modelOptions = useMemo(() => {
    const seen = new Map<string, string>();
    const push = (name?: string) => {
      const n = name?.trim();
      if (!n) return;
      const k = n.toLowerCase();
      if (!seen.has(k)) seen.set(k, n);
    };
    manageView?.models.forEach((m) => {
      push(m.id);
      m.aliases.forEach(push);
    });
    Object.keys(manageView?.mappings ?? {}).forEach(push);
    priceKeys.forEach(push);
    modelRows.forEach((r) => push(r.key));
    return [...seen.values()].sort((a, b) => a.localeCompare(b));
  }, [manageView, priceKeys, modelRows]);

  /** 分组倍率候选：用户分组列表 ∪ 已配置分组键 */
  const groupOptions = useMemo(() => {
    const seen = new Map<string, string>();
    const push = (name?: string) => {
      const n = name?.trim();
      if (!n) return;
      const k = n.toLowerCase();
      if (!seen.has(k)) seen.set(k, n);
    };
    groups.forEach((g) => push(g.name));
    groupRows.forEach((r) => push(r.key));
    return [...seen.values()].sort((a, b) => a.localeCompare(b));
  }, [groups, groupRows]);

  /** 校验一行：key 非空且不重复（大小写不敏感）、value 是正数 */
  const validate = (which: 'model' | 'group', rows: RatioRow[]): boolean => {
    const keys = rows.map((r) => r.key.trim().toLowerCase()).filter(Boolean);
    const dup = keys.some((k, i) => keys.indexOf(k) !== i);
    const bad = rows.some((r) => {
      if (!r.key.trim()) return r.value.trim() !== ''; // 空 key + 空 value = 待删占位行，不算错
      const v = Number(r.value);
      return !Number.isFinite(v) || v < 0;
    });
    if (dup || bad) {
      const msg = dup ? t('名称不能重复') : t('倍率必须是非负数字');
      if (which === 'model') setModelError(msg);
      else setGroupError(msg);
      return false;
    }
    return true;
  };

  const handleSaveRatios = async (): Promise<void> => {
    setSavingRatios(true);
    setError('');
    let modelRatio: Record<string, number>;
    let groupRatio: Record<string, number>;
    if (advanced) {
      try {
        modelRatio = JSON.parse(ratioText || '{}');
      } catch {
        setModelError(t('模型倍率 JSON 格式错误'));
        setSavingRatios(false);
        return;
      }
      try {
        groupRatio = JSON.parse(groupRatioText || '{}');
      } catch {
        setGroupError(t('分组倍率 JSON 格式错误'));
        setSavingRatios(false);
        return;
      }
    } else {
      if (!validate('model', modelRows) || !validate('group', groupRows)) {
        setSavingRatios(false);
        return;
      }
      modelRatio = fromRows(modelRows);
      groupRatio = fromRows(groupRows);
    }
    try {
      // payload 与后端 RatioConfig 对齐
      await api.updateRatios({ model_ratio: modelRatio, group_ratio: groupRatio });
      addToast(t('倍率配置已保存'));
      void loadRatios();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingRatios(false);
    }
  };

  /** 切到高级模式时，用当前表格内容刷新 JSON 文本 */
  const enterAdvanced = (): void => {
    setRatioText(JSON.stringify(fromRows(modelRows), null, 2));
    setGroupRatioText(JSON.stringify(fromRows(groupRows), null, 2));
    setAdvanced(true);
  };

  /** 切回表格模式时，解析 JSON；失败则留在高级模式并提示 */
  const leaveAdvanced = (): void => {
    let model: Record<string, number>;
    let group: Record<string, number>;
    try {
      model = JSON.parse(ratioText || '{}');
      group = JSON.parse(groupRatioText || '{}');
    } catch {
      addToast(t('JSON 格式错误，请先修正再切回表格模式'), 'error');
      return;
    }
    setModelRows(toRows(model));
    setGroupRows(toRows(group));
    setAdvanced(false);
  };

  if (ratioLoading) {
    return (
      <div className="mm-panel ratio-panel">
        <SkeletonList rows={4} />
      </div>
    );
  }

  return (
    <div className="mm-panel ratio-panel">
      <div className="mm-titlebar">
        <div className="mm-title">
          <span className="mm-title-name">{t('定价倍率')}</span>
          <span className="mm-meta">
            {t('最终费用 = 基础费用 × 模型倍率 × 分组倍率。缺省倍率为 1.0。')}
          </span>
        </div>
        <div className="mm-head-actions">
          <Button variant="outline" size="sm" onClick={advanced ? leaveAdvanced : enterAdvanced}>
            {advanced ? t('切换到表格模式') : t('JSON 高级模式')}
          </Button>
          <Button onClick={() => void handleSaveRatios()} disabled={savingRatios}>
            {savingRatios ? t('保存中...') : t('保存倍率配置')}
          </Button>
        </div>
      </div>

      {error && <div className="error-message">{error}</div>}

      {advanced ? (
        <div className="ratio-grid">
          <div className="form-group">
            <label>{t('模型倍率 (JSON)')}</label>
            <textarea
              className="form-input ratio-textarea"
              rows={10}
              value={ratioText}
              onChange={(e) => {
                setRatioText(e.target.value);
                setModelError(null);
              }}
              style={modelError ? { borderColor: 'rgb(239,68,68)' } : undefined}
              placeholder='{"glm-5.2": 1, "deepseek-v3": 0.5}'
            />
            {modelError && <span className="form-hint ratio-panel-err">{modelError}</span>}
          </div>
          <div className="form-group">
            <label>{t('分组倍率 (JSON)')}</label>
            <textarea
              className="form-input ratio-textarea"
              rows={10}
              value={groupRatioText}
              onChange={(e) => {
                setGroupRatioText(e.target.value);
                setGroupError(null);
              }}
              style={groupError ? { borderColor: 'rgb(239,68,68)' } : undefined}
              placeholder='{"default": 1, "vip": 0.8}'
            />
            {groupError && <span className="form-hint ratio-panel-err">{groupError}</span>}
          </div>
        </div>
      ) : (
        <div className="ratio-grid">
          <RatioTable
            title={t('模型倍率')}
            namePlaceholder="gpt-4o"
            hint={t(
              '模型名/别名 → 倍率。候选来自模型目录、别名与价格目录；单个模型的倍率也可在模型目录的定价弹窗里改。'
            )}
            rows={modelRows}
            err={modelError}
            options={modelOptions}
            onChange={(i, patch) => updateRow('model', i, patch)}
            onAdd={() => addRow('model')}
            onRemove={(i) => removeRow('model', i)}
          />
          <RatioTable
            title={t('分组倍率')}
            namePlaceholder="vip"
            hint={t('分组名 → 倍率。例如 vip: 0.8 表示 VIP 组用户打 8 折。')}
            rows={groupRows}
            err={groupError}
            options={groupOptions}
            onChange={(i, patch) => updateRow('group', i, patch)}
            onAdd={() => addRow('group')}
            onRemove={(i) => removeRow('group', i)}
          />
        </div>
      )}
    </div>
  );
}
