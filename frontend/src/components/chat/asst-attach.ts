/**
 * 附件模型（对齐 A2A v3.5.1 `islands/assistant-attachments.ts` + `ui-shared/assistant-files.ts`）。
 *
 * ── 两类附件，两套限制 ───────────────────────────────────────
 * 图片：≤ 6 张、单张 ≤ 10 MB，读成 data URL（**不是** blob URL：会话要落库，
 * blob URL 只在当前页面生命周期有效，重开历史全裂）；
 * 文本（JSON / .info 凭证）：≤ 3 个、单个 ≤ 512 KB，读成字符串随消息保存。
 *
 * ── 为什么「分拣」要是纯函数 ─────────────────────────────────
 * A2A 把这套逻辑写在 hook 里（`useAssistantAttachments`），判断与 toast 混在
 * 一起，没法单测「超限的第 7 张到底进没进去」。这里拆成 `planAttachmentAdd`
 * 只算「收哪些、跳哪些、为什么」，由渲染层决定怎么提示 —— 上限变了只改一处。
 *
 * ── 发给模型的正文 ──────────────────────────────────────────
 * 文件**不是**全文塞进每一轮请求：元信息 + 顶层字段 + 前 1200 字符，完整内容
 * 由工具按需读取（A2A 的 `read_attachment`；AIGX 目前无该工具，见交付清单）。
 * 不这么做，一份几十 KB 的凭证 JSON 会在每次工具来回里重复占上下文。
 */

/** 一次最多几张图（与文本文件分开计数） */
export const MAX_IMAGES = 6;
/** 单张图片字节上限 */
export const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
/** 一次最多几个文本附件 */
export const MAX_TEXT_FILES = 3;
/** 单个文本附件字节上限（凭证 JSON 通常只有几 KB） */
export const MAX_TEXT_FILE_BYTES = 512 * 1024;
/** 每轮请求里每个文件最多带多少字符的预览 */
export const MAX_FILE_PREVIEW_CHARS = 1200;
/** 图片长边上限（像素）：超过就等比缩小 */
export const MAX_IMAGE_EDGE = 1600;
/** data URL 字符数上限（≈0.9 MB 二进制）：超过就重编码一次 */
export const MAX_IMAGE_CHARS = 1_200_000;

/** 文本附件（随消息落库；`text` 是原文） */
export interface AssistantTextFile {
  name: string;
  /** 原始字节数（展示用；文本长度另算） */
  size: number;
  text: string;
}

/** 只需要三个字段，`File` 与测试里的假对象都满足 */
export interface FileLike {
  name: string;
  size: number;
  type: string;
}

export function isAssistantTextFile(value: unknown): value is AssistantTextFile {
  if (!value || typeof value !== 'object') return false;
  const file = value as Record<string, unknown>;
  return typeof file.name === 'string'
    && typeof file.text === 'string'
    && (typeof file.size === 'number' || file.size === undefined);
}

/** 宽松读取会话里存的文件数组（手改过的历史不让整条会话打不开） */
export function assistantFiles(value: unknown): AssistantTextFile[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter(isAssistantTextFile)
    .map((file) => ({
      name: file.name,
      size: Number.isFinite(file.size) ? Number(file.size) : file.text.length,
      text: file.text,
    }));
}

/**
 * 可以按文本读进来的附件：JSON / .info / 纯文本（`.txt` / `.md`）。
 *
 * 判据必须与输入框的 `accept` 一致（`AsstComposer` 写的是
 * `.json,.txt,.md,.info,application/json,text/plain`）—— 曾经这里只认
 * `.json`/`.info`，用户按界面提示选 `.txt` 会被静默判成「不支持的类型」跳过。
 *
 * 同时按 MIME 兜底：Windows 上 `.json` 的 MIME 经常是空串（注册表缺失），
 * 只看 `file.type` 会把正常凭证文件判成「不支持」。
 */
export function isTextCredentialFile(file: FileLike): boolean {
  return (
    /\.(json|info|txt|md)$/i.test(file.name) ||
    file.type === 'application/json' ||
    file.type === 'text/plain' ||
    file.type === 'text/markdown'
  );
}

/** 跳过原因（渲染层据此选文案：这里是判据，不是文案） */
export type AttachmentSkipReason =
  | 'unsupported-type'
  | 'image-too-large'
  | 'too-many-images'
  | 'file-too-large'
  | 'too-many-files';

