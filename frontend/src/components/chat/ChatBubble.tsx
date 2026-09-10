import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Bot, User, Copy, Check, Pencil, Volume2, Square, RefreshCw, Trash2, Code, MoreVertical,
} from 'lucide-react';
import MessageViewer from '../MessageViewer';
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

            {/* B7: 消息元数据 */}
            {(timestamp || responseDuration) && (
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
