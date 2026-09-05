import { useState } from 'react';
import { api } from '../api';
import type { TokenStats, Limits, NotifyConfig } from './types';

export default function Settings(): JSX.Element {
  const [usage] = useState<TokenStats | null>(null);
  const [limits, setLimits] = useState<Limits | null>(null);
  const [saving, setSaving] = useState(false);
  const [notification] = useState<NotifyConfig | null>(null);

  const handleSave = async () => {
    setSaving(true);
    try {
      await api.saveSettings(usage, limits, notification);
    } catch (err) {
      console.error('保存失败:', err);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div>
      <div className="page-header">
        <h1>设置</h1>
        <p>配置系统和通知选项</p>
      </div>

      {/* 使用限额 */}
      <div className="settings-section">
        <h2>使用限额</h2>
        <div className="form-group">
          <label>每月限额</label>
          <input
            type="number"
            value={limits?.monthly_limit || ''}
            onChange={(e) => setLimits({ ...(limits ?? { monthly_used: 0, monthly_limit: 0 }), monthly_limit: Number(e.target.value) })}
          />
        </div>
        <div className="form-group">
          <label>已用额度</label>
          <input
            type="number"
            value={limits?.monthly_used || ''}
            readOnly
          />
        </div>
      </div>

      {/* 点击保存 */}
      <button onClick={handleSave} disabled={saving}>
        {saving ? '保存中...' : '保存设置'}
      </button>
    </div>
  );
}
