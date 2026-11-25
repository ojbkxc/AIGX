/**
 * /chat 的页面外壳三件套：左栏会话历史 / 顶栏 / 空态。
 *
 * 1:1 对齐 A2A v3.5.1 `islands/assistant-shell.tsx` 的分区与层级：
 *   · 左栏 = 头部「新对话」+ 列表（会话项 / 空态）+ 底部动作区；
 *   · 顶栏 = 会话标题 + 右侧设置入口（A2A 的 keyLabel 位在 AIGX 放模型名，
 *     因为 AIGX 没有 `/api/model-access/keys`，模型选择才是这一页的现实入口）；
 *   · 空态 = logo + 标题 + 一行说明 + 建议按钮网格 + 可选警告条。
 *
 * ── AIGX 独有项（A2A 没有，不许因对齐而消失）─────────────────
 *   · 会话项上的「自动/手工」命名来源徽标、父会话追溯、行内重命名、分叉；
 *   · 顶栏的「调试」入口（ChatDebugger 的入口，A2A 无此概念）。
 * 这些都以附加元素的形式挂在 A2A 的位置上，不改变 A2A 的分区顺序。
 */
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ChatChevronIcon,
  ChatLayersIcon,
  ChatPlusIcon,
  ChatSlidersIcon,
} from '../../pages/chat-icons';

export interface AsstSessionItem {
  id: string;
  title: string;
  /** 秒级时间戳（后端 ChatSessionSummary.updated_at 单位） */
  updated_at: number;
  model?: string;
  parent_session_id?: string | null;
  title_source?: string | null;
}

export interface AsstSidebarProps {
  sessions: AsstSessionItem[];
  activeId: string;
  disabled: boolean;
  onNew: () => void;
  onOpen: (id: string) => void;
  onRemove: (id: string) => void;
  onRename: (id: string, title: string) => void;
  onFork: (id: string) => void;
  onClear: () => void;
  onTitleSettings: () => void;
  /** AIGX 独有：后端「聊天设置」（管理接口，仅管理员渲染） */
  onChatSettings?: () => void;
}

/** 秒级时间戳 → 「今天 HH:MM」/「M月D日」；解析不出来就给空串（不编造时间）。 */
function timeLabel(updatedAt: number): string {
  if (!updatedAt) return '';
  const ms = updatedAt > 1e12 ? updatedAt : updatedAt * 1000;
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return '';
  const now = new Date();
  const sameDay = date.getFullYear() === now.getFullYear()
    && date.getMonth() === now.getMonth()
    && date.getDate() === now.getDate();
  if (sameDay) {
    return date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  }
  return date.toLocaleDateString(undefined, { month: 'numeric', day: 'numeric' });
}

export function AsstSidebar({
  sessions,
  activeId,
  disabled,
  onNew,
  onOpen,
  onRemove,
  onRename,
  onFork,
  onClear,
  onTitleSettings,
  onChatSettings,
}: AsstSidebarProps): JSX.Element {
  const { t } = useTranslation();
  /** 行内重命名态（AIGX 独有：A2A 只能删，不能改名） */
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState('');

  const commitRename = (id: string): void => {
    const next = renameDraft.trim();
    setRenameId(null);
    if (next) onRename(id, next);
  };

  return (
    <aside className="asst-side" aria-label={t('历史对话')}>
      <div className="asst-side-head">
        {/* 生成中**不**置灰：点它要能给出「请先停止」的反馈（旧页面同口径）。
            静默禁用等于把「为什么点不动」藏起来 —— 行内动作才用 disabled。 */}
        <button type="button" className="asst-new" onClick={onNew}>
          <ChatPlusIcon size={14} />
          {t('新对话')}
        </button>
      </div>

      <div className="asst-side-list">
        {sessions.length === 0 && <div className="asst-side-empty">{t('还没有历史对话')}</div>}
        {sessions.map((item) => {
          const active = item.id === activeId;
          return (
            <div className={`asst-side-item${active ? ' active' : ''}`} key={item.id}>
              {renameId === item.id ? (
                <input
                  className="asst-side-rename"
                  autoFocus
                  value={renameDraft}
                  onChange={(event) => setRenameDraft(event.target.value)}
                  onBlur={() => commitRename(item.id)}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') { event.preventDefault(); commitRename(item.id); }
                    if (event.key === 'Escape') { event.preventDefault(); setRenameId(null); }
                  }}
                />
              ) : (
                <button
                  type="button"
                  className="asst-side-open"
                  onClick={() => onOpen(item.id)}
                  title={item.title || t('未命名对话')}
                >
                  <span className="asst-side-title">
                    {item.title || t('未命名对话')}
                    {item.title_source === 'user' && <span className="asst-side-tag">{t('手工')}</span>}
                    {item.title_source === 'auto' && <span className="asst-side-tag">{t('自动')}</span>}
                    {item.parent_session_id && <span className="asst-side-tag fork">{t('分叉')}</span>}
                  </span>
                  <span className="asst-side-time">
                    {timeLabel(item.updated_at)}
                    {item.model ? ` · ${item.model}` : ''}
                  </span>
                </button>
              )}
              <span className="asst-side-acts">
                <button
                  type="button"
                  className="asst-side-act"
                  title={t('重命名')}
                  aria-label={t('重命名')}
                  disabled={disabled}
                  onClick={() => { setRenameDraft(item.title || ''); setRenameId(item.id); }}
                >
                  ✎
                </button>
                <button
                  type="button"
                  className="asst-side-act"
                  title={t('分叉此会话')}
                  aria-label={t('分叉此会话')}
                  disabled={disabled}
                  onClick={() => onFork(item.id)}
                >
                  <ChatLayersIcon size={11} />
                </button>
                <button
                  type="button"
                  className="asst-side-act del"
                  title={t('删除这条对话')}
                  aria-label={t('删除这条对话')}
                  disabled={disabled}
                  onClick={() => onRemove(item.id)}
                >
                  ✕
                </button>
              </span>
            </div>
          );
        })}
      </div>

      <div className="asst-side-foot">
        <button type="button" className="asst-side-link" disabled={disabled} onClick={onTitleSettings}>
          <ChatSlidersIcon size={12} />
          {t('标题总结设置')}
        </button>
        {onChatSettings && (
          <button type="button" className="asst-side-link" onClick={onChatSettings}>
            {t('聊天设置')}
          </button>
        )}
        {sessions.length > 0 && (
          <button type="button" className="asst-side-link danger" disabled={disabled} onClick={onClear}>
            {t('清空全部')}
          </button>
        )}
      </div>
    </aside>
  );
}