export interface AttachmentPlan {
  /** 收下的图片 */
  images: FileLike[];
  /** 收下的文本附件 */
  textFiles: FileLike[];
  /** 被跳过的（带原因与文件名，界面据此逐条提示） */
  skipped: Array<{ name: string; reason: AttachmentSkipReason }>;
}

export interface AttachmentState {
  /** 已添加的图片张数 */
  imageCount: number;
  /** 已添加的文本文件数 */
  fileCount: number;
  /** 正在读取的张数（读取中也要占额度，否则快速连点能突破上限） */
  pendingImages?: number;
  pendingFiles?: number;
}

/**
 * 分拣一批新拖进/粘进来的文件。
 *
 * 上限按「已有 + 读取中 + 本次」三段一起算：读取是异步的（图片要过 canvas
 * 缩采样），只算已完成的会把「快速连拖 10 张」判成合法。
 */
export function planAttachmentAdd(
  state: AttachmentState,
  incoming: readonly FileLike[],
): AttachmentPlan {
  const images: FileLike[] = [];
  const textFiles: FileLike[] = [];
  const skipped: AttachmentPlan['skipped'] = [];
  let imageRoom = MAX_IMAGES - state.imageCount - (state.pendingImages ?? 0);
  let fileRoom = MAX_TEXT_FILES - state.fileCount - (state.pendingFiles ?? 0);

  for (const file of incoming) {
    const isImage = file.type.startsWith('image/');
    if (isImage) {
      if (file.size > MAX_IMAGE_BYTES) {
        skipped.push({ name: file.name, reason: 'image-too-large' });
        continue;
      }
      if (imageRoom <= 0) {
        skipped.push({ name: file.name, reason: 'too-many-images' });
        continue;
      }
      imageRoom -= 1;
      images.push(file);
      continue;
    }
    if (!isTextCredentialFile(file)) {
      skipped.push({ name: file.name, reason: 'unsupported-type' });
      continue;
    }
    if (file.size > MAX_TEXT_FILE_BYTES) {
      skipped.push({ name: file.name, reason: 'file-too-large' });
      continue;
    }
    if (fileRoom <= 0) {
      skipped.push({ name: file.name, reason: 'too-many-files' });
      continue;
    }
    fileRoom -= 1;
    textFiles.push(file);
  }
  return { images, textFiles, skipped };
}

/** 文本读出来的结果（空文件 / 二进制 / 正常，界面按三种形态分别提示） */
export type TextFileOutcome =
  | { kind: 'ok'; file: AssistantTextFile }
  | { kind: 'empty' }
  | { kind: 'binary' };

/** 去掉 BOM 后判定：空文件与二进制（含 NUL）都不该进附件列表 */
export function textFileOutcome(name: string, size: number, raw: string): TextFileOutcome {
  const text = String(raw ?? '').replace(/^\uFEFF/, '');
  if (!text.trim()) return { kind: 'empty' };
  if (text.includes('\u0000')) return { kind: 'binary' };
  return { kind: 'ok', file: { name, size, text } };
}

export function formatFileSize(bytes: number): string {
  const value = Math.max(0, Math.round(Number(bytes) || 0));
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) {
    const kb = value / 1024;
    return `${kb >= 10 ? Math.round(kb) : kb.toFixed(1).replace(/\.0$/, '')} KB`;
  }
  const mb = value / 1024 / 1024;
  return `${mb >= 10 ? Math.round(mb) : mb.toFixed(1).replace(/\.0$/, '')} MB`;
}

/** 顶层字段名（给模型一个「这是什么形状」的摘要） */
export function jsonTopLevelKeys(text: string): string[] {
  try {
    const parsed: unknown = JSON.parse(text);
    if (Array.isArray(parsed)) return [`数组（${parsed.length} 项）`];
    if (parsed && typeof parsed === 'object') {
      return Object.keys(parsed as Record<string, unknown>).slice(0, 20);
    }
    return [];
  } catch {
    return [];
  }
}

/**
 * 单个文件在模型侧的一段说明。
 *
 * 这里是**发给模型的提示词正文**，不是界面文案：固定中文、不过 i18n。
 * 走 `t()` 会让同一段对话的提示词随界面语言漂移（英文界面的模型会收到英文
 * 注解），也会把这些串塞进 zh/en.json —— 而 `.ts` 里的 `t('…')` 字面量
 * 根本不在 `scripts/i18n-fill-missing.mjs` 的扫描范围（它只走 `.tsx`），
 * 结果是「看起来做了 i18n、实际英文界面回退中文」。
 */
