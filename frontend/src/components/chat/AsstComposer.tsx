/**
 * /chat 的输入区（对齐 A2A v3.5.1 `islands/assistant-composer.tsx`）。
 *
 * ── 结构（与 A2A 逐层对应）──────────────────────────────────
 *   .asst-composer-wrap
 *     .asst-composer
 *       附件行（图片缩略图 + 文本附件条）
 *       自适应高度文本域（Enter 发送 / Shift+Enter 换行 / 粘贴图片）
 *       底部工具条：+ 附件 · 权限胶囊 …… 思考强度 · 模型·等级胶囊 · 发送
 *
 * ── 为什么模型与等级合成一个胶囊 ─────────────────────────────
 * 与 A2A 同一理由：它们是**同一次请求的两个参数**，各占一个下拉时用户既看不出
 * 是一组，也看不出当前组合。点开先看到两行菜单（模型 / 推理等级），点哪行走进
 * 对应子列表 —— A2A 用 Base UI 的 Popover 三屏，这里用 AsstPopover 复刻同样三屏。
 *
 * ── AIGX 独有：思考强度快捷 pill ────────────────────────────
 * `EffortPicker`（4 档：关/低/中/高）是 AIGX 既有的循环 pill，与胶囊里的
 * 8 档 REASONING_LEVELS 是**同一份状态**的两种粒度：快捷 pill 只在 4 档间循环，
 * 选中高粒度档位（xhigh/max）时显示为「思考 高」而不会改写配置。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import EffortPicker, { type Effort } from './EffortPicker';
import AsstPopover from './AsstPopover';
import {
  ChatBoltIcon,
  ChatCheckIcon,
  ChatChevronIcon,
  ChatFileIcon,
  ChatImageIcon,
  ChatLayersIcon,
  ChatMiniChevronIcon,
  ChatPlusIcon,
  ChatSearchIcon,
  ChatSendIcon,
  ChatShieldIcon,
  ChatSlidersIcon,
  ChatStopIcon,
} from '../../pages/chat-icons';
import { AsstFileChips } from './AsstMessages';
import { REASONING_LEVELS } from '../../pages/chat-state';
import { MAX_IMAGE_BYTES, MAX_IMAGES } from './asst-attach';

/** 附件上限从 `asst-attach` 再导出：判据只有一份（那边同时有文本附件上限）。
 *  此前 composer 自带一份 `6 / 10MB`，与 `planAttachmentAdd` 的两个常量并存 ——
 *  改一处忘一处就是「界面说 6 张、实际收了 3 张」这类对不上的 bug。 */
export { MAX_IMAGE_BYTES, MAX_IMAGES };

