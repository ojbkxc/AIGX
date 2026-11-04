import React, { useState } from 'react';
import {
  type ColumnSetting,
  type ColumnSettingsState,
  setColumnSettings,
  clearColumnSettings,
} from '../../lib/columnSettings';
import { cn } from '../../lib/utils';

interface ColumnSettingsDrawerProps {
  /** localStorage 键 */
  storageKey: string;
  /** 列定义（用于获取列名） */
  columns: Array<{ key: string; header: React.ReactNode }>;
  /** 抽屉是否打开 */
  open: boolean;
  /** 打开/关闭回调 */
  onOpenChange: (open: boolean) => void;
  /** 修改后回调（通知父组件重渲染） */
  onChange?: (state: ColumnSettingsState) => void;
}

/**
 * 列设置抽屉 — 列显隐 / 顺序 / 对齐。
 * 顺序通过上下移动按钮调整；显隐通过勾选框；对齐通过三态按钮。
 */
export default function ColumnSettingsDrawer({
  storageKey,
  columns,
  open,
  onOpenChange,
  onChange,
}: ColumnSettingsDrawerProps) {
  const initial = React.useMemo(() => {
    const stored = getColumnSettingsSafe(storageKey);
    if (stored) return stored;
    return {
      columns: columns.map((c) => ({
        key: c.key,
        visible: true,
        align: 'left' as const,
      })),
    };
  }, [storageKey]); // eslint-disable-line react-hooks/exhaustive-deps

  const [items, setItems] = useState<ColumnSetting[]>(initial.columns);

  // 外部 columns 变化时（如新增列），同步新列到设置里
  React.useEffect(() => {
    setItems((prev) => {
      const existingKeys = new Set(prev.map((i) => i.key));
      const newCols = columns.filter((c) => !existingKeys.has(c.key));
      if (newCols.length === 0) return prev;
      return [...prev, ...newCols.map((c) => ({ key: c.key, visible: true, align: 'left' as const }))];
    });
  }, [columns]);

  const persist = (next: ColumnSetting[]) => {
    const state: ColumnSettingsState = { columns: next };
    setColumnSettings(storageKey, state);
    onChange?.(state);
  };

  const move = (idx: number, dir: -1 | 1) => {
    const target = idx + dir;
    if (target < 0 || target >= items.length) return;
    const next = [...items];
    [next[idx], next[target]] = [next[target], next[idx]];
    setItems(next);
    persist(next);
  };

  const toggleVisible = (idx: number) => {
    const next = [...items];
    next[idx].visible = !next[idx].visible;
    setItems(next);
    persist(next);
  };

  const setAlign = (idx: number, align: 'left' | 'center' | 'right') => {
    const next = [...items];
    next[idx].align = align;
    setItems(next);
    persist(next);
  };

  const reset = () => {
    clearColumnSettings(storageKey);
    const defaults = columns.map((c) => ({ key: c.key, visible: true, align: 'left' as const }));
    setItems(defaults);
    onChange?.({ columns: defaults });
  };

  if (!open) return null;

  return (
    <div className="column-settings-overlay" onClick={() => onOpenChange(false)}>
      <div className="column-settings-drawer" onClick={(e) => e.stopPropagation()}>
        <div className="column-settings-header">
          <h3>列设置</h3>
          <button className="btn-close" onClick={() => onOpenChange(false)}>✕</button>
        </div>
        <div className="column-settings-body">
          {items.map((item, idx) => {
            const col = columns.find((c) => c.key === item.key);
            return (
              <div key={item.key} className="column-settings-row">
                <label className="column-settings-label">
                  <input
                    type="checkbox"
                    checked={item.visible}
                    onChange={() => toggleVisible(idx)}
                  />
                  <span>{col?.header ?? item.key}</span>
                </label>
                <div className="column-settings-controls">
                  <div className="column-settings-align">
                    {(['left', 'center', 'right'] as const).map((a) => (
                      <button
                        key={a}
                        className={cn('btn-align', item.align === a && 'active')}
                        onClick={() => setAlign(idx, a)}
                        title={`${a} 对齐`}
                      >
                        {a === 'left' ? '左' : a === 'center' ? '中' : '右'}
                      </button>
                    ))}
                  </div>
                  <div className="column-settings-move">
                    <button
                      className="btn-move"
                      onClick={() => move(idx, -1)}
                      disabled={idx === 0}
                      title="上移"
                    >
                      ↑
                    </button>
                    <button
                      className="btn-move"
                      onClick={() => move(idx, 1)}
                      disabled={idx === items.length - 1}
                      title="下移"
                    >
                      ↓
                    </button>
                  </div>
                </div>
              </div>
            );
          })}
        </div>
        <div className="column-settings-footer">
          <button className="btn-secondary" onClick={reset}>重置默认</button>
        </div>
      </div>
    </div>
  );
}

/** 内部读取，带 try/catch 保护 */
function getColumnSettingsSafe(key: string): ColumnSettingsState | null {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (
      parsed &&
      typeof parsed === 'object' &&
      !Array.isArray(parsed) &&
      Array.isArray((parsed as { columns?: unknown }).columns)
    ) {
      return parsed as ColumnSettingsState;
    }
    return null;
  } catch {
    return null;
  }
}
