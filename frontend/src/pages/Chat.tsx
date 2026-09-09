import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  MessageSquare, Plus, Trash2, PanelLeftClose, PanelLeftOpen, Pin,
  MoreHorizontal, Search, TerminalSquare, Image as ImageIcon, Pencil,
  SlidersHorizontal,
} from 'lucide-react';
import ChatDebugger, { type DebugMessage } from '../components/ChatDebugger';
import ModelPicker from '../components/ModelPicker';
import ConfirmDialog, { type ConfirmState } from '../components/ConfirmDialog';
import Tabs from '../components/ui/Tabs';
import { api } from '../api';
import type { PlaygroundRawResult } from '../types';
import './Chat.css';

interface ChatSession {
  id: string;
  title: string;
  messages: DebugMessage[];
  updated_at: number;
  pinned?: boolean;
}

const STORAGE_KEY = 'aigx_chat_sessions';

function loadSessions(): ChatSession[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed.filter((s): s is ChatSession =>
      Boolean(s && typeof s === 'object' && (s as ChatSession).id && Array.isArray((s as ChatSession).messages)),
    );
  } catch {
    return [];
  }
}

function saveSessions(sessions: ChatSession[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(sessions));
  } catch {
    // 本地存储写满/被禁用时静默降级
  }
}

function autoTitle(messages: DebugMessage[]): string {
  const first = messages.find((m) => m.role === 'user' && m.content.trim());
  const text = (first?.content ?? '').trim().replace(/\s+/g, ' ');
  return text ? text.slice(0, 20) : '';
}

/** 行 ID：secure context 用 crypto.randomUUID，http 环境回退时间戳随机串 */
function genId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

function newSession(now: number): ChatSession {
  return { id: genId(), title: '', messages: [], updated_at: now };
}

/** 排序：置顶优先，其余按更新时间倒序（Lxchat ChatDao 同款规则） */
function sortSessions(sessions: ChatSession[]): ChatSession[] {
  return [...sessions].sort((a, b) => {
    const pa = a.pinned ? 1 : 0;
    const pb = b.pinned ? 1 : 0;
    if (pa !== pb) return pb - pa;
    return b.updated_at - a.updated_at;
  });
}

/** 读取「已启用」提示词并映射为 ChatDebugger 的空状态建议卡片 */
function loadSuggestionPrompts(): Array<{ title: string; sub: string; content: string }> {
  try {
    const raw = localStorage.getItem('aigx_prompts');
    if (!raw) return [];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((p): p is { id: string; name: string; content: string; tags?: string[]; enabled?: boolean } =>
        Boolean(p && typeof p === 'object' && (p as { enabled?: boolean }).enabled !== false),
      )
      .slice(0, 6)
      .map((p) => ({
        title: p.name || 'Prompt',
        sub: (p.tags ?? []).slice(0, 3).join(' · '),
        content: p.content,
      }))
      .filter((p) => p.content);
  } catch {
    return [];
  }
}

type ChatMode = 'chat' | 'completions' | 'images';

/**
 * Chat — 聊天工作区（Playground 合并版）。
 *
 * 布局参照 new-api 的侧栏+主区结构，控件审美参照 open-webui：
 * - 左侧会话栏：新会话 + 模式 Tabs（对话/补全/图片）+ 搜索 + 会话列表。
 * - 顶栏：侧栏开关 + 全局模型 pill（按当前用户分组权限过滤）+ 新会话。
 * - 三模式共享同一个全局模型选择。
 */