function fileBlock(file: AssistantTextFile, index: number): string {
  const ordinal = index + 1;
  const keys = jsonTopLevelKeys(file.text);
  const shape = keys.length
    ? `顶层字段：${keys.join(', ')}`
    : '不是合法 JSON（可先用工具读取，再修正结构）';
  const truncated = file.text.length > MAX_FILE_PREVIEW_CHARS;
  const preview = truncated
    ? `${file.text.slice(0, MAX_FILE_PREVIEW_CHARS)}\n…（预览已截断，剩余 ${file.text.length - MAX_FILE_PREVIEW_CHARS} 字符未显示）`
    : file.text;
  const hint = truncated
    ? `完整内容请用工具读取附件 ${ordinal}；导入时可直接引用 attachment:${ordinal}。`
    : `完整内容已在上方；导入时可引用 attachment:${ordinal}。`;
  return [
    `【附件 ${ordinal}：${file.name}（${formatFileSize(file.size)}，${file.text.length} 字符）】`,
    shape,
    '',
    '--- 内容开始 ---',
    preview,
    '--- 内容结束 ---',
    hint,
  ].join('\n');
}

/**
 * 一条用户消息 + 附件 → 发给上游的文本正文。
 *
 * 纯文本仍保持字符串形态（上游对字符串兼容性最好），附件说明追加在正文后面；
 * 带图时由调用方把这段文本塞进第一个 text part。
 */
export function userContentWithFiles(
  content: string,
  files: readonly AssistantTextFile[],
): string {
  if (!files.length) return content;
  const blocks = files.map((file, index) => fileBlock(file, index)).join('\n\n');
  return `${content.trim() ? `${content}\n\n` : ''}【用户上传的文件】\n\n${blocks}`;
}

/* ─── 图片：读入 + 降采样 ─────────────────────────────────── */

export function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ''));
    reader.onerror = () => reject(reader.error ?? new Error('read failed'));
    reader.readAsDataURL(file);
  });
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error('image decode failed'));
    image.src = src;
  });
}

/**
 * 读图并按需降采样。
 *
 * 截图动辄 2560 宽，原图 base64 会让会话 JSON 涨到几 MB（后端单条会话有上限），
 * 表现为「对话还在屏幕上、但没进历史」。GIF 不重编码（canvas 只留第一帧，
 * 动画就没了）；其余按长边缩到 `MAX_IMAGE_EDGE`，体积仍超时转 WebP，两者取
 * 字符数更小的一份。
 */
export async function prepareImage(file: File): Promise<string> {
  const raw = await readAsDataUrl(file);
  if (file.type === 'image/gif') return raw;
  const image = await loadImage(raw);
  const longest = Math.max(image.naturalWidth, image.naturalHeight);
  const scale = longest > 0 ? Math.min(1, MAX_IMAGE_EDGE / longest) : 1;
  if (scale === 1 && raw.length <= MAX_IMAGE_CHARS) return raw;
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(image.naturalWidth * scale));
  canvas.height = Math.max(1, Math.round(image.naturalHeight * scale));
  const context = canvas.getContext('2d');
  if (!context) return raw;
  context.drawImage(image, 0, 0, canvas.width, canvas.height);
  const encoded = canvas.toDataURL('image/webp', 0.86);
  return encoded.length < raw.length ? encoded : raw;
}

/* ─── 与模型能力的兼容 ─────────────────────────────────────── */

/**
 * 有图但当前模型不支持读图 → 提示（**不拦发送**）。
 *
 * A2A 的原话是「只提示，不拦：用户可能就是想想试，网关会回明确错误」。
 * 这一点必须照抄 —— 这里静默清空图片会让人以为「我贴的图丢了」。
 * `supportsImages` 缺省（模型列表里没有这个模型 / 元信息未声明）时**不提示**：
 * 「不知道」不等于「不支持」。
 */
export function imageIncompatibility(images: readonly string[], supportsImages: boolean | undefined): boolean {
  return images.length > 0 && supportsImages === false;
}

/** 上下文用完/切换会话时清附件（A2A 在发送成功、切会话、新建对话三处清） */
export function clearAttachments(): { images: string[]; files: AssistantTextFile[] } {
  return { images: [], files: [] };
}
