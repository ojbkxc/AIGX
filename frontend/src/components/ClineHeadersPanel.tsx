import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import { useToast } from './Toast';

/**
 * Cline 伪装头覆盖表（系统设置 → 运维 → 网关 → Cline 伪装头）
 *
 * 对照 Agent2API「设置 → 网关 → Cline 伪装头」：管理发往 Cline 上游的
 * 伪装请求头逐键覆盖。默认值取自代码内的官方客户端形态模板（上游收紧时
 * 跟着版本走），界面只把「与默认不同」的键提交为覆盖表：
 * - 与默认相同 → 不进覆盖表（回落默认）；
 * - 值清空 → 覆盖为空串（这个头不发）；
 * - 删除行 → 该键从覆盖表去掉（若原来是默认头则回落默认值）；
 * - 新增键 → 自定义头。
 * Authorization / Content-Type 是网关固定头，后端 400 拒绝，界面不提供新增。
 * 保存后下一个 Cline 请求即生效，无需重启。
 */

interface RowState {
  key: string;
  value: string;
}

/** 由 defaults + overrides 重建编辑行（键并集；值取覆盖值，缺省回落默认） */
function buildRows(defs: Record<string, string>, overrides: Record<string, string>): RowState[] {
  const keys = Array.from(new Set([...Object.keys(defs), ...Object.keys(overrides)]));
  return keys.map((k) => ({ key: k, value: overrides[k] ?? defs[k] ?? '' }));
}

export default function ClineHeadersPanel(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [defaults, setDefaults] = useState<Record<string, string>>({});
  const [rows, setRows] = useState<RowState[]>([]);
  const [newKey, setNewKey] = useState('');

  const applyData = (defs: Record<string, string>, overrides: Record<string, string>): void => {
    setDefaults(defs);
    setRows(buildRows(defs, overrides));
  };

  const load = async (): Promise<void> => {
    setLoading(true);
    setError('');
    try {
      const res = await api.getClineHeaders();
      applyData(res?.data?.defaults ?? {}, res?.data?.overrides ?? {});
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const updateRow = (idx: number, value: string): void => {
    setRows(rows.map((r, i) => (i === idx ? { ...r, value } : r)));
  };

  const removeRow = (idx: number): void => {
    setRows(rows.filter((_, i) => i !== idx));
  };

  const addRow = (): void => {
    const k = newKey.trim();
    if (!k) return;
    if (rows.some((r) => r.key.toLowerCase() === k.toLowerCase())) {
      setError(t('该头名已存在'));
      return;
    }
    setRows([...rows, { key: k, value: '' }]);
    setNewKey('');
  };

  const save = async (): Promise<void> => {
    setSaving(true);
    setError('');
    try {
      // 只提交「与默认不同」的行：空串 = 不发该头；等同默认 = 回落默认
      const overrides: Record<string, string> = {};
      for (const r of rows) {
        if ((defaults[r.key] ?? '') !== r.value) overrides[r.key] = r.value;
      }
      const res = await api.saveClineHeaders(overrides);
      applyData(res?.data?.defaults ?? {}, res?.data?.overrides ?? {});
      addToast(t('Cline 伪装头已保存（下一个 Cline 请求即生效）'));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div className="loading">{t('加载中')}</div>;
  }

  return (
    <div className="card">
      <div className="card-header">
        <h2>{t('Cline 伪装头')}</h2>
        <p className="card-subtitle">
          {t('发往 Cline 上游的伪装请求头逐键覆盖：默认值对齐官方客户端形态；值清空 = 不发该头，删除行 = 回落默认。Authorization 与 Content-Type 为网关固定头，不可覆盖。保存后下一个请求即生效。')}
        </p>
      </div>
      <div className="card-body">
        {error && <div className="error-message">{error}</div>}
        <div className="settings-form">
          {rows.map((r, i) => {
            const isDefault = defaults[r.key] != null;
            const dirty = (defaults[r.key] ?? '') !== r.value;
            return (
              <div
                className="form-group"
                key={r.key}
                style={{ display: 'flex', gap: 8, alignItems: 'flex-start' }}
              >
                <div style={{ flex: '0 0 220px' }}>
                  <label>{isDefault ? r.key : `${r.key} ${t('(自定义)')}`}</label>
                  {isDefault && (
                    <div className="form-hint">
                      {`${t('默认')}: ${defaults[r.key]}`}
                    </div>
                  )}
                </div>
                <input
                  className="form-input"
                  style={{ flex: 1 }}
                  value={r.value}
                  placeholder={isDefault ? defaults[r.key] : t('头值；清空 = 不发该头')}
                  onChange={(e) => updateRow(i, e.target.value)}
                />
                {dirty && <span style={{ color: '#b8860b' }}>●</span>}
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => removeRow(i)}
                >
                  {t('删除')}
                </button>
              </div>
            );
          })}
          <div className="form-group" style={{ display: 'flex', gap: 8 }}>
            <input
              className="form-input"
              style={{ flex: '0 0 220px' }}
              placeholder={t('新增头名，如 x-custom-hint')}
              value={newKey}
              onChange={(e) => setNewKey(e.target.value)}
            />
            <button
              type="button"
              className="btn btn-secondary"
              onClick={addRow}
              disabled={!newKey.trim()}
            >
              {t('新增')}
            </button>
          </div>
          <div className="settings-actions" style={{ display: 'flex', gap: 8 }}>
            <button className="btn btn-primary" onClick={() => void save()} disabled={saving}>
              {saving ? t('保存中...') : t('保存更改')}
            </button>
            <button className="btn btn-secondary" onClick={() => void load()} disabled={saving}>
              {t('放弃修改')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}