import { useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Send, Mic, Square, Plus, Image, Video, AudioLines, Link2,
} from 'lucide-react';

interface Attachment {
  kind: 'image' | 'video' | 'audio';
  url: string;
}

interface ChatInputProps {
  input: string;
  attachments: Attachment[];
  isBusy: boolean;
  isRecording: boolean;
  isTranscribing: boolean;
  contextEstimate: number;
  onInputChange: (value: string) => void;
  onSend: () => void;
  onStop: () => void;
  onStartRecording: () => void;
  onAddAttachment: (attachment: Attachment) => void;
  onRemoveAttachment: (index: number) => void;
  onClear: () => void;
}

export default function ChatInput({
  input,
  attachments,
  isBusy,
  isRecording,
  isTranscribing,
  contextEstimate,
  onInputChange,
  onSend,
  onStop,
  onStartRecording,
  onAddAttachment,
  onRemoveAttachment,
  onClear,
}: ChatInputProps): JSX.Element {
  const { t } = useTranslation();
  const [showAttachMenu, setShowAttachMenu] = useState(false);
  const [showUrlInput, setShowUrlInput] = useState(false);
  const [urlDraft, setUrlDraft] = useState('');
  const attachMenuRef = useRef<HTMLDivElement>(null);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>): void => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      onSend();
    }
  };

  const handlePaste = (e: React.ClipboardEvent<HTMLTextAreaElement>): void => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (const item of items) {
      if (item.type.startsWith('image/')) {
        const file = item.getAsFile();
        if (!file) continue;
        e.preventDefault();
        const reader = new FileReader();
        reader.onload = () => {
          const url = String(reader.result || '');
          if (url) onAddAttachment({ kind: 'image', url });
        };
        reader.readAsDataURL(file);
      }
    }
  };

  const handlePickLocalFile = (accept: string): void => {
    const inputEl = document.createElement('input');
    inputEl.type = 'file';
    inputEl.accept = accept;
    inputEl.multiple = false;
    inputEl.onchange = () => {
      const file = inputEl.files?.[0];
      if (!file) return;
      const reader = new FileReader();
      reader.onload = () => {
        const url = String(reader.result || '');
        const kind: 'image' | 'video' | 'audio' = file.type.startsWith('video')
          ? 'video'
          : file.type.startsWith('audio') ? 'audio' : 'image';
        onAddAttachment({ kind, url });
      };
      reader.readAsDataURL(file);
    };
    inputEl.click();
    setShowAttachMenu(false);
  };

  const handleCommitUrlDraft = (): void => {
    const url = urlDraft.trim();
    if (url) onAddAttachment({ kind: 'image', url });
    setUrlDraft('');
    setShowUrlInput(false);
  };

  const handleSendClick = (): void => {
    if (isBusy) {
      onStop();
      return;
    }
    if (!input.trim() && !attachments.length) {
      onStartRecording();
      return;
    }
    onSend();
  };

  return (
    <div className="chat-debugger-input-row">
      <div className="chat-debugger-input-shell">
        {/* URL 输入行 */}
        {showUrlInput && (
          <div className="chat-debugger-url-row">
            <Link2 size={13} />
            <input
              autoFocus
              placeholder={t('粘贴图片 URL，Enter 确认')}
              value={urlDraft}
              onChange={(e) => setUrlDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') { e.preventDefault(); handleCommitUrlDraft(); }
                if (e.key === 'Escape') { e.preventDefault(); setUrlDraft(''); setShowUrlInput(false); }
              }}
              onBlur={() => { setUrlDraft(''); setShowUrlInput(false); }}
            />
          </div>
        )}

        {/* 附件预览 */}
        {attachments.length > 0 && (
          <div className="chat-debugger-attach-chips">
            {attachments.map((a, i) => (
              <span key={i} className="chat-debugger-chip" title={a.url.length > 48 ? a.url.slice(0, 200) : a.url}>
                {a.kind === 'image' && <Image size={12} />}
                {a.kind === 'video' && <Video size={12} />}
                {a.kind === 'audio' && <AudioLines size={12} />}
                {a.kind === 'image' ? t('图片') : a.kind === 'video' ? t('视频') : t('音频')}
                <button type="button" onClick={() => onRemoveAttachment(i)}>×</button>
              </span>
            ))}
          </div>
        )}

        {/* 主输入框 */}
        <textarea
          rows={2}
          placeholder={isRecording ? t('录音中…再次点击麦克风结束') : isTranscribing ? t('语音转文字中…') : t('输入消息，Enter 发送，Shift+Enter 换行')}
          value={input}
          onChange={(e) => onInputChange(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={isBusy}
          onPaste={handlePaste}
        />

        {/* 底部元信息区 */}
        <div className="chat-debugger-input-meta">
          <div className="chat-debugger-input-left">
            {/* 附件菜单 */}
            <div className="chat-debugger-attach-menu" ref={attachMenuRef}>
              <button
                type="button"
                className="chat-debugger-icon-btn"
                title={t('添加附件')}
                onClick={() => setShowAttachMenu((v) => !v)}
                disabled={isBusy}
              >
                <Plus size={15} />
              </button>
              {showAttachMenu && (
                <div className="chat-debugger-attach-pop">
                  <button type="button" onClick={() => handlePickLocalFile('image/*')}>
                    <Image size={13} /> {t('上传图片')}
                  </button>
                  <button type="button" onClick={() => handlePickLocalFile('video/*')}>
                    <Video size={13} /> {t('上传视频')}
                  </button>
                  <button type="button" onClick={() => handlePickLocalFile('audio/*')}>
                    <AudioLines size={13} /> {t('上传音频')}
                  </button>
                  <button
                    type="button"
                    onClick={() => { setShowAttachMenu(false); setShowUrlInput(true); }}
                  >
                    <Link2 size={13} /> {t('粘贴媒体 URL')}
                  </button>
                </div>
              )}
            </div>

            {/* 上下文估算 */}
            <div className="chat-debugger-context" title={t('上下文 ≈')}>
              {t('上下文 ≈')} {contextEstimate.toLocaleString()} tokens
            </div>
          </div>

          <div className="chat-debugger-input-actions">
            {/* 清空按钮 */}
            <button
              type="button"
              className="btn btn-outline btn-sm"
              onClick={onClear}
              disabled={isBusy || !attachments.length && !input.trim()}
              title={t('清空对话')}
            >
              <Send size={14} style={{ transform: 'rotate(180deg)' }} />
            </button>

            {/* 发送/停止/录音按钮 */}
            <button
              type="button"
              className={`chat-debugger-send-fab ${isRecording ? 'recording' : ''} ${isTranscribing ? 'busy' : ''}`}
              onClick={handleSendClick}
              disabled={isTranscribing || (!isBusy && !input.trim() && !attachments.length && !navigator.mediaDevices?.getUserMedia)}
              title={isBusy ? t('停止生成') : (!input.trim() && !attachments.length) ? t('语音输入') : t('发送')}
            >
              {isBusy ? <Square size={14} /> : (!input.trim() && !attachments.length) ? <Mic size={15} /> : <Send size={15} />}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
