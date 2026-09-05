import React, { useRef, useState } from 'react';
import { cn } from '../../lib/utils';

export interface FileInputProps {
  label?: string;
  hint?: string;
  accept?: string;
  disabled?: boolean;
  className?: string;
  onChange?: (file: File | null) => void;
}

/**
 * FileInput — 文件选择控件，复用 App.css 的 .form-input 样式体系。
 * 选中后显示文件名，便于在表单中内联使用。
 */
export default function FileInput({
  label,
  hint,
  accept,
  disabled,
  className,
  onChange
}: FileInputProps) {
  const inputId = label ? ile- : undefined;
  const inputRef = useRef<HTMLInputElement>(null);
  const [fileName, setFileName] = useState<string>('');

  const handleChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0] ?? null;
    setFileName(file ? file.name : '');
    onChange?.(file);
  };

  return (
    <div className="form-group">
      {label && <label htmlFor={inputId}>{label}</label>}
      <input
        id={inputId}
        ref={inputRef}
        type="file"
        accept={accept}
        disabled={disabled}
        className="form-input"
        style={{ display: 'none' }}
        onChange={handleChange}
      />
      <button
        type="button"
        className={cn('btn btn-outline', className)}
        disabled={disabled}
        onClick={() => inputRef.current?.click()}
      >
        {fileName || '选择文件'}
      </button>
      {hint && <span className="form-hint">{hint}</span>}
    </div>
  );
}