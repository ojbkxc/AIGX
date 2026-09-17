import { useState, useEffect, useMemo, useRef, type ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Search, Plus, Copy, Trash2, Pencil, Upload, Download, BookOpen, Globe, Languages } from 'lucide-react';
import { useToast } from '../components/Toast';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import { Button, Card, Input, Textarea, Badge, EmptyState } from '../components/ui';
import { api, translatePrompts } from '../api';
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

/** 翻译目标语言选项（值需与后端 TRANSLATE_TARGETS 对齐） */
const TRANSLATE_TARGETS = ['简体中文', 'English', '日本語', '한국어'];

/** 判断文本是否疑似英文（保守：含英文字母且不含中日韩字符） */
function looksEnglish(text: string): boolean {
  const hasLetter = /[a-zA-Z]/.test(text);
  const hasCjk = /[\u4e00-\u9fff\u3040-\u30ff\uac00-\ud7af]/.test(text);
  return hasLetter && !hasCjk;
}

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
  const navigate = useNavigate();
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

  // 公开提示词源（拉取公开源）
  const [sourceModal, setSourceModal] = useState(false);
  const [sources, setSources] = useState<Array<{ id: string; name: string; description: string; repo: string }>>([]);
  const [sourcesLoading, setSourcesLoading] = useState(false);
  const [fetchingSource, setFetchingSource] = useState<string | null>(null);
  // 自环翻译：目标语言 + 每批大小 + 是否翻译英文条目
  const [translateTarget, setTranslateTarget] = useState('简体中文');
  const [translateEnabled, setTranslateEnabled] = useState(true);
  const [translateBatch, setTranslateBatch] = useState(20);
  const [translating, setTranslating] = useState(false);

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

  const openSources = async () => {
    setSourceModal(true);
    setSourcesLoading(true);
    try {
      const res = await api.listPromptSources();
      setSources(Array.isArray(res?.data) ? res.data : []);
    } catch (err) {
      addToast(err instanceof Error ? err.message : t('拉取公开源列表失败'), 'error');
    } finally {
      setSourcesLoading(false);
    }
  };

  /**
   * 自环翻译英文提示词条目（按批次切片调用 /api/prompts/translate）。
   *
   * items：待翻译条目数组（content 可能为英文）。返回按 index 补齐后的
   * 翻译文本 Map；翻译失败抛错（由调用方 toast 提示）。
   */
  const translateItems = async (
    items: Array<{ content: string }>,
  ): Promise<Map<number, string>> => {
    const result = new Map<number, string>();
    if (!translateEnabled || translateTarget === 'English') return result;
    const targets = items
      .map((it, idx) => ({ it, idx }))
      .filter(({ it }) => looksEnglish(it.content));
    if (targets.length === 0) return result;
    const batchSize = Math.min(Math.max(translateBatch, 1), 40);
    for (let i = 0; i < targets.length; i += batchSize) {
      const chunk = targets.slice(i, i + batchSize);
      const res = await translatePrompts(
        chunk.map(({ it }) => ({ content: it.content })),
        translateTarget,
      );
      const translated = res?.data?.translated;
      if (!Array.isArray(translated)) break;
      for (const item of translated) {
        const target = chunk.find((c) => c.idx === item.index);
        if (target && item.content) result.set(target.idx, item.content);
      }
    }
    return result;
  };

  const handleFetchSource = async (id: string) => {
    if (fetchingSource) return;
    setFetchingSource(id);
    try {
      const res = await api.fetchPromptSource(id);
      const list = Array.isArray(res?.data) ? res.data : [];
      if (list.length === 0) {
        addToast(t('该源无可用提示词'), 'error');
        return;
      }
      const now = Date.now();
      const items: PromptItem[] = list.map((x) => ({
        id: genId(),
        name: x.name || t('未命名提示词'),
        content: x.content || '',
        tags: Array.isArray(x.tags) ? x.tags : [],
        enabled: true,
        created_at: now,
        updated_at: now,
      }));

      // 自环翻译：把英文条目的 content 替换为中文（翻译失败则保留原文）
      if (translateEnabled && translateTarget !== 'English') {
        const english = items.filter((it) => looksEnglish(it.content));
        if (english.length > 0) {
          setTranslating(true);
          try {
            const translated = await translateItems(english.map((it) => ({ content: it.content })));
            let count = 0;
            english.forEach((it, idx) => {
              const text = translated.get(idx);
              if (text) {
                it.content = text;
                count += 1;
              }
            });
            if (count > 0) addToast(t('已自动翻译') + ` ${count} ` + t('条英文提示词'));
          } catch (err) {
            addToast(err instanceof Error ? err.message : t('翻译失败，保留原文'), 'error');
          } finally {
            setTranslating(false);
          }
        }
      }

      // 按 name 去重：已存在同名提示词则跳过，仅补充新条目
      setPrompts((prev) => {
        const existing = new Set(prev.map((p) => p.name));
        const fresh = items.filter((it) => !existing.has(it.name));
        return [...fresh, ...prev];
      });
      addToast(t('拉取完成，新增') + ` ${items.length} ` + t('条提示词，可在 Chat 页空状态直接使用'));
      setSourceModal(false);
    } catch (err) {
      addToast(err instanceof Error ? err.message : t('拉取公开源失败'), 'error');
    } finally {
      setFetchingSource(null);
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
          <Button variant="outline" size="sm" onClick={openSources} style={{ gap: 6 }}>
            <Globe size={13} />
            {t('拉取公开源')}
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void (async () => {
                setTranslating(true);
                try {
                  const list = prompts.filter((p) => looksEnglish(p.content));
                  if (list.length === 0) {
                    addToast(t('没有需要翻译的英文提示词'));
                    return;
                  }
                  const translated = await translateItems(list.map((it) => ({ content: it.content })));
                  let count = 0;
                  setPrompts((prev) => prev.map((p) => {
                    const idx = list.findIndex((e) => e.id === p.id);
                    if (idx < 0) return p;
                    const text = translated.get(idx);
                    if (!text) return p;
                    count += 1;
                    return { ...p, content: text, updated_at: Date.now() };
                  }));
                  addToast(t('已翻译') + ` ${count} ` + t('条'));
                } catch (err) {
                  addToast(err instanceof Error ? err.message : t('翻译失败'), 'error');
                } finally {
                  setTranslating(false);
                }
              })();
            }}
            disabled={translating || prompts.length === 0}
            style={{ gap: 6 }}
          >
            <Languages size={13} />
            {translating ? t('翻译中...') : t('翻译已有')}
          </Button>
          <Button variant="outline" size="sm" onClick={() => navigate('/chat')} style={{ gap: 6 }}>
            <BookOpen size={13} />
            {t('去使用')}
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

      {sourceModal && (
        <div className="modal-overlay" onClick={() => setSourceModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3>{t('拉取公开提示词源')}</h3>
              <button className="modal-close" onClick={() => setSourceModal(false)}>&times;</button>
            </div>
            <div className="modal-body">
              <p className="prompts-source-hint">{t('选择一个公开源，拉取后按名称去重合并到本地提示词库（同名跳过）。')}</p>
              <div className="prompts-translate-row">
                <div className="prompts-translate-item">
                  <span>{t('目标语言')}</span>
                  <select
                    className="form-input"
                    style={{ width: 120 }}
                    value={translateTarget}
                    onChange={(e) => setTranslateTarget(e.target.value)}
                    aria-label={t('翻译目标语言')}
                  >
                    {TRANSLATE_TARGETS.map((lang) => (
                      <option key={lang} value={lang}>{lang}</option>
                    ))}
                  </select>
                </div>
                <div className="prompts-translate-item">
                  <span>{t('批量大小')}</span>
                  <input
                    type="number"
                    className="form-input"
                    style={{ width: 80 }}
                    min={1}
                    max={40}
                    value={translateBatch}
                    onChange={(e) => setTranslateBatch(Math.min(40, Math.max(1, Number(e.target.value) || 20)))}
                    aria-label={t('翻译批量大小')}
                  />
                </div>
                <label className="prompts-translate-toggle">
                  <input
                    type="checkbox"
                    checked={translateEnabled}
                    onChange={(e) => setTranslateEnabled(e.target.checked)}
                  />
                  <span>{t('自动翻译英文提示词')}</span>
                </label>
              </div>
              <p className="prompts-source-hint">{t('翻译走 AIGX 自己的渠道（需 [agent] 配置模型），失败时保留原文。')}</p>
              {sourcesLoading ? (
                <div className="prompts-source-loading">{t('加载中...')}</div>
              ) : sources.length === 0 ? (
                <div className="prompts-source-loading">{t('暂无可用公开源')}</div>
              ) : (
                <div className="prompts-source-list">
                  {sources.map((s) => (
                    <div key={s.id} className="prompts-source-item">
                      <div className="prompts-source-info">
                        <strong>{s.name}</strong>
                        <span>{s.description}</span>
                        <small>{s.repo}</small>
                      </div>
                      <Button
                        size="sm"
                        variant="outline"
                        disabled={fetchingSource !== null || translating}
                        onClick={() => void handleFetchSource(s.id)}
                        style={{ gap: 6 }}
                      >
                        {fetchingSource === s.id
                          ? t('拉取中...')
                          : translating
                            ? t('翻译中...')
                            : t('拉取')}
                      </Button>
                    </div>
                  ))}
                </div>
              )}
            </div>
            <div className="modal-footer">
              <button className="btn btn-outline" onClick={() => setSourceModal(false)}>{t('关闭')}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}