import { useState, useEffect, useMemo, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Plus, Trash2, Table2, Code2 } from 'lucide-react';
import './ModelMappingEditor.css';

interface MappingRow {
  id: string;
  from: string;
  to: string;
}

interface ModelMappingEditorProps {
  value: Record<string, string> | null;
  onChange: (value: Record<string, string>) => void;
  sourceModelOptions?: string[];
  targetModelOptions?: string[];
  disabled?: boolean;
}

export default function ModelMappingEditor(props: ModelMappingEditorProps): JSX.Element {
  const { t } = useTranslation();
  const [mode, setMode] = useState<'visual' | 'json'>('visual');
  const [rows, setRows] = useState<MappingRow[]>([]);
  const [jsonText, setJsonText] = useState('');
  const [jsonError, setJsonError] = useState<string | null>(null);
  const nextId = useState({ current: 0 })[0];
  const duplicateSources = useMemo(() => getDuplicateSources(rows), [rows]);

  function getDuplicateSources(rows: MappingRow[]): string[] {
    const seen = new Set<string>();
    const duplicates = new Set<string>();
    for (const row of rows) {
      const src = row.from.trim();
      if (!src) continue;
      if (seen.has(src)) duplicates.add(src);
      else seen.add(src);
    }
    return Array.from(duplicates);
  }

  const newRow = (): MappingRow => {
    nextId.current += 1;
    return { id: `m-${nextId.current}`, from: '', to: '' };
  };

  const rowsToObj = (rs: MappingRow[]): Record<string, string> => {
    const obj: Record<string, string> = {};
    for (const r of rs) {
      const k = r.from.trim();
      if (k) obj[k] = r.to.trim();
    }
    return obj;
  };

  const objToRows = (obj: Record<string, string>): MappingRow[] => {
    return Object.entries(obj).map(([from, to]) => ({ id: `m-${++nextId.current}`, from, to }));
  };

  const sameMapping = (a: Record<string, string>, b: Record<string, string>): boolean => {
    const ak = Object.keys(a);
    const bk = Object.keys(b);
    if (ak.length !== bk.length) return false;
    for (const k of ak) if (a[k] !== b[k]) return false;
    return true;
  };

  // 外部 value 变化时同步
  useEffect(() => {
    const obj = props.value || {};
    // 避免反馈循环：空行（from 未填）产出 {} → 父组件存 {} → 回灌会擦掉正在编辑的空行
    if (sameMapping(obj, rowsToObj(rows))) return;
    setRows(objToRows(obj));
    setJsonText(JSON.stringify(obj, null, 2));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.value]);

  const sync = (rs: MappingRow[]): void => {
    setRows(rs);
    const dups = getDuplicateSources(rs);
    if (dups.length > 0) {
      setJsonError(t('源模型重复'));
    } else {
      setJsonError(null);
    }
    props.onChange(rowsToObj(rs));
  };

  const handleAddRow = (): void => sync([...rows, newRow()]);

  const handleDeleteRow = (id: string): void => sync(rows.filter((r) => r.id !== id));

  const handleRowChange = (id: string, field: 'from' | 'to', value: string): void => {
    sync(rows.map((r) => (r.id === id ? { ...r, [field]: value } : r)));
  };

  const handleJsonChange = (text: string): void => {
    setJsonText(text);
    try {
      const obj = text.trim() ? JSON.parse(text) : {};
      if (obj && typeof obj === 'object' && !Array.isArray(obj)) {
        setJsonError(null);
        props.onChange(obj);
        setRows(objToRows(obj));
      } else {
        setJsonError(t('必须是 JSON 对象'));
      }
    } catch {
      setJsonError(t('JSON 格式错误'));
    }
  };

  const handleModeChange = (next: 'visual' | 'json'): void => {
    if (next === 'json') {
      setJsonText(JSON.stringify(rowsToObj(rows), null, 2));
    }
    setMode(next);
  };

  const handleFillTemplate = (): void => {
    const template: Record<string, string> = { 'gpt-3.5-turbo': 'gpt-3.5-turbo-0125' };
    props.onChange(template);
    setRows(objToRows(template));
    setJsonText(JSON.stringify(template, null, 2));
  };

  return (
    <div className="mme-wrap">
      <div className="mme-toolbar">
        <div className="mme-mode-tabs">
          <button
            type="button"
            className={mode === 'visual' ? 'active' : ''}
            onClick={() => handleModeChange('visual')}
          >
            <Table2 size={13} />
            {t('可视化')}
          </button>
          <button
            type="button"
            className={mode === 'json' ? 'active' : ''}
            onClick={() => handleModeChange('json')}
          >
            <Code2 size={13} />
            {t('JSON')}
          </button>
        </div>
        <button
          type="button"
          className="mme-template-btn"
          onClick={handleFillTemplate}
          disabled={props.disabled}
        >
          {t('填充模板')}
        </button>
      </div>

      {jsonError && <div className="mme-error">{jsonError}</div>}

      {duplicateSources.length > 0 && (
        <div className="mme-warn">
          {t('源模型重复：')} {duplicateSources.join(', ')}
        </div>
      )}

      {mode === 'visual' ? (
        <div className="mme-visual">
          {rows.length > 0 ? (
            <>
              <div className="mme-row mme-row-head">
                <div>{t('源模型')}</div>
                <div>{t('目标模型')}</div>
                <div className="mme-row-action" />
              </div>
              {rows.map((row) => (
                <div key={row.id} className="mme-row">
                  <input
                    value={row.from}
                    onChange={(e) => handleRowChange(row.id, 'from', e.target.value)}
                    placeholder="gpt-4"
                    disabled={props.disabled}
                    list="mme-source-list"
                  />
                  <input
                    value={row.to}
                    onChange={(e) => handleRowChange(row.id, 'to', e.target.value)}
                    placeholder="gpt-4-0613"
                    disabled={props.disabled}
                    list="mme-target-list"
                  />
                  <div className="mme-row-action">
                    <button
                      type="button"
                      onClick={() => handleDeleteRow(row.id)}
                      disabled={props.disabled}
                      aria-label={t('删除')}
                    >
                      <Trash2 size={14} />
                    </button>
                  </div>
                </div>
              ))}
            </>
          ) : (
            <div className="mme-empty">
              {t('未配置映射，点击下方添加')}
            </div>
          )}
          <button
            type="button"
            className="mme-add-btn"
            onClick={handleAddRow}
            disabled={props.disabled}
          >
            <Plus size={14} />
            {t('添加映射')}
          </button>
        </div>
      ) : (
        <textarea
          className="mme-json-textarea"
          value={jsonText}
          onChange={(e: FormEvent<HTMLTextAreaElement>) => handleJsonChange(e.currentTarget.value)}
          placeholder={'{\n  "gpt-4": "gpt-4-0613"\n}'}
          disabled={props.disabled}
          rows={6}
        />
      )}

      {props.sourceModelOptions && props.sourceModelOptions.length > 0 && (
        <datalist id="mme-source-list">
          {props.sourceModelOptions.map((m) => <option key={m} value={m} />)}
        </datalist>
      )}
      {props.targetModelOptions && props.targetModelOptions.length > 0 && (
        <datalist id="mme-target-list">
          {props.targetModelOptions.map((m) => <option key={m} value={m} />)}
        </datalist>
      )}
    </div>
  );
}