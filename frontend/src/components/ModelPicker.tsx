import { useEffect, useRef, useState } from 'react';
import { Bot, Search } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import './ModelPicker.css';

export interface ModelPickerProps {
  /** 当前模型值（受控） */
  value: string;
  /** 模型变化回调 */
  onChange: (model: string) => void;
  /** 指定渠道模型列表（渠道调试场景）；留空走网关模型列表 */
  channelModels?: string[];
  /** 紧凑 pill 形态（顶栏） */
  compact?: boolean;
  /** 组件重挂载时重置值 */
  key?: string;
}

/**
 * ModelPicker — 全局模型选择器（Open WebUI 顶部 pill 形态）。
 * 抽离自 ChatDebugger，供 /chat 顶栏与调试器复用；支持搜索、
 * 键盘上下选择、Enter 确认、Esc 关闭、点击外部关闭。
 */
export default function ModelPicker({
  value,
  onChange,
  channelModels = [],
  compact = false,
}: ModelPickerProps): JSX.Element {
  const { t } = useTranslation();
  const [models, setModels] = useState<string[]>(channelModels);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [idx, setIdx] = useState(0);
  const rootRef = useRef<HTMLDivElement | null>(null);

  const channelModelsKey = channelModels.join(',');

  useEffect(() => {
    if (channelModels.length) {
      setModels(channelModels.slice());
      return;
    }
    let mounted = true;
    api.listModels()
      .then((res) => {
        if (!mounted) return;
        const raw = Array.isArray(res) ? res : res?.data;
        const list: string[] = Array.isArray(raw)
          ? (raw as Array<string | { id?: string }>)
            .map((m) => (typeof m === 'string' ? m : m.id))
            .filter((v): v is string => Boolean(v))
          : [];
        setModels(list);
      })
      .catch(() => { /* 模型列表失败静默降级 */ });
    return () => { mounted = false; };
  }, [channelModelsKey]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const onDoc = (e: MouseEvent): void => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, []);

  const q = query.trim().toLowerCase();
  const visible = q ? models.filter((m) => m.toLowerCase().includes(q)) : models;
  const clamp = (i: number): number => Math.max(0, Math.min(visible.length - 1, i));

  const pick = (m: string): void => {
    onChange(m);
    setOpen(false);
    setQuery('');
  };

  return (
    <div className={`model-picker ${compact ? 'model-picker-compact' : ''}`} ref={rootRef}>
      <button
        type="button"
        className="model-picker-btn"
        onClick={() => setOpen((v) => !v)}
        title={value || t('选择模型')}
        aria-expanded={open}
      >
        <Bot size={14} />
        <span className="model-picker-name">{value || t('选择模型')}</span>
      </button>
      {open && (
        <div className="model-picker-pop">
          <div className="model-picker-search">
            <Search size={13} />
            <input
              autoFocus
              placeholder={t('搜索模型…')}
              value={query}
              onChange={(e) => { setQuery(e.target.value); setIdx(0); }}
              onKeyDown={(e) => {
                if (e.key === 'ArrowDown') {
                  e.preventDefault();
                  setIdx((i) => clamp(i + 1));
                } else if (e.key === 'ArrowUp') {
                  e.preventDefault();
                  setIdx((i) => clamp(i - 1));
                } else if (e.key === 'Enter') {
                  e.preventDefault();
                  const m = visible[clamp(idx)];
                  if (m) pick(m);
                } else if (e.key === 'Escape') {
                  setOpen(false);
                  setQuery('');
                }
              }}
            />
            {visible.length > 0 && (
              <span className="model-picker-count">{visible.length}</span>
            )}
          </div>
          <div className="model-picker-list">
            {visible.length === 0 && (
              <div className="model-picker-empty">{t('无匹配模型')}</div>
            )}
            {visible.map((m, i) => (
              <button
                type="button"
                key={m}
                className={`model-picker-item ${m === value ? 'active' : ''} ${i === clamp(idx) ? 'hover' : ''}`}
                onMouseEnter={() => setIdx(clamp(i))}
                onClick={() => pick(m)}
              >
                {m}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}