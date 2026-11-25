/**
 * /chat 的消息流渲染（对齐 A2A v3.5.1 `islands/assistant-messages.tsx`）。
 *
 * ── 两条视觉口径（与 A2A 完全一致）──────────────────────────
 *   · 用户消息 = 右侧浅底气泡，最大宽度 80%；
 *   · 助手消息 = **不套气泡**，占满 840px 内容列（正文里要放工具卡与代码块，
 *     气泡会把它们压窄），左侧一枚主色头像 + `模型名 / 时间` 细行。
 *
 * ── 与 A2A 的差别（AIGX 独有，全部保留）────────────────────
 *   · 工具卡：AIGX 用既有的 `ToolCallCard`（#63 分片聚合、参数/结果分块复制、
 *     用量收尾、估算角标）—— A2A 的工具卡是它自己的 `asst-tool` 三态卡；
 *   · 深度思考：`reasoning` 走 `MessageViewer` 的折叠块（A2A 只把正文送
 *     Markdown，没有 reasoning 概念）；
 *   · 停止/失败态：`toolState` 的 interrupted / error 决定气泡尾部标记。
 */
import { useTranslation } from 'react-i18next';
import { Copy, Check, Pencil, Trash2, RefreshCw, GitFork } from 'lucide-react';
import MessageViewer from '../MessageViewer';
import ToolCallCard from './ToolCallCard';
import { ChatAvatar, ChatFileIcon } from '../../pages/chat-icons';
import { messageActions } from './asst-message-ops';
import type { DebugMessage } from './types';

/** 字节数 → 人性化尺寸。
 *
 *  刻意**不从 `asst-attach.ts` 取**：那个模块是另一位成员在同一目录里建的附件
 *  限额层，本文件只需要一个纯展示函数；跨文件借一个格式化函数会让两块工作互相
 *  卡住（对方改签名即红），所以这里自带一份最小实现（1 位小数，与 A2A 的
 *  `formatFileSize` 同口径）。 */
