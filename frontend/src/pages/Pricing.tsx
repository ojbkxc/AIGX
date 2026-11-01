import { useState, useEffect, useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { Plus, Trash2 } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { SkeletonList, Combobox } from '../components/ui';
import type { ManageView, GroupItem } from '../types';
import './Pricing.css';

/**
 * 定价倍率页 —— 只保留「倍率配置」。
 *
 * 模型价格目录已并入「模型管理」（`/model-manage`）：逐模型定价在那里与
 * 启停、别名放在一起，避免同一份数据在两页各改一遍。这里剩下的两项
 * （模型倍率 / 分组倍率）是**全局**参数，与具体模型无关，仍留在本页；
 * 汇率与价格同步在「系统设置 → 计费」。
 *
 * 交互：主视图是表格（模型名/倍率/增删行），保存时前端组装成 JSON 调
 * 既有 ratios 接口 —— 后端零改动。JSON 文本框保留为「高级模式」折叠项，
 * 供批量粘贴大批量倍率时使用。
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

export default function Pricing() {
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
  ) => {
    const setter = which === 'model' ? setModelRows : setGroupRows;
    setter((rows) => rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
    if (which === 'model') setModelError(null);
    else setGroupError(null);
  };

  const addRow = (which: 'model' | 'group') => {
    (which === 'model' ? setModelRows : setGroupRows)((rows) => [
      ...rows,
      { key: '', value: '1' },
    ]);
  };

  const removeRow = (which: 'model' | 'group', index: number) => {
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

  const handleSaveRatios = async () => {
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
  const enterAdvanced = () => {
    setRatioText(JSON.stringify(fromRows(modelRows), null, 2));
    setGroupRatioText(JSON.stringify(fromRows(groupRows), null, 2));
    setAdvanced(true);
  };

  /** 切回表格模式时，解析 JSON；失败则留在高级模式并提示 */
  const leaveAdvanced = () => {
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

  /** 表格编辑区（模型/分组共用） */
  const ratioTable = (
    which: 'model' | 'group',
    rows: RatioRow[],
    err: string | null,
    nameHint: string,
    placeholder: string,
    options: string[],
  ) => (
    <div className="form-group">
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <label>{which === 'model' ? t('模型倍率') : t('分组倍率')}</label>
        <button
          type="button"
          className="btn btn-outline btn-sm"
          onClick={() => addRow(which)}
          style={{ gap: 4 }}
        >
          <Plus size={12} /> {t('新增')}
        </button>
      </div>
      {rows.length === 0 ? (
        <p className="ratio-hint" style={{ margin: '8px 0' }}>
          {t('暂无条目，点「新增」添加。缺省倍率为 1.0。')}
        </p>
      ) : (
        <div className="table-wrapper" style={{ maxHeight: 360, overflowY: 'auto' }}>
          <table>
            <thead>
              <tr>
                <th>{which === 'model' ? t('模型名') : t('分组名')}</th>
                <th style={{ width: 140 }}>{t('倍率')}</th>
                <th style={{ width: 60 }}></th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row, i) => (
                <tr key={i}>
                  <td>
                    <Combobox
                      value={row.key}
                      placeholder={placeholder}
                      options={options}
                      disabledOptions={rows
                        .filter((_, j) => j !== i)
                        .map((r) => r.key.trim())
                        .filter(Boolean)}
                      onChange={(v) => updateRow(which, i, { key: v })}
                    />
                  </td>
                  <td>
                    <input
                      className="form-input"
                      value={row.value}
                      inputMode="decimal"
                      style={err ? { borderColor: 'rgb(239,68,68)' } : undefined}
                      onChange={(e) => updateRow(which, i, { value: e.target.value })}
                    />
                  </td>
                  <td>
                    <button
                      type="button"
                      className="btn btn-danger btn-sm"
                      title={t('删除')}
                      onClick={() => removeRow(which, i)}
                    >
                      <Trash2 size={12} />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <span className="form-hint">{nameHint}</span>
      {err && (
        <span className="form-hint" style={{ color: 'rgb(239,68,68)' }}>
          {err}
        </span>
      )}
    </div>
  );

  return (
    <div className="pricing-shell">
      <div className="page-header">
        <div>
          <h1>{t('定价倍率')}</h1>
          <p>{t('全局倍率配置。逐模型的输入/输出价格请到「模型管理」里编辑。')}</p>
        </div>
      </div>

      <div className="notify-note" style={{ fontSize: 12.5, lineHeight: 1.7 }}>
        {t('模型价格目录已并入模型管理页。')}{' '}
        <Link to="/model-manage">{t('前往模型管理编辑价格')}</Link>
      </div>

      {error && <div className="error-message">{error}</div>}

      <div className="pricing-content">
        <div className="card">
          <div className="card-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <h2>{t('倍率配置')}</h2>
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={advanced ? leaveAdvanced : enterAdvanced}
            >
              {advanced ? t('切换到表格模式') : t('JSON 高级模式')}
            </button>
          </div>
          <div className="card-body">
            {ratioLoading ? (
              <SkeletonList rows={3} />
            ) : (
              <>
                <p className="ratio-hint">
                  {t('最终费用 = 基础费用 × 模型倍率 × 分组倍率。缺省倍率为 1.0。')}
                </p>
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
                      {modelError && (
                        <span className="form-hint" style={{ color: 'rgb(239,68,68)' }}>
                          {modelError}
                        </span>
                      )}
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
                      {groupError && (
                        <span className="form-hint" style={{ color: 'rgb(239,68,68)' }}>
                          {groupError}
                        </span>
                      )}
                    </div>
                  </div>
                ) : (
                  <div className="ratio-grid">
                    {ratioTable(
                      'model',
                      modelRows,
                      modelError,
                      t('模型名/别名 → 倍率。候选来自模型目录、别名与价格目录；单个模型的倍率也可在模型管理的定价弹窗里改。'),
                      'gpt-4o',
                      modelOptions,
                    )}
                    {ratioTable(
                      'group',
                      groupRows,
                      groupError,
                      t('分组名 → 倍率。例如 vip: 0.8 表示 VIP 组用户打 8 折。'),
                      'vip',
                      groupOptions,
                    )}
                  </div>
                )}

                <div style={{ marginTop: 16 }}>
                  <button
                    className="btn btn-primary"
                    onClick={handleSaveRatios}
                    disabled={savingRatios}
                  >
                    {savingRatios ? t('保存中...') : t('保存倍率配置')}
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
