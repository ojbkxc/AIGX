import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Bot, User, Copy, Check, Pencil, Volume2, Square, RefreshCw, Trash2, Code, MoreVertical, GitFork,
} from 'lucide-react';
import MessageViewer from '../MessageViewer';
import ToolCallCard from './ToolCallCard';
import type { DebugMessage } from '../ChatDebugger';

interface ChatBubbleProps {
  message: DebugMessage;
  index: number;
  isLast: boolean;
  isBusy: boolean;
  isEditing: boolean;
  editDraft: string;
  isCopied: boolean;
  isTtsPlaying: boolean;
  isTtsTarget: boolean;
  timestamp?: number;
  responseDuration?: number;
  onEditDraftChange: (value: string) => void;
  onBeginEdit: () => void;
  onCommitEdit: () => void;
  onCancelEdit: () => void;
  onCopy: () => void;
  onSpeak: () => void;
  onRegenerate: () => void;
  onDelete?: () => void;
  /** 从这条消息分叉新会话（对照上游 assistant-message-operations onFork；
   *  仅 assistant 消息提供，截取含本条——上游 assistant_sessions.rs:101-109） */
  onFork?: () => void;
}

export default function ChatBubble({
  message,
  index: _index,
  isLast,
  isBusy,
  isEditing,
  editDraft,
  isCopied,
  isTtsPlaying,
  isTtsTarget,
  timestamp,
  responseDuration,
  onEditDraftChange,
  onBeginEdit,
  onCommitEdit,
  onCancelEdit,
  onCopy,
  onSpeak,
  onRegenerate,
  onDelete,
  onFork,
}: ChatBubbleProps): JSX.Element {
  const { t } = useTranslation();
  const [showSource, setShowSource] = useState(false);
  const [showMobileMenu, setShowMobileMenu] = useState(false);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      onCommitEdit();
    }
    if (e.key === 'Escape') {
      e.preventDefault();
      onCancelEdit();
    }
  };

  return (
    <div className={`chat-debugger-msg chat-debugger-msg-${message.role}`}>
      <span className="chat-debugger-msg-icon">
        {message.role === 'user' ? <User size={13} /> : <Bot size={13} />}
      </span>
      <div className="chat-debugger-msg-body">
        {/* 附件渲染 */}
        {message.attachments?.map((a, j) => (
          <div key={j} className="chat-debugger-msg-media">
            {a.kind === 'image' && <img src={a.url} alt="attachment" loading="lazy" />}
            {a.kind === 'video' && <video src={a.url} controls muted />}
            {a.kind === 'audio' && <audio src={a.url} controls />}
          </div>
        ))}

        {/* 编辑态 */}
        {isEditing ? (
          <div className="chat-debugger-edit-box">
            <textarea
              className="form-input"
              rows={3}
              autoFocus
              value={editDraft}
              onChange={(e) => onEditDraftChange(e.target.value)}
              onKeyDown={handleKeyDown}
            />
            <div className="chat-debugger-edit-actions">
              <button
                type="button"
                className="btn btn-outline btn-sm"
                onClick={onCancelEdit}
              >
                {t('取消')}
              </button>
              <button
                type="button"
                className="btn btn-primary btn-sm"
                onClick={onCommitEdit}
                disabled={isBusy}
              >
                {t('保存并重发')}
              </button>
            </div>
          </div>
        ) : (
          <>
            {/* 消息内容（支持源码/markdown 切换） */}
            <div className="chat-debugger-msg-content">
              {showSource ? (
                <pre className="chat-debugger-msg-source">{message.content}</pre>
              ) : (
                <MessageViewer content={message.content} reasoning={message.reasoning} />
              )}
            </div>

            {/* #37：停止标记与正文同泡（原实现另起一只「⏹」气泡，看起来像两条回答）。
                标记走 toolState 而非拼进 content——正文要保持纯净，回传历史/复制/朗读
                都不该带上 UI 提示。有工具卡片时不渲染：卡片自带「停止」状态徽标，
                两处同开就是重复标注。 */}
            {message.role === 'assistant' && message.toolState === 'interrupted'
              && !(message.toolCalls && message.toolCalls.length > 0) && (
              <div className="chat-debugger-msg-stopped" data-testid="msg-stopped">
                {'⏹'} {t('已停止生成')}
              </div>
            )}

            {/* 工具调用卡片（#63）：参数/结果折叠，流式/中断/错误三态着色 */}
            {message.role === 'assistant' && message.toolCalls && message.toolCalls.length > 0 && (
              <ToolCallCard
                calls={message.toolCalls}
                state={message.toolState ?? 'done'}
                usage={message.usage}
              />
            )}

            {/* B7: 消息元数据 */}
            {(timestamp || responseDuration || message.usage) && (
              <div className="chat-debugger-msg-meta">
                {timestamp && (
                  <span className="chat-debugger-msg-time">
                    {new Date(timestamp).toLocaleTimeString()}
                  </span>
                )}
                {responseDuration !== undefined && (
                  <span
                    className="chat-debugger-msg-duration"
                    data-speed={responseDuration < 500 ? 'fast' : responseDuration < 2000 ? 'mid' : 'slow'}
                  >
                    {(responseDuration / 1000).toFixed(1)}s
                  </span>
                )}
                {/* usage 收尾（#63）：无工具卡片时在这里展示；有卡片时卡片内展示 */}
                {message.usage && !(message.toolCalls && message.toolCalls.length > 0) && (
                  <span className="chat-debugger-msg-usage" data-testid="usage-summary">
                    {message.usage.prompt_tokens !== undefined && (
                      <span>{t('输入 Tokens')}: {message.usage.prompt_tokens}</span>
                    )}
                    {message.usage.completion_tokens !== undefined && (
                      <span>{t('输出 Tokens')}: {message.usage.completion_tokens}</span>
                    )}
                    {message.usage.total_tokens !== undefined && (
                      <span>{t('总 Token')}: {message.usage.total_tokens}</span>
                    )}
                    {/* #24：后端估算口径角标（#21-B 合成尾帧），与上游真值区分 */}
                    {message.usage.estimated === true && (
                      <span className="usage-estimated-badge" data-testid="usage-estimated" title={t('由网关估算，非上游回报')}>
                        {t('估算')}
                      </span>
                    )}
                  </span>
                )}
              </div>
            )}

            {/* 操作按钮 */}
            <div className="chat-debugger-msg-actions">
              <button
                type="button"
                className="chat-debugger-action-btn"
                title={t('复制消息')}
                onClick={onCopy}
              >
                {isCopied ? <Check size={13} /> : <Copy size={13} />}
              </button>

              {/* B8: 源码切换 */}
              <button
                type="button"
                className={`chat-debugger-action-btn ${showSource ? 'active' : ''}`}
                title={showSource ? t('渲染视图') : t('源码视图')}
                onClick={() => setShowSource((v) => !v)}
              >
                <Code size={13} />
              </button>

              {message.role === 'user' && (
                <button
                  type="button"
                  className="chat-debugger-action-btn"
                  title={t('编辑并重发')}
                  onClick={onBeginEdit}
                  disabled={isBusy}
                >
                  <Pencil size={13} />
                </button>
              )}

              {/* B8: 删除 */}
              {onDelete && (
                <button
                  type="button"
                  className="chat-debugger-action-btn chat-debugger-action-danger"
                  title={t('删除消息')}
                  onClick={onDelete}
                  disabled={isBusy}
                >
                  <Trash2 size={13} />
                </button>
              )}

              {message.role === 'assistant' && (
                <>
                  <button
                    type="button"
                    className={`chat-debugger-action-btn ${isTtsTarget ? 'active' : ''}`}
                    title={isTtsPlaying && isTtsTarget ? t('停止朗读') : t('朗读（TTS）')}
                    onClick={onSpeak}
                  >
                    {isTtsPlaying && isTtsTarget ? <Square size={13} /> : <Volume2 size={13} />}
                  </button>
                  {isLast && (
                    <button
                      type="button"
                      className="chat-debugger-action-btn"
                      title={t('重新生成')}
                      onClick={onRegenerate}
                      disabled={isBusy}
                    >
                      <RefreshCw size={13} />
                    </button>
                  )}
                  {/* 会话 fork：截取到本条（含）复制为新会话，原会话不动 */}
                  {onFork && (
                    <button
                      type="button"
                      className="chat-debugger-action-btn"
                      title={t('从此消息分叉新会话')}
                      onClick={onFork}
                      disabled={isBusy}
                    >
                      <GitFork size={13} />
                    </button>
                  )}
                </>
              )}

              {/* B8: 移动端更多菜单 */}
              <div className="chat-debugger-msg-more">
                <button
                  type="button"
                  className="chat-debugger-action-btn chat-debugger-more-btn"
                  title={t('更多操作')}
                  onClick={() => setShowMobileMenu((v) => !v)}
                >
                  <MoreVertical size={13} />
                </button>
                {showMobileMenu && (
                  <div className="chat-debugger-more-menu">
                    <button type="button" onClick={() => { onCopy(); setShowMobileMenu(false); }}>
                      <Copy size={13} /> {t('复制')}
                    </button>
                    <button type="button" onClick={() => { setShowSource((v) => !v); setShowMobileMenu(false); }}>
                      <Code size={13} /> {showSource ? t('渲染视图') : t('源码视图')}
                    </button>
                    {message.role === 'assistant' && onFork && (
                      <button type="button" onClick={() => { onFork(); setShowMobileMenu(false); }}>
                        <GitFork size={13} /> {t('分叉新会话')}
                      </button>
                    )}
                    {onDelete && (
                      <button type="button" className="danger" onClick={() => { onDelete(); setShowMobileMenu(false); }}>
                        <Trash2 size={13} /> {t('删除')}
                      </button>
                    )}
                  </div>
                )}
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