function formatSize(bytes: number): string {
  const value = Math.max(0, Math.round(Number(bytes) || 0));
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1).replace(/\.0$/, '')} KB`;
  return `${(value / (1024 * 1024)).toFixed(1).replace(/\.0$/, '')} MB`;
}

export interface AsstMessagesProps {
  messages: DebugMessage[];
  /** 会话当前模型（顶栏/胶囊同源） */
  model: string;
  /** 生成中：禁止编辑/删除/重试 */
  busy: boolean;
  /** 正在被复制的那条（index）*/
  copiedIndex: number | null;
  onCopy: (index: number) => void;
  onBeginEdit: (index: number) => void;
  onDelete: (index: number) => void;
  onRegenerate: (index: number) => void;
  onFork: (index: number) => void;
}

function timeLabel(timestamp?: number): string {
  if (!timestamp) return '';
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) return '';
  try {
    return date.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
  } catch {
    return '';
  }
}

export default function AsstMessages({
  messages,
  model,
  busy,
  copiedIndex,
  onCopy,
  onBeginEdit,
  onDelete,
  onRegenerate,
  onFork,
}: AsstMessagesProps): JSX.Element {
  const { t } = useTranslation();

  return (
    <>
      {messages.map((message, index) => {
        const isUser = message.role === 'user';
        const isLast = index === messages.length - 1;
        const toolState = message.toolState === 'interrupted'
          ? 'interrupted'
          : message.toolState === 'error'
            ? 'error'
            : message.toolState === 'streaming'
              ? 'streaming'
              : 'done';

        if (isUser) {
          /**
           * 判据统一走 `asst-message-ops.ts`（对齐 A2A）：忙碌时编辑/删除都不执行、
           * 空消息不给复制。此前这里是三个裸 `disabled={busy}` + 无条件复制按钮，
           * 空消息（编辑后未填）也能点复制，复制出来是空串。
           */
          const actions = messageActions({
            role: 'user',
            content: message.content,
            isLast,
            busy,
          });
          return (
            <div className="asst-msg user" key={index}>
              <div className="asst-msg-usercol">
                <div className="asst-bubble">
                  {message.attachments?.length ? (
                    <div className="asst-msg-atts">
                      {message.attachments.map((attachment, i) => (
                        <div className="asst-msg-att" key={i}>
                          {attachment.kind === 'image' && (
                            <img className="asst-msg-shot" src={attachment.url} alt={t('图片 {{n}}', { n: i + 1 })} loading="lazy" />
                          )}
                          {attachment.kind === 'video' && <video className="asst-msg-shot" src={attachment.url} controls muted />}
                          {attachment.kind === 'audio' && <audio src={attachment.url} controls />}
                        </div>
                      ))}
                    </div>
                  ) : null}
                  {/* 文本附件（JSON / .info）：与图片同泡，形态同 A2A 的 asst-msg-files */}
                  {message.files?.length ? <AsstFileChips files={message.files} /> : null}
                  {message.content ? <p>{message.content}</p> : null}
                </div>
                <div className="asst-msg-acts">
                  {actions.copy && (
                    <button
                      type="button"
                      className="asst-msg-act"
                      title={t('复制')}
                      aria-label={t('复制')}
                      onClick={() => onCopy(index)}
                    >
                      {copiedIndex === index ? <Check size={12} /> : <Copy size={12} />}
                    </button>
                  )}
                  {actions.edit && (
                    <button
                      type="button"
                      className="asst-msg-act"
                      title={t('编辑并重发')}
                      aria-label={t('编辑并重发')}
                      onClick={() => onBeginEdit(index)}
                    >
                      <Pencil size={12} />
                    </button>
                  )}
                  {actions.remove && (
                    <button
                      type="button"
                      className="asst-msg-act"
                      title={t('删除这条')}
                      aria-label={t('删除这条')}
                      onClick={() => onDelete(index)}
                    >
                      <Trash2 size={12} />
                    </button>
                  )}
                </div>
              </div>
            </div>
          );
        }

        return (
          <div className="asst-msg bot" key={index}>
            <ChatAvatar className="asst-avatar" />
            <div className="asst-msg-body">
              <div className="asst-msg-who">
                <b>{model || t('助手')}</b>
                <span>{timeLabel(Date.now())}</span>
                {/* #37：中止标记只出现**一处**。有工具卡片时由卡片的「停止」徽标
                    承担（ToolCallCard 的 interrupted 态），气泡里不再叠一份文字；
                    没有工具卡片的纯文本回复才在 who 行标「已停止生成」。 */}
                {message.toolState === 'interrupted' && !message.toolCalls?.length && (
                  <em className="asst-msg-flag stop" data-testid="msg-stopped">{t('已停止生成')}</em>
                )}
                {message.toolState === 'error' && (
                  <em className="asst-msg-flag bad" data-testid="msg-error">{t('上游报错')}</em>
                )}
              </div>

              {message.content
                ? <MessageViewer content={message.content} reasoning={message.reasoning} />
                : message.toolState === 'streaming'
                  ? <span className="asst-caret" />
                  : message.reasoning
                    ? <MessageViewer content="" reasoning={message.reasoning} />
                    : null}
              {/* AIGX 独有：工具调用卡（#63）。嵌在助手消息里，与 A2A 的
                  「工具卡在助手消息内」同一层级。 */}
              {message.toolCalls?.length ? (
                <ToolCallCard calls={message.toolCalls} state={toolState} usage={message.usage} />
              ) : message.usage ? (
                <ToolCallCard calls={[]} state={toolState} usage={message.usage} />
              ) : null}

              {/* 操作条：判据走 `messageActions`（忙碌一律不执行、空消息不给复制、
                  重生成只对「最后一条有正文的助手消息」）。流式中的最后一条整条隐藏：
                  正在生成，按钮全是无意义的；停止/失败态保留 —— 用户要能复制已收到
                  的内容、重试或分叉。 */}
              {(() => {
                const actions = messageActions({
                  role: 'assistant',
                  content: message.content,
                  streaming: message.toolState === 'streaming',
                  isLast,
                  busy,
                  hasToolCalls: Boolean(message.toolCalls?.length),
                });
                if (isLast && message.toolState === 'streaming') return null;
                return (
                  <div className="asst-msg-acts">
                    {actions.copy && (
                      <button
                        type="button"
                        className="asst-msg-act"
                        title={t('复制')}
                        aria-label={t('复制')}
                        onClick={() => onCopy(index)}
                      >
                        {copiedIndex === index ? <Check size={12} /> : <Copy size={12} />}
                      </button>
                    )}
                    {actions.regenerate && (
                      <button
                        type="button"
                        className="asst-msg-act"
                        title={t('重新生成')}
                        aria-label={t('重新生成')}
                        onClick={() => onRegenerate(index)}
                      >
                        <RefreshCw size={12} />
                      </button>
                    )}
                    {actions.fork && (
                      <button
                        type="button"
                        className="asst-msg-act"
                        title={t('从此条分叉')}
                        aria-label={t('从此条分叉')}
                        onClick={() => onFork(index)}
                      >
                        <GitFork size={12} />
                      </button>
                    )}
                    {actions.remove && (
                      <button
                        type="button"
                        className="asst-msg-act"
                        title={t('删除这条')}
                        aria-label={t('删除这条')}
                        onClick={() => onDelete(index)}
                      >
                        <Trash2 size={12} />
                      </button>
                    )}
                  </div>
                );
              })()}
            </div>
          </div>
        );
      })}
    </>
  );
}

/** 用户消息里附件之外的文件条（JSON / .info 文本附件）——AIGX 的 composer 支持，
 *  这里给历史回放留同一形态（A2A 的 asst-msg-files）。传 onRemove 时渲染移除键。 */
export function AsstFileChips({ files, onRemove }: {
  files: Array<{ name: string; size: number }>;
  onRemove?: (index: number) => void;
}): JSX.Element | null {
  const { t } = useTranslation();
  if (!files.length) return null;
  return (
    <div className="asst-msg-files">
      {files.map((file, index) => (
        <span className="asst-msg-file" key={`${file.name}-${index}`} title={file.name}>
          <ChatFileIcon size={13} />
          <span className="asst-msg-file-name">{file.name}</span>
          <span className="asst-msg-file-size">{formatSize(file.size)}</span>
          {onRemove && (
            <button
              type="button"
              className="asst-msg-file-x"
              title={t('移除')}
              aria-label={t('移除')}
              onClick={() => onRemove(index)}
            >
              ✕
            </button>
          )}
        </span>
      ))}
    </div>
  );
}