export interface AsstTopbarProps {
  title: string;
  /** 顶栏右侧的模型胶囊文案（A2A 此处是 API Key 标签） */
  modelLabel: string;
  onHistory: () => void;
  onSettings: () => void;
  /** AIGX 独有：打开 ChatDebugger 调试视图 */
  onDebugger: () => void;
  debuggerActive: boolean;
}

export function AsstTopbar({
  title,
  modelLabel,
  onHistory,
  onSettings,
  onDebugger,
  debuggerActive,
}: AsstTopbarProps): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="asst-top">
      <button
        type="button"
        className="asst-menu"
        title={t('历史对话')}
        aria-label={t('历史对话')}
        onClick={onHistory}
      >
        ☰
      </button>
      <span className="asst-title" title={title}>{title}</span>
      <span className="spacer" />
      <span className="asst-top-model" title={modelLabel}>
        <ChatLayersIcon size={12} />
        {modelLabel || t('未选模型')}
      </span>
      {/* AIGX 独有：调试视图入口（SSE 报文 / tool_calls 调试面） */}
      <button
        type="button"
        className={`asst-top-btn${debuggerActive ? ' on' : ''}`}
        title={t('调试视图（SSE 报文 / tool_calls）')}
        aria-pressed={debuggerActive}
        onClick={onDebugger}
      >
        {t('调试')}
      </button>
      <button type="button" className="asst-settings-btn" title={t('助手设置')} onClick={onSettings}>
        <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
          <g fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="3.1" />
            <path d="M19.3 13.8l1.5 1.1-1.8 3-1.8-.6c-.5.4-1 .7-1.6.9l-.3 1.9h-3.5l-.3-1.9c-.6-.2-1.1-.5-1.6-.9l-1.8.6-1.8-3 1.5-1.1a7 7 0 0 1 0-1.9l-1.5-1.1 1.8-3 1.8.6c.5-.4 1-.7 1.6-.9l.3-1.9h3.5l.3 1.9c.6.2 1.1.5 1.6.9l1.8-.6 1.8 3-1.5 1.1c.05.32.07.63.07.95 0 .31-.02.62-.05.9z" />
          </g>
        </svg>
        {t('助手设置')}
      </button>
    </div>
  );
}

export interface AsstEmptyProps {
  suggestions: Array<{ title: string; sub?: string; content: string }>;
  onSend: (text: string) => void;
  /** 没有可发消息的模型时的提示（A2A 的警告位：模型读不出来 / 无工具） */
  warning?: string;
  failed?: string;
}

export function AsstEmpty({ suggestions, onSend, warning, failed }: AsstEmptyProps): JSX.Element {
  const { t } = useTranslation();
  return (
    <div className="asst-empty">
      <div className="asst-empty-logo">
        <svg viewBox="0 0 24 24" width="26" height="26" aria-hidden="true">
          <path fill="currentColor" d="M12 2.7l1.85 5.45 5.45 1.85-5.45 1.85L12 17.3l-1.85-5.45L4.7 10l5.45-1.85L12 2.7z" />
          <path fill="currentColor" d="M18.8 15l.85 2.35L22 18.2l-2.35.85L18.8 21.4l-.85-2.35L15.6 18.2l2.35-.85L18.8 15z" />
        </svg>
      </div>
      <h3>{t('开始一个新对话')}</h3>
      <p>
        {t('发消息或做任务，模型可通过 AIGX 网关自由选路。')}
        <br />
        {t('历史会话保存在本机，随时切回继续。')}
      </p>
      {suggestions.length > 0 && (
        <div className="asst-suggests">
          {suggestions.map((item, index) => (
            <button
              type="button"
              key={`${item.title}-${index}`}
              className="asst-suggest"
              onClick={() => onSend(item.content)}
              title={item.sub || item.title}
            >
              <span className="asst-suggest-title">{item.title}</span>
              {item.sub && <span className="asst-suggest-sub">{item.sub}</span>}
            </button>
          ))}
        </div>
      )}
      {warning && <div className="asst-empty-warn">{warning}</div>}
      {failed && <div className="asst-empty-warn bad">{failed}</div>}
    </div>
  );
}

/** 供页面在顶栏折叠态里用的箭头（左栏收起时指向右，展开时指向左） */
export function AsstCollapseArrow({ collapsed }: { collapsed: boolean }): JSX.Element {
  return <ChatChevronIcon size={12} className={collapsed ? '' : 'flip'} />;
}