export default function Chat(): JSX.Element {
  const { t } = useTranslation();
  const [mode, setMode] = useState<ChatMode>('chat');
  const [sessions, setSessions] = useState<ChatSession[]>(() => {
    const loaded = loadSessions();
    return loaded.length ? loaded : [newSession(Date.now())];
  });
  const [activeId, setActiveId] = useState<string>(() => loadSessions()[0]?.id ?? '');
  const [sidebarOpen, setSidebarOpen] = useState<boolean>(() => window.innerWidth > 900);
  // 会话搜索（Lxchat 抽屉同款）
  const [query, setQuery] = useState('');
  // 会话行菜单：当前打开菜单的会话 id
  const [menuId, setMenuId] = useState<string | null>(null);
  // 重命名：正在重命名的会话 id 与草稿
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState('');
  // 删除确认弹窗
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null);
  // 会话菜单弹层：fixed 定位坐标（点击 ⋯ 时按按钮 rect 计算）
  const [menuPos, setMenuPos] = useState<{ left: number; top: number } | null>(null);
  // 全局模型（三模式共享；下拉由 ModelPicker 按用户分组白名单过滤）
  const [model, setModel] = useState('');
  // Completions 参数预设与 Images 输出数（顶栏面板内联，避免参数区过载）
  const [compPreset, setCompPreset] = useState<'balanced' | 'precise' | 'creative'>('balanced');
  const [imgCount, setImgCount] = useState(1);

  const active = useMemo(
    () => sessions.find((s) => s.id === activeId) ?? sessions[0],
    [sessions, activeId],
  );

  // activeId 与 active 会话失配时自动对齐（全新会话 activeId='' 而 active
  // 回退 sessions[0]，若不回写 activeId，消息将永不持久化）。
  useEffect(() => {
    if (sessions.length && !sessions.some((s) => s.id === activeId)) {
      setActiveId(sessions[0].id);
    }
  }, [sessions, activeId]);

  // 点击外部关闭会话菜单
  useEffect(() => {
    if (!menuId) return;
    const onDoc = (): void => setMenuId(null);
    document.addEventListener('click', onDoc);
    return () => document.removeEventListener('click', onDoc);
  }, [menuId]);

  const persist = (next: ChatSession[]): void => {
    setSessions(next);
    saveSessions(next);
  };

  // 消息变化持久化 + 会话按更新时间排序 + 首条用户消息自动标题
  const handleMessagesChange = (messages: DebugMessage[]): void => {
    setSessions((prev) => {
      const current = prev.find((s) => s.id === activeId);
      if (!current) return prev;
      const title = current.title || autoTitle(messages);
      const isEmpty = !title && !messages.length;
      const next = sortSessions(prev
        .map((s) => (s.id === activeId
          ? { ...s, title: title || s.title, messages, updated_at: isEmpty ? s.updated_at : Date.now() }
          : s)));
      saveSessions(next);
      return next;
    });
  };

  const handleNew = (): void => {
    const session = newSession(Date.now());
    persist(sortSessions([session, ...sessions]));
    setActiveId(session.id);
    setMode('chat');
  };

  const openDeleteConfirm = (id: string): void => {
    const target = sessions.find((s) => s.id === id);
    const title = target?.title || t('新会话');
    setMenuId(null);
    setConfirmState({
      title: t('删除会话'),
      message: <>{t('确定删除该会话？')}「{title}」</>,
      confirmText: t('删除'),
      danger: true,
      onConfirm: () => deleteSession(id),
    });
  };

  const deleteSession = (id: string): void => {
    const next = sessions.filter((s) => s.id !== id);
    if (!next.length) next.push(newSession(Date.now()));
    persist(sortSessions(next));
    if (id === activeId) {
      const fallback = next.find((s) => s.id !== id) ?? next[0];
      setActiveId(fallback.id);
    }
  };

  const handleTogglePin = (id: string): void => {
    persist(sessions.map((s) => (s.id === id ? { ...s, pinned: !s.pinned } : s)));
    setMenuId(null);
  };

  const beginRename = (id: string): void => {
    const s = sessions.find((x) => x.id === id);
    setRenamingId(id);
    setRenameDraft(s?.title || '');
    setMenuId(null);
  };

  const commitRename = (): void => {
    if (renamingId == null) return;
    const title = renameDraft.trim();
    persist(sessions.map((s) => (s.id === renamingId ? { ...s, title: title || s.title } : s)));
    setRenamingId(null);
  };

  const handleSelect = (id: string): void => {
    setActiveId(id);
    setMode('chat');
    if (window.innerWidth <= 900) setSidebarOpen(false);
  };

  const q = query.trim().toLowerCase();
  const visibleSessions = q
    ? sessions.filter((s) => (s.title || t('新会话')).toLowerCase().includes(q))
    : sessions;

  return (
    <div className={`chat-shell ${sidebarOpen ? 'chat-shell-sidebar-open' : 'chat-shell-sidebar-closed'}`}>
      <aside className="chat-sidebar">
        <div className="chat-sidebar-head">
          <button type="button" className="btn btn-primary btn-sm chat-new-btn" onClick={handleNew}>
            <Plus size={14} />
            {t('新会话')}
          </button>
        </div>

        {/* 模式切换（对话/补全/图片）：放左侧会话栏，与全局模型 pill 解耦 */}
        <div className="chat-sidebar-modes">
          <Tabs<ChatMode>
            items={[
              { key: 'chat', label: <><MessageSquare size={14} /> {t('对话')}</> },
              { key: 'completions', label: <><TerminalSquare size={14} /> {t('补全')}</> },
              { key: 'images', label: <><ImageIcon size={14} /> {t('图片')}</> },
            ]}
            active={mode}
            onChange={setMode}
            ariaLabel={t('聊天模式')}
          />
        </div>

        {/* 会话搜索（Lxchat 抽屉同款） */}
        <div className="chat-sidebar-search">
          <Search size={13} />
          <input
            placeholder={t('搜索会话…')}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          {query && (
            <button type="button" onClick={() => setQuery('')} title={t('清空')}>×</button>
          )}
        </div>

        <div className="chat-sidebar-list">
          {visibleSessions.length === 0 && (
            <div className="chat-sidebar-empty">{q ? t('没有匹配的会话') : t('暂无会话')}</div>
          )}
          {visibleSessions.map((s) => (
            <div
              key={s.id}
              className={`chat-session ${s.id === (active?.id ?? '') ? 'active' : ''} ${s.pinned ? 'pinned' : ''}`}
              role="button"
              tabIndex={0}
              onClick={() => handleSelect(s.id)}
              onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); handleSelect(s.id); } }}
            >
              {s.pinned ? <Pin size={13} className="chat-session-icon" /> : <MessageSquare size={13} className="chat-session-icon" />}
              {renamingId === s.id ? (
                <input
                  className="chat-session-rename"
                  autoFocus
                  value={renameDraft}
                  onChange={(e) => setRenameDraft(e.target.value)}
                  onClick={(e) => e.stopPropagation()}
                  onKeyDown={(e) => {
                    e.stopPropagation();
                    if (e.key === 'Enter') { e.preventDefault(); commitRename(); }
                    if (e.key === 'Escape') { e.preventDefault(); setRenamingId(null); }
                  }}
                  onBlur={commitRename}
                />
              ) : (
                <span className="chat-session-title">{s.title || t('新会话')}</span>
              )}
              {renamingId !== s.id && (
                <div className="chat-session-actions" onClick={(e) => e.stopPropagation()}>
                  <button
                    type="button"
                    className="chat-session-more"
                    title={t('更多操作')}
                    onClick={(e) => {
                      if (menuId === s.id) { setMenuId(null); return; }
                      const rect = (e.currentTarget as HTMLButtonElement).getBoundingClientRect();
                      const MENU_W = 130;
                      const MENU_H = 110;
                      const left = Math.max(8, rect.right - MENU_W);
                      const top = rect.bottom + 4 + MENU_H > window.innerHeight
                        ? rect.top - MENU_H - 4
                        : rect.bottom + 4;
                      setMenuPos({ left, top });
                      setMenuId(s.id);
                    }}
                  >
                    <MoreHorizontal size={13} />
                  </button>
                  {menuId === s.id && menuPos && (
                    <div className="chat-session-menu" style={{ position: 'fixed', left: menuPos.left, top: menuPos.top }}>
                      <button type="button" onClick={() => handleTogglePin(s.id)}>
                        <Pin size={13} /> {s.pinned ? t('取消置顶') : t('置顶')}
                      </button>
                      <button type="button" onClick={() => beginRename(s.id)}>
                        <Pencil size={13} /> {t('重命名')}
                      </button>
                      <button type="button" className="danger" onClick={() => openDeleteConfirm(s.id)}>
                        <Trash2 size={13} /> {t('删除会话')}
                      </button>
                    </div>
                  )}
                </div>
              )}
            </div>
          ))}
        </div>
      </aside>

      <div className="chat-overlay" onClick={() => setSidebarOpen(false)} />

      <main className="chat-main">
        {/* 唯一顶栏：侧栏开关 + 全局模型下拉（原模式 Tabs 位置）+ 参数 + 新会话 */}
        <div className="chat-topbar">
          <div className="chat-topbar-left">
            <button
              type="button"
              className="chat-icon-btn chat-sidebar-toggle"
              onClick={() => setSidebarOpen((v) => !v)}
              title={sidebarOpen ? t('收起会话列表') : t('展开会话列表')}
              aria-label={sidebarOpen ? t('收起会话列表') : t('展开会话列表')}
            >
              {sidebarOpen ? <PanelLeftClose size={15} /> : <PanelLeftOpen size={15} />}
            </button>
          </div>
          <div className="chat-topbar-right">
            <ModelPicker
              value={model}
              onChange={setModel}
              compact
            />
            {mode === 'completions' && (
              <label className="chat-preset-select" title={t('参数预设')}>
                <SlidersHorizontal size={13} />
                <select
                  value={compPreset}
                  onChange={(e) => setCompPreset(e.target.value as 'balanced' | 'precise' | 'creative')}
                >
                  <option value="balanced">{t('平衡')}</option>
                  <option value="precise">{t('严谨')}</option>
                  <option value="creative">{t('创意')}</option>
                </select>
              </label>
            )}
            {mode === 'images' && (
              <label className="chat-preset-select" title={t('生成数量')}>
                <ImageIcon size={13} />
                <select value={String(imgCount)} onChange={(e) => setImgCount(Number(e.target.value) || 1)}>
                  <option value="1">1</option>
                  <option value="2">2</option>
                  <option value="4">4</option>
                </select>
              </label>
            )}
            <button
              type="button"
              className="chat-icon-btn"
              title={t('新建会话')}
              aria-label={t('新建会话')}
              onClick={handleNew}
            >
              <Plus size={15} />
            </button>
          </div>
        </div>

        <div className="chat-main-body">
          {mode === 'chat' && active && (
            <div className="chat-mode-body">
              <ChatDebugger
                key={active.id}
                initialMessages={active.messages}
                onMessagesChange={handleMessagesChange}
                model={model}
                onModelChange={setModel}
                hideToolbar
                suggestionPrompts={loadSuggestionPrompts()}
              />
            </div>
          )}
          {mode === 'completions' && (
            <CompletionsPanel model={model} preset={compPreset} />
          )}
          {mode === 'images' && (
            <ImagesPanel model={model} count={imgCount} />
          )}
        </div>
      </main>

      <ConfirmDialog state={confirmState} onClose={() => setConfirmState(null)} />
    </div>
  );
}

