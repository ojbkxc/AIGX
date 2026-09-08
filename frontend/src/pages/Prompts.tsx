import { useState, useEffect, useMemo, useRef, type ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { Search, Plus, Copy, Trash2, Pencil, Upload, Download, BookOpen } from 'lucide-react';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Input, Textarea, Badge, EmptyState } from '../components/ui';
import './Prompts.css';

interface PromptItem {
  id: string;
  name: string;
  content: string;
  tags: string[];
  enabled: boolean;
  created_at: number;
  updated_at: number;
}

interface PromptForm {
  name: string;
  content: string;
  tags: string;
}

const STORAGE_KEY = 'aigx_prompts';
const EMPTY_FORM: PromptForm = { name: '', content: '', tags: '' };

/** 行 ID：secure context 用 crypto.randomUUID，http 环境回退时间戳随机串 */
function genId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function loadPrompts(): PromptItem[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((p): p is PromptItem =>
      Boolean(p && typeof p === 'object' && (p as PromptItem).id && typeof (p as PromptItem).content === 'string'),
    );
  } catch {
    return [];
  }
}

function savePrompts(items: PromptItem[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(items));
  } catch {
    // 本地存储写满/被禁用时静默降级
  }
}

/**
 * Prompts — 提示词库（P1 前端体验跃升第 16 项）。
 *
 * localStorage 持久化（与 /chat 会话存储同模式，后端 API 保持可替换），
 * open-webui Prompts workspace 的信息架构：搜索 + 标签 + 卡片网格 +
 * 创建/编辑/复制/删除/启用切换 + JSON 导入导出。
 * Chat 页通过 suggestionPrompts 消费「已启用」提示词。
 */