/** 字节数 → 人性化尺寸（composer 底部的附件合计用；纯展示，自带实现） */
function formatSize(bytes: number): string {
  const value = Math.max(0, Math.round(Number(bytes) || 0));
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1).replace(/\.0$/, '')} KB`;
  return `${(value / (1024 * 1024)).toFixed(1).replace(/\.0$/, '')} MB`;
}

export type PermissionMode = 'confirm' | 'full';

export interface AsstTextFile {
  name: string;
  size: number;
  text: string;
}

export interface AsstComposerProps {
  draft: string;
  onDraftChange: (value: string) => void;
  images: string[];
  files: AsstTextFile[];
  onAddImages: (files: FileList | File[]) => void;
  onAddFiles: (files: FileList | File[]) => void;
  onRemoveImage: (index: number) => void;
  onRemoveFile: (index: number) => void;
  models: string[];
  model: string | null;
  modelsLoading: boolean;
  onRefreshModels: () => void;
  onPickModel: (name: string) => void;
  reasoning: string | null;
  onPickReasoning: (level: string | null) => void;
  mode: PermissionMode;
  /** 权限模式在 AIGX 对话链路上暂不生效（见交付清单），这里只落形态 */
  modeDisabled?: boolean;
  onModeChange: (mode: PermissionMode) => void;
  effort: Effort;
  onPickEffort: (effort: Effort) => void;
  busy: boolean;
  awaiting: boolean;
  canSend: boolean;
  onSend: () => void;
  onStop: () => void;
  /* ── 以下为判据层接线（asst-model.ts / asst-attach.ts）───────────
   * 全部可选：缺省时行为与接线前逐字一致（少一层提示，不多一次误判）。 */
  /**
   * 当前模型是否还在可用列表里（`modelAvailability().available`）。
   * `false` 时只在胶囊旁给一行警告 —— **不清空已选模型**（列表刷新失败一次
   * 就丢掉用户的模型，比「可能失效」更糟）。
   */
  modelAvailable?: boolean;
  /**
   * 当前模型是否支持读图（`modelSupportsImages()`）。
   * `false` 才提示；`undefined`（未声明）= 不提示。
   */
  modelSupportsImages?: boolean;
  /** 推理等级候选（`resolveEffortOptions().options`；缺省用 A2A 八档表） */
  effortLevels?: string[];
  /** 模型声明了、但本网关不认的档位（`resolveEffortOptions().dropped`） */
  effortDropped?: string[];
  /** 附件/文件正在读取（读取期间禁用发送，避免半份附件被发出去） */
  reading?: boolean;
}

/** 等级展示：内部存小写，界面统一首字母大写（Max / High…） */
function levelLabel(level: string | null, follow: string): string {
  if (!level) return follow;
  return level === 'xhigh' ? 'xHigh' : level.charAt(0).toUpperCase() + level.slice(1);
}

export default function AsstComposer(props: AsstComposerProps): JSX.Element {
  const { t } = useTranslation();
  const {
    draft, onDraftChange, images, files, onAddImages, onAddFiles, onRemoveImage, onRemoveFile,
    models, model, modelsLoading, onRefreshModels, onPickModel,
    reasoning, onPickReasoning, mode, modeDisabled, onModeChange,
    effort, onPickEffort, busy, awaiting, canSend, onSend, onStop,
    modelAvailable, modelSupportsImages, effortLevels, effortDropped, reading,
  } = props;

  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const imgRef = useRef<HTMLInputElement | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [dragging, setDragging] = useState(false);
  const [modelOpen, setModelOpen] = useState(false);
  const [modeOpen, setModeOpen] = useState(false);
  const [pane, setPane] = useState<'root' | 'model' | 'reason'>('root');
  const [keyword, setKeyword] = useState('');

  const disabled = busy || awaiting;
  const displayModel = model || t('选择模型…');
  /**
   * 有图但当前模型明确不支持读图 → **只提示不拦**。
   *
   * 与 A2A 同一口径（`assistant-composer.tsx:115` 的原话是「只提示，不拦：
   * 用户可能就是想想试，网关会回明确错误」）。这里静默丢掉图片会让人以为
   * 「我贴的图丢了」，那是更坏的结果。`modelSupportsImages === undefined`
   * （模型未声明能力）不提示 —— 「不知道」不等于「不支持」。
   */
  const imageWarning = images.length > 0 && modelSupportsImages === false;
  /** 模型清单已拉到但当前模型不在其中：提示，但不改选中的值 */
  const modelWarning = modelAvailable === false && !modelsLoading;
  /** 候选档位：缺省用 A2A 八档表（调用方传了就用调用方的，超范围档已剔除） */
  const levelOptions = effortLevels && effortLevels.length ? effortLevels : REASONING_LEVELS;

  /* 自增高（上限 168px ≈ 8 行）。+2px 是亚像素差的余量：scrollHeight 取整后
     常比实际内容矮半像素，不留余量浏览器会误判「还差一行」而挂上滚动条。 */
  const autoGrow = useCallback(() => {
    const el = taRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight + 2, 168)}px`;
  }, []);
  useEffect(autoGrow, [draft, autoGrow]);

  /* 弹层关闭时回到第一屏，下次打开看到的是两行菜单 */
  useEffect(() => {
    if (!modelOpen) {
      setPane('root');
      setKeyword('');
    }
  }, [modelOpen]);

  const filtered = useMemo(() => {
    const key = keyword.trim().toLowerCase();
    return key ? models.filter((name) => name.toLowerCase().includes(key)) : models;
  }, [models, keyword]);

  /**
   * 一批文件按类型分流（A2A 只有一个 `onAddFiles` 入口、由 hook 内部分拣；
   * AIGX 这里分成两个回调，所以分流的判据落在组件里）。
   *
   * 粘贴/拖拽**必须走这里**而不是直接 `onAddImages`：用户从资源管理器拖一个
   * `.json` 进来时，旧实现会把它当图片送进 canvas 解码 → 解不出 → 静默消失。
   */
  const routeFiles = useCallback((picked: FileList | File[]): void => {
    const list = Array.from(picked);
    const pictureFiles = list.filter((file) => file.type.startsWith('image/'));
    const textFiles = list.filter((file) => !file.type.startsWith('image/'));
    if (pictureFiles.length) onAddImages(pictureFiles);
    if (textFiles.length) onAddFiles(textFiles);
  }, [onAddFiles, onAddImages]);

  const onPaste = (event: React.ClipboardEvent<HTMLTextAreaElement>): void => {
    const items = event.clipboardData?.items;
    if (!items) return;
    const picked: File[] = [];
    for (let i = 0; i < items.length; i += 1) {
      if (items[i].kind !== 'file') continue;
      const file = items[i].getAsFile();
      if (file) picked.push(file);
    }
    if (picked.length) {
      event.preventDefault();
      routeFiles(picked);
    }
  };

  const modeText = mode === 'full' ? t('完全访问') : t('变更前确认');

  return (
    <div className="asst-composer-wrap">
      <div
        className={`asst-composer${dragging ? ' drag' : ''}`}
        onDragOver={(event) => {
          if (!event.dataTransfer?.types.includes('Files')) return;
          event.preventDefault();
          setDragging(true);
        }}
        onDragLeave={(event) => {
          if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
          setDragging(false);
        }}
        onDrop={(event) => {
          event.preventDefault();
          setDragging(false);
          if (event.dataTransfer?.files?.length) routeFiles(event.dataTransfer.files);
        }}
      >
        {images.length > 0 && (
          <div className="asst-atts">
            {images.map((url, index) => (
              <div className="asst-att" key={index}>
                <img className="asst-att-img" src={url} alt={t('图片 {{n}}', { n: index + 1 })} />
                <button
                  type="button"
                  className="asst-att-x"
                  title={t('移除')}
                  aria-label={t('移除')}
                  onClick={() => onRemoveImage(index)}
                >
                  ✕
                </button>
              </div>
            ))}
          </div>
        )}

        {files.length > 0 && (
          <AsstFileChips
            files={files.map((file) => ({ name: file.name, size: file.size }))}
            onRemove={onRemoveFile}
          />
        )}

        <textarea
          ref={taRef}
          rows={1}
          value={draft}
          disabled={disabled}
          placeholder={disabled
            ? t('等这一轮跑完…')
            : t('发消息或做任务…（Enter 发送，Shift+Enter 换行，可粘贴图片）')}
          onChange={(event) => onDraftChange(event.currentTarget.value)}
          onPaste={onPaste}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
              event.preventDefault();
              if (canSend) onSend();
            }
          }}
        />

        {files.length > 0 && (
          <div className="asst-composer-warn">
            <ChatFileIcon size={13} />
            {t('文本附件会以文本发送给当前对话模型，其中的凭证也会保存在这条会话里。')}
          </div>
        )}

        {/* 附件读取中：图片要过 canvas 降采样、文本要读全文，都是异步的。
            读取期间不禁用输入，但要让用户看见「还有东西在路上」。 */}
        {reading && (
          <div className="asst-composer-warn" data-testid="asst-attach-reading">
            <ChatImageIcon size={13} />
            {t('正在读取附件…')}
          </div>
        )}

        {/* 有图但模型明确不支持读图：只提示，不拦发送（A2A 同口径） */}
        {imageWarning && (
          <div className="asst-composer-warn" data-testid="asst-image-warning">
            <ChatImageIcon size={13} />
            {t('当前模型未声明支持读图：图片仍会发送，上游可能返回明确错误。')}
          </div>
        )}

        {/* 模型清单已拉到但当前模型不在其中：提示且不改选中值 */}
        {modelWarning && (
          <div className="asst-composer-warn" data-testid="asst-model-warning">
            <ChatSlidersIcon size={13} />
            {t('当前模型不在可用模型列表里，请重新选择，或到「模型管理」检查是否已启用。')}
          </div>
        )}

        <div className="asst-bar">
          {/* 左：添加图片 / 文本文件 */}
          <input
            ref={imgRef}
            type="file"
            accept="image/*"
            multiple
            hidden
            onChange={(event) => {
              if (event.currentTarget.files?.length) routeFiles(event.currentTarget.files);
              event.currentTarget.value = '';
            }}
          />
          <input
            ref={fileRef}
            type="file"
            accept=".json,.txt,.md,.info,application/json,text/plain"
            multiple
            hidden
            onChange={(event) => {
              if (event.currentTarget.files?.length) routeFiles(event.currentTarget.files);
              event.currentTarget.value = '';
            }}
          />
          <button
            type="button"
            className="asst-round"
            title={t('添加图片或文本文件（也可粘贴或拖入）')}
            aria-label={t('添加图片或文件')}
            onClick={() => imgRef.current?.click()}
          >
            <ChatPlusIcon size={17} />
          </button>
          <button
            type="button"
            className="asst-round"
            title={t('添加文本文件（JSON / txt）')}
            aria-label={t('添加文本文件')}
            onClick={() => fileRef.current?.click()}
          >
            <ChatFileIcon size={15} />
          </button>

          {/* 权限模式胶囊（A2A 的第二个胶囊）。AIGX 的对话链路没有工具执行通道，
              这里落形态并标注 disabled 理由，不伪装成可用。 */}
          <div className="asst-pill-wrap">
            <button
              type="button"
              data-asst-trigger="mode"
              className={`asst-pill asst-mode-pill${mode === 'full' ? ' full' : ''}${modeDisabled ? ' disabled' : ''}`}
              aria-expanded={modeOpen}
              aria-haspopup="dialog"
              disabled={modeDisabled}
              title={modeDisabled ? t('对话链路暂无工具执行通道，权限模式在调试视图里才生效') : t('权限模式')}
              onClick={() => setModeOpen((open) => !open)}
            >
              <span className="asst-pill-ico">{mode === 'full' ? <ChatBoltIcon size={14} /> : <ChatShieldIcon size={14} />}</span>
              <span>{modeText}</span>
              <ChatChevronIcon size={11} className="asst-pill-chev" />
            </button>
            <AsstPopover open={modeOpen} onClose={() => setModeOpen(false)} side="top" align="start" className="asst-pop-md">
              {(['confirm', 'full'] as const).map((value) => (
                <button
                  type="button"
                  key={value}
                  className={`asst-radio-row${mode === value ? ' on' : ''}`}
                  onClick={() => { onModeChange(value); setModeOpen(false); }}
                >
                  <span className="asst-row-ico">{value === 'full' ? <ChatBoltIcon size={15} /> : <ChatShieldIcon size={15} />}</span>
                  <span className="asst-row-text">
                    <b>{value === 'full' ? t('完全访问') : t('变更前确认')}</b>
                    <small>{value === 'full'
                      ? t('改动类工具直接执行，不再询问')
                      : t('改动类工具先列清单，等你点「执行」')}</small>
                  </span>
                  {mode === value && <ChatCheckIcon size={12} />}
                </button>
              ))}
              <p className="asst-pop-note">
                {t('只跳过确认，不扩大工具权限；每次执行仍会记录在对话里。')}
              </p>
            </AsstPopover>
          </div>

          <span className="asst-bar-spacer" />

          {/* AIGX 独有：思考强度快捷 pill（4 档循环，与胶囊的 8 档同一份状态） */}
          <EffortPicker storageKey="aigx_chat_effort" value={effort} onChange={onPickEffort} disabled={disabled} />

          {/* 右：模型 · 等级 + 发送 */}
          <div className="asst-pill-wrap">
            <button
              type="button"
              data-asst-trigger="model"
              className="asst-pill asst-model-pill"
              aria-expanded={modelOpen}
              aria-haspopup="dialog"
              title={displayModel}
              onClick={() => { setModelOpen((open) => !open); if (!modelOpen) onRefreshModels(); }}
            >
              <span className={`asst-dot${model ? ' ok' : ''}`} />
              <span className="asst-pill-model">{displayModel}</span>
              <span className="asst-pill-sep">·</span>
              <span className="asst-pill-level">{levelLabel(reasoning, t('跟随模型'))}</span>
              <ChatChevronIcon size={11} className="asst-pill-chev" />
            </button>
            <AsstPopover open={modelOpen} onClose={() => setModelOpen(false)} side="top" align="end" className="asst-pop-lg">
              {pane === 'root' && (
                <>
                  <button
                    type="button"
                    className="asst-pick-row"
                    onClick={() => { setPane('model'); onRefreshModels(); }}
                  >
                    <span className="asst-pr-ico"><ChatLayersIcon size={14} /></span>
                    <span className="asst-pr-k">{t('模型')}</span>
                    <span className="asst-pr-v"><span className="mono">{displayModel}</span><ChatMiniChevronIcon size={11} /></span>
                  </button>
                  <button type="button" className="asst-pick-row" onClick={() => setPane('reason')}>
                    <span className="asst-pr-ico"><ChatSlidersIcon size={14} /></span>
                    <span className="asst-pr-k">{t('推理等级')}</span>
                    <span className="asst-pr-v"><span>{levelLabel(reasoning, t('跟随模型'))}</span><ChatMiniChevronIcon size={11} /></span>
                  </button>
                </>
              )}

              {pane === 'model' && (
                <>
                  <div className="asst-sub-head">
                    <button type="button" className="asst-back" onClick={() => setPane('root')} aria-label={t('返回')}>
                      <ChatChevronIcon size={11} className="flip" />
                    </button>
                    <span>{t('模型')}</span>
                    <button
                      type="button"
                      className="asst-back ml-auto"
                      disabled={modelsLoading}
                      title={t('刷新列表')}
                      aria-label={t('刷新列表')}
                      onClick={onRefreshModels}
                    >
                      ↻
                    </button>
                  </div>
                  <label className="asst-search">
                    <input
                      autoFocus
                      value={keyword}
                      placeholder={t('搜索模型…')}
                      onChange={(event) => setKeyword(event.currentTarget.value)}
                    />
                    <ChatSearchIcon size={13} />
                  </label>
                  <div className="asst-sub-list">
                    {filtered.length === 0 && (
                      <div className="asst-sub-empty">
                        {modelsLoading
                          ? t('正在读取可用模型…')
                          : keyword.trim() ? t('没有匹配的模型') : t('没有可用的模型')}
                      </div>
                    )}
                    {filtered.map((name) => (
                      <button
                        type="button"
                        key={name}
                        aria-pressed={name === model}
                        className={`asst-sub-item${name === model ? ' selected' : ''}`}
                        onClick={() => { onPickModel(name); setModelOpen(false); }}
                      >
                        <span className="mono">{name}</span>
                        <span className="asst-sub-tail">{name === model && <ChatCheckIcon size={12} />}</span>
                      </button>
                    ))}
                  </div>
                </>
              )}

              {pane === 'reason' && (
                <>
                  <div className="asst-sub-head">
                    <button type="button" className="asst-back" onClick={() => setPane('root')} aria-label={t('返回')}>
                      <ChatChevronIcon size={11} className="flip" />
                    </button>
                    <span>{t('推理等级')}</span>
                    <small>{t('模型不支持时会自动忽略')}</small>
                  </div>
                  <div className="asst-sub-list">
                    <button
                      type="button"
                      aria-pressed={!reasoning}
                      className={`asst-sub-item${!reasoning ? ' selected' : ''}`}
                      onClick={() => { onPickReasoning(null); setModelOpen(false); }}
                    >
                      <span>{t('跟随模型')}</span>
                      <span className="asst-sub-tail">{!reasoning && <ChatCheckIcon size={12} />}</span>
                    </button>
                    {levelOptions.map((level) => (
                      <button
                        type="button"
                        key={level}
                        aria-pressed={reasoning === level}
                        className={`asst-sub-item${reasoning === level ? ' selected' : ''}`}
                        onClick={() => { onPickReasoning(level); setModelOpen(false); }}
                      >
                        <span className="mono">{level}</span>
                        <span className="asst-sub-tail">{reasoning === level && <ChatCheckIcon size={12} />}</span>
                      </button>
                    ))}
                  </div>
                  {/* 模型声明了、但本网关不认的档位：如实报出来，不静默丢弃
                      （选了也只会被后端忽略，用户会以为「我设了 max 怎么没变」） */}
                  {effortDropped && effortDropped.length > 0 && (
                    <p className="asst-pop-note" data-testid="asst-effort-dropped">
                      {t('该模型声明了本网关不支持的推理档位：{{levels}}', { levels: effortDropped.join(' / ') })}
                    </p>
                  )}
                </>
              )}
            </AsstPopover>
          </div>

          {busy ? (
            <button type="button" className="asst-send stop" title={t('停止')} aria-label={t('停止')} onClick={onStop}>
              <ChatStopIcon size={13} />
            </button>
          ) : (
            <button
              type="button"
              className="asst-send"
              title={t('发送')}
              aria-label={t('发送')}
              disabled={!canSend}
              onClick={onSend}
            >
              <ChatSendIcon size={16} />
            </button>
          )}
        </div>
      </div>

      {files.length > 0 && (
        <p className="asst-composer-hint">
          {t('已添加 {n} 个文本附件，共 {size}', {
            n: files.length,
            size: formatSize(files.reduce((sum, file) => sum + file.size, 0)),
          })}
        </p>
      )}
      {images.length > 0 && !imageWarning && (
        <p className="asst-composer-hint">
          <ChatImageIcon size={12} />
          {t('图片会随本条消息发送；模型不支持图片时会由上游返回明确错误。')}
        </p>
      )}
    </div>
  );
}