/** Completions 面板：参数左栏 + 响应 JSON 右栏（open-webui 双栏） */
function CompletionsPanel({
  model,
  preset,
}: {
  model: string;
  preset: 'balanced' | 'precise' | 'creative';
}): JSX.Element {
  const { t } = useTranslation();
  const [prompt, setPrompt] = useState('');
  const [temperature, setTemperature] = useState('0.7');
  const [maxTokens, setMaxTokens] = useState('1024');
  const [topP, setTopP] = useState('1');
  const [presence, setPresence] = useState('0');
  const [frequency, setFrequency] = useState('0');
  const [jsonMode, setJsonMode] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [result, setResult] = useState<unknown>(null);

  const handleRun = async (): Promise<void> => {
    if (!prompt.trim() || busy) return;
    setBusy(true);
    setError('');
    setResult(null);
    try {
      const res = await api.playgroundChat({
        model: model.trim(),
        prompt: prompt.trim(),
        max_tokens: Number(maxTokens) || 1024,
        temperature: Number(temperature) || 0.7,
        top_p: Number(topP) || 1,
        presence_penalty: Number(presence) || 0,
        frequency_penalty: Number(frequency) || 0,
        json_mode: jsonMode,
      });
      const content = res.data?.content;
      if (typeof content === 'string') {
        setResult({ content, model });
      } else {
        setResult(res);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const applyPreset = (p: 'balanced' | 'precise' | 'creative'): void => {
    setTemperature(p === 'precise' ? '0.2' : p === 'creative' ? '1.3' : '0.7');
  };

  return (
    <div className="playground-v2">
      <div className="playground-v2-form">
        <div className="playground-v2-model-chip">
          <span className="playground-v2-model-label">{t('模型')}</span>
          <span className="playground-v2-model-name">{model || t('顶栏选择模型')}</span>
        </div>
        <div className="form-group">
          <label>{t('提示词')}</label>
          <textarea
            className="form-input"
            rows={6}
            placeholder={t('输入 completions 提示词…')}
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
          />
        </div>
        <div className="playground-v2-params">
          <div className="form-group">
            <label>{t('温度')}</label>
            <input className="form-input" type="number" step="0.1" min="0" max="2" value={temperature} onChange={(e) => setTemperature(e.target.value)} />
          </div>
          <div className="form-group">
            <label>{t('最大输出 Token')}</label>
            <input className="form-input" type="number" min="1" value={maxTokens} onChange={(e) => setMaxTokens(e.target.value)} />
          </div>
          <div className="form-group">
            <label>top_p</label>
            <input className="form-input" type="number" step="0.1" min="0" max="1" value={topP} onChange={(e) => setTopP(e.target.value)} />
          </div>
          <div className="form-group">
            <label>{t('存在惩罚')}</label>
            <input className="form-input" type="number" step="0.1" min="-2" max="2" value={presence} onChange={(e) => setPresence(e.target.value)} />
          </div>
          <div className="form-group">
            <label>{t('频率惩罚')}</label>
            <input className="form-input" type="number" step="0.1" min="-2" max="2" value={frequency} onChange={(e) => setFrequency(e.target.value)} />
          </div>
        </div>
        <label className="playground-v2-check">
          <input type="checkbox" checked={jsonMode} onChange={(e) => setJsonMode(e.target.checked)} />
          <span>{t('JSON 模式')}</span>
        </label>
        <div className="playground-v2-run">
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => void handleRun()}
            disabled={busy || !prompt.trim()}
          >
            {busy ? t('运行中…') : t('运行')}
          </button>
          <button
            type="button"
            className="btn btn-outline btn-sm"
            title={t('应用预设参数')}
            onClick={() => applyPreset(preset)}
          >
            {t('应用预设')}
          </button>
        </div>
        {error && <div className="error-message">{error}</div>}
      </div>
      <div className="playground-v2-result">
        <div className="playground-v2-result-head">
          <span>{t('响应')}</span>
          {result != null && (
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={() => {
                void navigator.clipboard?.writeText(JSON.stringify(result, null, 2));
              }}
            >
              {t('复制 JSON')}
            </button>
          )}
        </div>
        {result == null && !busy && (
          <div className="playground-v2-empty">{t('运行后这里显示上游 JSON 响应')}</div>
        )}
        {result != null && (
          <pre className="playground-v2-json">{JSON.stringify(result, null, 2)}</pre>
        )}
      </div>
    </div>
  );
}

/** Images 面板：参数 + 图片网格 + JSON（透传 /images/generations） */
function ImagesPanel({
  model,
  count,
}: {
  model: string;
  count: number;
}): JSX.Element {
  const { t } = useTranslation();
  const [prompt, setPrompt] = useState('');
  const [size, setSize] = useState('1024x1024');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [result, setResult] = useState<unknown>(null);

  const handleRun = async (): Promise<void> => {
    if (!prompt.trim() || busy) return;
    setBusy(true);
    setError('');
    setResult(null);
    try {
      const res = (await api.playgroundImages({
        model: model.trim(),
        prompt: prompt.trim(),
        n: count,
        size,
      })) as PlaygroundRawResult;
      setResult(res.data ?? res);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const images: Array<{ url?: string; b64_json?: string }> =
    (result as { data?: Array<{ url?: string; b64_json?: string }> })?.data ?? [];

  return (
    <div className="playground-v2">
      <div className="playground-v2-form">
        <div className="playground-v2-model-chip">
          <span className="playground-v2-model-label">{t('模型')}</span>
          <span className="playground-v2-model-name">{model || t('顶栏选择模型')}</span>
        </div>
        <div className="form-group">
          <label>{t('提示词')}</label>
          <textarea
            className="form-input"
            rows={6}
            placeholder={t('描述你想生成的图片…')}
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
          />
        </div>
        <div className="playground-v2-params">
          <div className="form-group">
            <label>size</label>
            <select className="form-input" value={size} onChange={(e) => setSize(e.target.value)}>
              <option value="1024x1024">1024x1024</option>
              <option value="1792x1024">1792x1024</option>
              <option value="1024x1792">1024x1792</option>
            </select>
          </div>
          <div className="form-group">
            <label>{t('生成数量')}</label>
            <span className="playground-v2-count-chip">{count}</span>
          </div>
        </div>
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => void handleRun()}
          disabled={busy || !prompt.trim()}
        >
          {busy ? t('生成中...') : t('生成')}
        </button>
        {error && <div className="error-message">{error}</div>}
      </div>
      <div className="playground-v2-result">
        <div className="playground-v2-result-head">
          <span>{t('结果')}</span>
          {result != null && (
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={() => void navigator.clipboard?.writeText(JSON.stringify(result, null, 2))}
            >
              {t('复制 JSON')}
            </button>
          )}
        </div>
        {result == null && !busy && (
          <div className="playground-v2-empty">{t('生成后这里显示图片与上游 JSON 响应')}</div>
        )}
        {images.length > 0 && (
          <div className="playground-v2-images">
            {images.map((img, i) => (
              <img
                key={i}
                src={img.url || `data:image/png;base64,${img.b64_json ?? ''}`}
                alt={`generated-${i}`}
                loading="lazy"
              />
            ))}
          </div>
        )}
        {result != null && images.length === 0 && (
          <pre className="playground-v2-json">{JSON.stringify(result, null, 2)}</pre>
        )}
      </div>
    </div>
  );
}