export default function Prompts(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [prompts, setPrompts] = useState<PromptItem[]>(() => loadPrompts());
  const [query, setQuery] = useState('');
  const [tagFilter, setTagFilter] = useState('');
  const [showModal, setShowModal] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [form, setForm] = useState<PromptForm>(EMPTY_FORM);
  const [formError, setFormError] = useState('');
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  const [importing, setImporting] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    savePrompts(prompts);
  }, [prompts]);

  const allTags = useMemo(() => {
    const set = new Set<string>();
    for (const p of prompts) for (const tag of p.tags) if (tag.trim()) set.add(tag.trim());
    return [...set].sort();
  }, [prompts]);

  const q = query.trim().toLowerCase();
  const visible = useMemo(() => {
    return prompts
      .filter((p) => {
        if (tagFilter && !p.tags.some((tag) => tag.trim() === tagFilter)) return false;
        if (!q) return true;
        const haystack = [p.name, p.content, ...p.tags].join(' ').toLowerCase();
        return haystack.includes(q);
      })
      .sort((a, b) => b.updated_at - a.updated_at);
  }, [prompts, q, tagFilter]);

  const openCreate = () => {
    setEditingId(null);
    setForm(EMPTY_FORM);
    setFormError('');
    setShowModal(true);
  };

  const openEdit = (p: PromptItem) => {
    setEditingId(p.id);
    setForm({ name: p.name, content: p.content, tags: p.tags.join(', ') });
    setFormError('');
    setShowModal(true);
  };

  const handleSave = () => {
    if (!form.name.trim()) {
      setFormError(t('提示词名称为必填项'));
      return;
    }
    if (!form.content.trim()) {
      setFormError(t('提示词内容为必填项'));
      return;
    }
    const tags = form.tags
      .split(/[,，]/)
      .map((s) => s.trim())
      .filter(Boolean);
    const now = Date.now();
    if (editingId) {
      setPrompts((prev) => prev.map((p) => (p.id === editingId
        ? { ...p, name: form.name.trim(), content: form.content, tags, updated_at: now }
        : p)));
      addToast(t('提示词已保存'));
    } else {
      const item: PromptItem = {
        id: genId(),
        name: form.name.trim(),
        content: form.content,
        tags,
        enabled: true,
        created_at: now,
        updated_at: now,
      };
      setPrompts((prev) => [item, ...prev]);
      addToast(t('提示词已保存'));
    }
    setShowModal(false);
  };

  const handleDelete = (p: PromptItem) => {
    setConfirmState({
      title: t('删除提示词'),
      message: `${t('确定删除提示词')}「${p.name}」？`,
      confirmText: t('删除'),
      danger: true,
      onConfirm: () => {
        setPrompts((prev) => prev.filter((x) => x.id !== p.id));
        addToast(t('提示词已删除'));
      },
    });
  };

  const handleCopy = async (p: PromptItem) => {
    try {
      await navigator.clipboard.writeText(p.content);
      addToast(t('内容已复制'));
    } catch {
      addToast(t('复制失败，请手动选择复制'), 'error');
    }
  };

  const handleToggle = (id: string) => {
    setPrompts((prev) => prev.map((p) => (p.id === id ? { ...p, enabled: !p.enabled, updated_at: Date.now() } : p)));
  };

  const handleExport = () => {
    try {
      const blob = new Blob([JSON.stringify(prompts, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `aigx-prompts-${new Date().toISOString().slice(0, 10)}.json`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(url);
    } catch {
      addToast(t('导出失败'), 'error');
    }
  };

  const handleImportFile = (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    setImporting(true);
    const reader = new FileReader();
    reader.onload = () => {
      try {
        const parsed = JSON.parse(String(reader.result)) as unknown;
        if (!Array.isArray(parsed)) throw new Error('not-array');
        const now = Date.now();
        const items: PromptItem[] = parsed
          .filter((x): x is Record<string, unknown> => Boolean(x && typeof x === 'object'))
          .map((x) => ({
            id: typeof x.id === 'string' ? x.id : genId(),
            name: typeof x.name === 'string' && x.name ? x.name : t('未命名提示词'),
            content: typeof x.content === 'string' ? x.content : '',
            tags: Array.isArray(x.tags) ? x.tags.filter((s): s is string => typeof s === 'string') : [],
            enabled: x.enabled !== false,
            created_at: typeof x.created_at === 'number' ? x.created_at : now,
            updated_at: typeof x.updated_at === 'number' ? x.updated_at : now,
          }))
          .filter((p) => p.content);
        setPrompts((prev) => {
          const byId = new Map(prev.map((p) => [p.id, p]));
          for (const item of items) byId.set(item.id, item);
          return [...byId.values()];
        });
        addToast(t('导入完成') + `（${items.length}）`);
      } catch {
        addToast(t('导入失败，JSON 格式错误'), 'error');
      } finally {
        setImporting(false);
        if (fileInputRef.current) fileInputRef.current.value = '';
      }
    };
    reader.onerror = () => {
      setImporting(false);
      addToast(t('导入失败，文件读取错误'), 'error');
    };
    reader.readAsText(file);
  };

  const enabledCount = prompts.filter((p) => p.enabled).length;

  return (
    <div>
      <div className="page-header">
        <div>
          <h1>{t('提示词库')}</h1>
          <p>{t('管理常用提示词，供聊天快速调用')}</p>
        </div>
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
          <input
            ref={fileInputRef}
            type="file"
            accept="application/json"
            style={{ display: 'none' }}
            onChange={handleImportFile}
          />
          <Button variant="outline" size="sm" onClick={() => fileInputRef.current?.click()} disabled={importing} style={{ gap: 6 }}>
            <Upload size={13} />
            {importing ? t('导入中...') : t('导入 JSON')}
          </Button>
          <Button variant="outline" size="sm" onClick={handleExport} disabled={prompts.length === 0} style={{ gap: 6 }}>
            <Download size={13} />
            {t('导出 JSON')}
          </Button>
          <Button size="sm" onClick={openCreate} style={{ gap: 6 }}>
            <Plus size={13} />
            {t('新建提示词')}
          </Button>
        </div>
      </div>

      {/* 汇总行 */}
      <div className="prompts-stats">
        <div className="prompts-stat">
          <BookOpen size={16} className="prompts-stat-icon" />
          <div>
            <div className="prompts-stat-value">{prompts.length}</div>
            <div className="prompts-stat-label">{t('总数')}</div>
          </div>
        </div>
        <div className="prompts-stat">
          <div className="prompts-stat-dot" />
          <div>
            <div className="prompts-stat-value">{enabledCount}</div>
            <div className="prompts-stat-label">{t('已启用')}</div>
          </div>
        </div>
      </div>

      <Card bodyClassName="">
        <div className="prompts-toolbar">
          <div className="prompts-search">
            <Search size={14} className="prompts-search-icon" />
            <input
              className="form-input"
              placeholder={t('搜索提示词名称/内容/标签…')}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              aria-label={t('搜索提示词')}
            />
          </div>
          {allTags.length > 0 && (
            <select
              className="form-input"
              style={{ width: 160 }}
              value={tagFilter}
              onChange={(e) => setTagFilter(e.target.value)}
              aria-label={t('按标签筛选')}
            >
              <option value="">{t('全部标签')}</option>
              {allTags.map((tag) => (
                <option key={tag} value={tag}>{tag}</option>
              ))}
            </select>
          )}
          <span className="prompts-count">{t('显示')} {visible.length} / {prompts.length}</span>
        </div>

        {visible.length === 0 ? (
          <EmptyState
            icon="📝"
            message={prompts.length === 0 ? t('暂无提示词，点击右上角「新建提示词」开始') : t('没有匹配的提示词')}
          />
        ) : (
          <div className="prompts-grid">
            {visible.map((p) => (
              <div key={p.id} className="prompt-card">
                <div className="prompt-card-head">
                  <strong className="prompt-card-name">{p.name}</strong>
                  <Badge tone={p.enabled ? 'success' : 'neutral'}>{p.enabled ? t('已启用') : t('已停用')}</Badge>
                </div>
                <div className="prompt-card-content">{p.content}</div>
                {p.tags.length > 0 && (
                  <div className="prompt-card-tags">
                    {p.tags.map((tag) => (
                      <button
                        key={tag}
                        type="button"
                        className="prompt-tag"
                        onClick={() => setTagFilter(tagFilter === tag ? '' : tag)}
                        title={t('按此标签筛选')}
                      >
                        {tag}
                      </button>
                    ))}
                  </div>
                )}
                <div className="prompt-card-actions">
                  <button type="button" className="btn btn-outline btn-sm" onClick={() => void handleCopy(p)} title={t('复制内容')}>
                    <Copy size={13} />
                  </button>
                  <button type="button" className="btn btn-outline btn-sm" onClick={() => openEdit(p)} title={t('编辑')}>
                    <Pencil size={13} />
                  </button>
                  <button type="button" className="btn btn-outline btn-sm" onClick={() => handleToggle(p.id)} title={p.enabled ? t('停用') : t('启用')}>
                    {p.enabled ? t('停用') : t('启用')}
                  </button>
                  <button type="button" className="btn btn-danger btn-sm" onClick={() => handleDelete(p)} title={t('删除')}>
                    <Trash2 size={13} />
                  </button>
                </div>
              </div>
            ))}
          </div>
        )}
      </Card>

      {showModal && (
        <div className="modal-overlay" onClick={() => setShowModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{editingId ? t('编辑提示词') : t('新建提示词')}</h3>
              <button className="modal-close" onClick={() => setShowModal(false)}>&times;</button>
            </div>
            <div className="modal-body">
              {formError && <div className="error-message">{formError}</div>}
              <div className="form-group">
                <label>{t('名称')} *</label>
                <Input
                  value={form.name}
                  onChange={(e) => setForm({ ...form, name: e.target.value })}
                  placeholder={t('例如：代码审查助手')}
                  autoFocus
                />
              </div>
              <div className="form-group">
                <label>{t('内容')} *</label>
                <Textarea
                  value={form.content}
                  onChange={(e) => setForm({ ...form, content: e.target.value })}
                  placeholder={t('提示词正文，支持多行')}
                  rows={8}
                />
              </div>
              <div className="form-group">
                <label>{t('标签（逗号分隔）')}</label>
                <Input
                  value={form.tags}
                  onChange={(e) => setForm({ ...form, tags: e.target.value })}
                  placeholder={t('例如：编码, 审查, 常用')}
                />
              </div>
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setShowModal(false)}>{t('取消')}</button>
              <button className="btn btn-primary" onClick={handleSave}>{t('保存')}</button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}