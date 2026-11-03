import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { X } from 'lucide-react';
import './MultiSelect.css';

export interface MultiSelectProps {
  /** 已选值（受控） */
  value: string[];
  onChange: (value: string[]) => void;
  /** 候选列表（调用方负责去重/排序） */
  options: string[];
  placeholder?: string;
  disabled?: boolean;
  /** 允许输入候选之外的自由值（默认 true） */
  allowFreeText?: boolean;
  /** 候选为空时的提示（如「暂无可选模型」） */
  emptyHint?: string;
}

/**
 * MultiSelect — 表单内联多选（标签形态）。
 *
 * 与 Combobox（单选）互补：面向「白名单/多选」字段（如 API 密钥的模型白名单）。
 * 已选项渲染成可移除标签；输入框内键入即过滤候选，Enter 选中高亮项或自由值，
 * Backspace 在空输入时删掉最后一个标签，Esc 关闭。
 *
 * 空数组语义与 Agent2API 一致：**空 = 不限制**，由调用方在文案里说明。
 */
export default function MultiSelect({
  value,
  onChange,
  options,
  placeholder,
  disabled,
  allowFreeText = true,
  emptyHint,
}: MultiSelectProps): JSX.Element {
  const { t } = useTranslation();
  const [text, setText] = useState('');
  const [open, setOpen] = useState(false);
  const [idx, setIdx] = useState(0);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // 大小写不敏感去重：已选集合（提交给后端的是首次选中的写法）
  const selectedLower = useMemo(() => new Set(value.map((v) => v.toLowerCase())), [value]);

  /** 候选中剔除已选项，再按输入过滤 */
  const visible = useMemo(() => {
    const q = text.trim().toLowerCase();
    return options
      .filter((o) => !selectedLower.has(o.toLowerCase()))
      .filter((o) => (q ? o.toLowerCase().includes(q) : true));
  }, [options, selectedLower, text]);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent): void => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
        setText('');
      }
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, [open]);

  const add = (option: string): void => {
    const v = option.trim();
    if (!v || selectedLower.has(v.toLowerCase())) return;
    onChange([...value, v]);
    setText('');
    setIdx(0);
  };

  const remove = (option: string): void => {
    onChange(value.filter((v) => v !== option));
  };

  const clamp = (i: number): number => Math.max(0, Math.min(visible.length - 1, i));

  const commitText = (): void => {
    const v = text.trim();
    if (!v) return;
    if (!allowFreeText && !visible.some((o) => o.toLowerCase() === v.toLowerCase())) {
      setText('');
      return;
    }
    add(v);
  };

  return (
    <div className="ms-wrap" ref={rootRef}>
      <div
        className={`ms-box${disabled ? ' disabled' : ''}`}
        onClick={() => {
          if (!disabled) {
            setOpen(true);
            inputRef.current?.focus();
          }
        }}
      >
        {value.map((v) => (
          <span className="ms-chip" key={v}>
            <span className="ms-chip-label">{v}</span>
            {!disabled && (
              <button
                type="button"
                className="ms-chip-x"
                aria-label={t('移除 {{name}}', { name: v })}
                onMouseDown={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  remove(v);
                }}
              >
                <X size={12} />
              </button>
            )}
          </span>
        ))}
        <input
          ref={inputRef}
          className="ms-input"
          value={text}
          placeholder={value.length === 0 ? placeholder : ''}
          disabled={disabled}
          onFocus={() => setOpen(true)}
          onChange={(e) => {
            setText(e.target.value);
            setOpen(true);
            setIdx(0);
          }}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') {
              e.preventDefault();
              if (!open) setOpen(true);
              else setIdx((i) => clamp(i + 1));
            } else if (e.key === 'ArrowUp') {
              e.preventDefault();
              setIdx((i) => clamp(i - 1));
            } else if (e.key === 'Enter') {
              e.preventDefault();
              const candidate = visible[clamp(idx)];
              if (open && candidate) add(candidate);
              else commitText();
            } else if (e.key === 'Backspace' && !text && value.length > 0) {
              remove(value[value.length - 1]);
            } else if (e.key === 'Escape') {
              setOpen(false);
              setText('');
            } else if (e.key === ',' || e.key === '、') {
              e.preventDefault();
              commitText();
            }
          }}
          onBlur={() => {
            if (text.trim()) commitText();
          }}
        />
      </div>
      {open && visible.length > 0 && (
        <div className="ms-list" role="listbox">
          {visible.slice(0, 200).map((option, i) => (
            <button
              type="button"
              key={option}
              role="option"
              aria-selected={false}
              className={`ms-item${i === clamp(idx) ? ' hover' : ''}`}
              onMouseDown={(e) => {
                e.preventDefault();
                add(option);
              }}
              onMouseEnter={() => setIdx(i)}
            >
              {option}
            </button>
          ))}
        </div>
      )}
      {open && visible.length === 0 && (
        <div className="ms-list">
          <div className="ms-empty">
            {options.length === 0 && emptyHint
              ? emptyHint
              : allowFreeText
                ? t('无匹配候选，回车使用输入值')
                : t('无匹配候选')}
          </div>
        </div>
      )}
    </div>
  );
}