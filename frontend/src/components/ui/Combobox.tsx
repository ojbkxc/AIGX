import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import './Combobox.css';

export interface ComboboxProps {
  /** 当前值（受控） */
  value: string;
  onChange: (value: string) => void;
  /** 候选列表（调用方负责去重/排序） */
  options: string[];
  /** 置灰不可选的候选（如已被占用的名字），仍展示作提示 */
  disabledOptions?: string[];
  placeholder?: string;
  disabled?: boolean;
  /** 允许输入候选之外的自由值（默认 true） */
  allowFreeText?: boolean;
  /** 输入框额外 class（默认沿用 cb-input） */
  className?: string;
}

/**
 * Combobox — 表单内联可搜索下拉（input 形态）。
 *
 * 与 ModelPicker（按钮 + 弹层的选择器）互补：面向表格行 / 表单字段。
 * 聚焦展开全部候选，输入即过滤，Enter / 点击选定，Esc 还原并关闭，
 * 点击外部关闭；`allowFreeText` 时允许提交候选之外的自由值；
 * `disabledOptions` 中的项置灰不可选（键盘导航自动跳过）。
 */
export default function Combobox({
  value,
  onChange,
  options,
  disabledOptions,
  placeholder,
  disabled,
  allowFreeText = true,
  className,
}: ComboboxProps): JSX.Element {
  const { t } = useTranslation();
  const [text, setText] = useState(value);
  const [open, setOpen] = useState(false);
  const [idx, setIdx] = useState(0);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // 外部值变化时同步（选定 / 父组件重置）
  useEffect(() => {
    setText(value);
  }, [value]);

  // 点击外部关闭
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent): void => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
        setText(value); // 未确认的输入还原
      }
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, [open, value]);

  /** 禁用判断大小写不敏感（模型名/分组名去重场景，大小写差异视为同一键） */
  const disabledSet = useMemo(
    () => new Set((disabledOptions ?? []).map((o) => o.toLowerCase())),
    [disabledOptions],
  );
  const isDisabledOption = (option: string): boolean => disabledSet.has(option.toLowerCase());

  // 未编辑（text 与受控值一致）时展示全部候选，编辑后按输入过滤
  const dirty = text !== value;
  const visible = useMemo(() => {
    if (!dirty) return options;
    const q = text.trim().toLowerCase();
    if (!q) return options;
    return options.filter((o) => o.toLowerCase().includes(q));
  }, [dirty, text, options]);

  const clamp = (i: number): number => Math.max(0, Math.min(visible.length - 1, i));
  /** 键盘导航时跳过禁用项 */
  const step = (from: number, delta: number): number => {
    if (visible.length === 0) return 0;
    let i = clamp(from);
    for (let n = 0; n < visible.length; n += 1) {
      i = clamp(i + delta);
      if (!isDisabledOption(visible[i])) return i;
      if (i === 0 || i === visible.length - 1) break;
    }
    return clamp(from);
  };

  const pick = (option: string): void => {
    if (isDisabledOption(option)) return;
    onChange(option);
    setText(option);
    setOpen(false);
  };

  const commitFreeText = (): void => {
    const v = text.trim();
    if (!allowFreeText) {
      setText(value);
      setOpen(false);
      return;
    }
    if (v && v !== value) onChange(v);
    setOpen(false);
  };

  return (
    <div className="cb-wrap" ref={rootRef}>
      <input
        ref={inputRef}
        className={className ?? 'cb-input'}
        value={text}
        placeholder={placeholder}
        disabled={disabled}
        onFocus={(e) => {
          setOpen(true);
          setIdx(0);
          // 全选现有文本，便于直接键入替换
          e.target.select();
        }}
        onChange={(e) => {
          setText(e.target.value);
          setOpen(true);
          setIdx(0);
        }}
        onKeyDown={(e) => {
          if (e.key === 'ArrowDown') {
            e.preventDefault();
            if (!open) setOpen(true);
            else setIdx((i) => step(i, 1));
          } else if (e.key === 'ArrowUp') {
            e.preventDefault();
            setIdx((i) => step(i, -1));
          } else if (e.key === 'Enter') {
            e.preventDefault();
            const candidate = visible[clamp(idx)];
            if (open && candidate && !isDisabledOption(candidate) && (dirty || visible.length > 0)) {
              pick(candidate);
            } else {
              commitFreeText();
            }
          } else if (e.key === 'Escape') {
            setText(value);
            setOpen(false);
          } else if (e.key === 'Tab') {
            commitFreeText();
          }
        }}
        onBlur={() => {
          // 选项点击走 onMouseDown preventDefault，不会先触发 blur；
          // 真正的 blur（Tab 除外）提交自由文本
          if (open) commitFreeText();
        }}
      />
      {open && visible.length > 0 && (
        <div className="cb-list" role="listbox">
          {visible.slice(0, 100).map((option, i) => {
            const isDisabled = isDisabledOption(option);
            return (
              <button
                type="button"
                key={option}
                role="option"
                aria-selected={option === value}
                className={`cb-item ${option === value ? 'active' : ''} ${
                  i === clamp(idx) ? 'hover' : ''
                }`}
                disabled={isDisabled}
                title={isDisabled ? t('已被占用') : undefined}
                onMouseDown={(e) => {
                  e.preventDefault();
                  pick(option);
                }}
                onMouseEnter={() => setIdx(i)}
              >
                {option}
                {isDisabled && <span className="cb-item-tag">{t('已占用')}</span>}
              </button>
            );
          })}
        </div>
      )}
      {open && visible.length === 0 && (
        <div className="cb-list">
          <div className="cb-empty">
            {allowFreeText ? t('无匹配候选，将使用输入值') : t('无匹配候选')}
          </div>
        </div>
      )}
    </div>
  );
}
