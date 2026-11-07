/**
 * AIGX · 公共列设置面板（React 版，对齐 Agent2API table-col-settings.tsx 功能集）。
 *
 * - 行结构：拖拽把手 / 列名 / 显隐开关 / 对齐三档（共用一套 grid）
 * - 指针拖拽重排（不用 HTML5 拖放；每跨一行重排，松手才落盘）
 * - 锁定列：开关禁用（恒显示），把手仍可拖序
 * - 恢复默认：顺序 + 显隐 + 对齐 + 列宽一起回列声明
 * - 锚点浮层：贴触发按钮右下（align=end / side=bottom），窄屏改视口锚
 *
 * 状态由页面持有（受控组件）：本面板只出事件，落盘时机由页面定
 * （开关/对齐即时落盘；拖序松手落盘）——与 ModelManage 既有交互口径一致。
 */
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { GripVertical } from 'lucide-react';
import type { TableColumnAlign, TableColumnDef, TableColumnItem } from '../../lib/tableColumns';

const ALIGN_OPTIONS: Array<{ value: TableColumnAlign; label: string }> = [
  { value: 'left', label: '左' },
  { value: 'center', label: '中' },
  { value: 'right', label: '右' },
];

export interface TableColumnSettingsProps {
  open: boolean;
  onClose: () => void;
  /** 列声明（键集合与默认值的唯一事实源） */
  defs: TableColumnDef[];
  /** 受控状态（页面持有） */
  items: TableColumnItem[];
  /** 任意变更（面板内即时回调；persist 提示调用方是否应落盘） */
  onChange: (next: TableColumnItem[], persist: boolean) => void;
  /** 恢复默认 */
  onReset: () => void;
  /** 触发按钮 ref 所在容器（浮层贴它定位；缺省贴面板父级） */
  anchorRef?: React.RefObject<HTMLElement | null>;
}

export default function TableColumnSettings({
  open,
  onClose,
  defs,
  items,
  onChange,
  onReset,
  anchorRef,
}: TableColumnSettingsProps): JSX.Element | null {
  const { t } = useTranslation();
  const panelRef = useRef<HTMLDivElement | null>(null);
  const rowsRef = useRef<HTMLDivElement | null>(null);
  const dragCleanup = useRef<(() => void) | null>(null);
  const [draggingKey, setDraggingKey] = useState<string | null>(null);

  const defByKey = new Map(defs.map((d) => [d.key, d]));
  const labelOf = (key: string): string => defByKey.get(key)?.label ?? key;

  // 点外面收面板（mousedown 口径与 ModelManage 既有面板一致）
  useEffect(() => {
    if (!open) return undefined;
    const onDown = (e: MouseEvent): void => {
      const anchor = anchorRef?.current;
      if (
        panelRef.current &&
        !panelRef.current.contains(e.target as Node) &&
        !(anchor && anchor.contains(e.target as Node))
      ) {
        onClose();
      }
    };
    window.addEventListener('mousedown', onDown);
    return () => window.removeEventListener('mousedown', onDown);
  }, [open, onClose, anchorRef]);

  // 收起/卸载时把拖拽会话收干净并补写盘（防内存与 localStorage 漂移）
  const finishDrag = (): void => {
    if (!dragCleanup.current) return;
    dragCleanup.current();
  };
  useEffect(() => {
    if (open) return undefined;
    finishDrag();
    return undefined;
  }, [open]);
  useEffect(() => () => finishDrag(), []);

  if (!open) return null;

  const toggleVisible = (key: string): void => {
    const def = defByKey.get(key);
    if (def?.locked) return; // 锁定列不可关
    onChange(
      items.map((i) => (i.key === key ? { ...i, visible: !i.visible } : i)),
      true,
    );
  };

  const setAlign = (key: string, align: TableColumnAlign): void => {
    const item = items.find((i) => i.key === key);
    if (!item || item.align === align) return;
    onChange(items.map((i) => (i.key === key ? { ...i, align } : i)), true);
  };

  /** 拖动排序：指针事件；每跨一行重排不落盘，松手（pointerup）落盘 */
  const startDrag = (key: string, e: React.PointerEvent<HTMLElement>): void => {
    e.preventDefault();
    dragCleanup.current?.();
    setDraggingKey(key);
    document.body.classList.add('tbl-colset-dragging');
    const rows = rowsRef.current;
    const teardown = (): void => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      document.body.classList.remove('tbl-colset-dragging');
      dragCleanup.current = null;
      setDraggingKey(null);
    };
    const onMove = (ev: PointerEvent): void => {
      const over = document.elementFromPoint(ev.clientX, ev.clientY)?.closest('[data-col-key]');
      if (!over || !rows || !rows.contains(over)) return;
      const overKey = over.getAttribute('data-col-key');
      if (!overKey || overKey === key) return;
      onChange(
        (() => {
          const from = items.findIndex((c) => c.key === key);
          const to = items.findIndex((c) => c.key === overKey);
          if (from < 0 || to < 0) return items;
          const next = [...items];
          const [moved] = next.splice(from, 1);
          next.splice(to, 0, moved);
          return next;
        })(),
        false, // 拖动过程不落盘
      );
    };
    const onUp = (): void => {
      teardown();
      onChange(items, true); // 松手落盘（items 为最新闭包外值时由页面以函数式兜底）
    };
    dragCleanup.current = teardown;
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  };

  return (
    <div className="tbl-colset" ref={panelRef} role="group" aria-label={t('列设置')}>
      <div className="tbl-colset-grid tbl-colset-head">
        <span />
        <span>{t('列名')}</span>
        <span>{t('显示')}</span>
        <span>{t('对齐')}</span>
      </div>
      <div className="tbl-colset-rows" ref={rowsRef}>
        {items.map((item) => {
          const def = defByKey.get(item.key);
          const locked = def?.locked === true;
          return (
            <div
              key={item.key}
              data-col-key={item.key}
              className={`tbl-colset-grid tbl-colset-row${draggingKey === item.key ? ' dragging' : ''}`}
            >
              <span
                className="tbl-colset-grip"
                onPointerDown={(e) => startDrag(item.key, e)}
                title={t('拖动调整顺序')}
                aria-label={`${t('拖动调整顺序')}：${labelOf(item.key)}`}
              >
                <GripVertical size={14} />
              </span>
              <span className="tbl-colset-name" title={labelOf(item.key)}>
                {labelOf(item.key)}
                {locked && <em className="tbl-colset-locked">{t('锁定')}</em>}
              </span>
              <span className="tbl-colset-switch">
                <input
                  type="checkbox"
                  checked={item.visible || locked}
                  disabled={locked}
                  onChange={() => toggleVisible(item.key)}
                  aria-label={`${t('显示')}：${labelOf(item.key)}`}
                />
              </span>
              <span className="tbl-colset-align" role="group" aria-label={`${t('对齐')}：${labelOf(item.key)}`}>
                {ALIGN_OPTIONS.map((a) => (
                  <button
                    key={a.value}
                    type="button"
                    className={item.align === a.value ? 'active' : ''}
                    onClick={() => setAlign(item.key, a.value)}
                    aria-pressed={item.align === a.value}
                    title={`${t('对齐')}：${t(a.label)}`}
                  >
                    {t(a.label)}
                  </button>
                ))}
              </span>
            </div>
          );
        })}
      </div>
      <div className="tbl-colset-foot">
        <span className="tbl-colset-tip">{t('拖动手柄调整顺序；双击表头把手还原列宽')}</span>
        <button type="button" className="tbl-colset-reset" onClick={onReset}>
          {t('恢复默认')}
        </button>
      </div>
    </div>
  );
}
