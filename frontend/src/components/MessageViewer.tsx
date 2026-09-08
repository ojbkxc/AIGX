import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { BrainCircuit, ChevronDown, ChevronRight, Copy, Check } from 'lucide-react';
import './MessageViewer.css';

/**
 * MessageViewer — 消息可读渲染（P0 收尾）。
 *
 * 自写行级 Markdown 解析：无第三方依赖（外部依赖是负债），只覆盖
 * 对话场景高价值子集——fenced code（语言标签 + 一键复制 + 流式容错）、
 * 行内 code、标题、列表、引用、链接、粗体、分割线；其余按纯文本。
 * 所有 HTML 先转义再渲染，不执行任何上游内容。
 */

interface MessageViewerProps {
  /** 正文（已不含 reasoning，由 SSE 层拆分） */
  content: string;
  /** DeepSeek 式深度思考块：默认收起、可展开、可折叠 */
  reasoning?: string;
}

interface CodeBlock {
  lang: string;
  code: string;
}

interface RenderBlock {
  type: 'text' | 'code';
  html?: string;
  lang?: string;
  code?: string;
}

/** 纯文本转义：上游内容永不作为 HTML 执行 */
function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/** 行内渲染：URL 先保护再转义，避免 href 中的 & 被二次转义 */
function inlineHtml(text: string): string {
  const urls: string[] = [];
  const withPlaceholders = text.replace(
    /(^|[\s(])(https?:\/\/[^\s<>"')\]]+)/g,
    (_m, p1: string, url: string) => {
      const idx = urls.length;
      urls.push(url);
      return `${p1}\u0000URL${idx}\u0000`;
    },
  );
  let html = escapeHtml(withPlaceholders)
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  urls.forEach((url, i) => {
    const safeUrl = escapeHtml(url);
    html = html.replace(
      `\u0000URL${i}\u0000`,
      `<a href="${safeUrl}" target="_blank" rel="noopener noreferrer">${safeUrl}</a>`,
    );
  });
  return html;
}

/** 段落级渲染：只在 fenced code 之间成段输出（code 块由 React 层渲染） */
function renderText(lines: string[]): string {
  const out: string[] = [];
  let list: 'ul' | 'ol' | null = null;

  const closeList = (): void => {
    if (list) { out.push(`</${list}>`); list = null; }
  };

  for (const raw of lines) {
    const line = raw.replace(/\r$/, '');
    if (!line.trim()) { closeList(); continue; }

    const h = line.match(/^(#{1,6})\s+(.*)$/);
    if (h) { closeList(); out.push(`<h${h[1].length}>${inlineHtml(h[2])}</h${h[1].length}>`); continue; }
    const hr = line.match(/^\s*(-{3,}|\*{3,})\s*$/);
    if (hr) { closeList(); out.push('<hr />'); continue; }
    const q = line.match(/^\s*>\s?(.*)$/);
    if (q) { closeList(); out.push(`<blockquote>${inlineHtml(q[1])}</blockquote>`); continue; }
    const ul = line.match(/^\s*[-*+]\s+(.*)$/);
    const ol = line.match(/^\s*\d+[.)]\s+(.*)$/);
    if (ul || ol) {
      const kind = ul ? 'ul' : 'ol';
      const text = (ul || ol)?.[1] ?? '';
      if (list !== kind) { closeList(); out.push(`<${kind}>`); list = kind; }
      out.push(`<li>${inlineHtml(text)}</li>`);
      continue;
    }
    closeList();
    out.push(`<p>${inlineHtml(line)}</p>`);
  }
  closeList();
  return out.join('');
}

/** 内容切成文本/代码块（流式截断时未闭合 fence 也保留为 code 块） */
function parseBlocks(content: string): RenderBlock[] {
  const lines = content.split('\n');
  let inFence = false;
  let lang = '';
  let acc: string[] = [];
  let textAcc: string[] = [];
  const blocks: RenderBlock[] = [];
  const flushText = (): void => {
    if (textAcc.length) {
      blocks.push({ type: 'text', html: renderText(textAcc) });
      textAcc = [];
    }
  };
  for (const raw of lines) {
    const line = raw.replace(/\r$/, '');
    const fence = line.match(/^\s*```(\S*)\s*$/);
    if (!fence) {
      if (inFence) acc.push(line);
      else textAcc.push(line);
      continue;
    }
    if (!inFence) {
      inFence = true;
      lang = fence[1];
      acc = [];
      flushText();
    } else {
      blocks.push({ type: 'code', lang, code: acc.join('\n') });
      inFence = false;
    }
  }
  if (inFence) blocks.push({ type: 'code', lang, code: acc.join('\n') });
  flushText();
  return blocks;
}

export default function MessageViewer({ content, reasoning }: MessageViewerProps): JSX.Element {
  const { t } = useTranslation();
  const [reasoningOpen, setReasoningOpen] = useState(false);
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);

  const blocks = useMemo(() => parseBlocks(content), [content]);

  const copyCode = (block: CodeBlock, idx: number): void => {
    const text = block.code;
    const write = (): Promise<void> =>
      navigator.clipboard ? navigator.clipboard.writeText(text) : Promise.reject(new Error('no clipboard'));
    write().then(() => {
      setCopiedIdx(idx);
      setTimeout(() => setCopiedIdx(null), 1500);
    }).catch(() => {
      try {
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.focus();
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
        setCopiedIdx(idx);
        setTimeout(() => setCopiedIdx(null), 1500);
      } catch {
        // 复制失败静默降级
      }
    });
  };

  return (
    <div className="message-viewer">
      {reasoning ? (
        <div className={`reasoning-block ${reasoningOpen ? 'open' : ''}`}>
          <button
            type="button"
            className="reasoning-toggle"
            aria-expanded={reasoningOpen}
            onClick={() => setReasoningOpen((v) => !v)}
          >
            {reasoningOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
            <BrainCircuit size={13} />
            <span>{t('深度思考')}</span>
          </button>
      {reasoningOpen && <div className="reasoning-content">{reasoning}</div>}
        </div>
      ) : null}
      {blocks.map((b, i) =>
        b.type === 'text' ? (
          <div key={i} className="message-viewer-body" dangerouslySetInnerHTML={{ __html: b.html ?? '' }} />
        ) : (
          <div key={i} className="code-block-shell">
            <div className="code-block-head">
              {b.lang ? <span className="code-lang">{b.lang}</span> : <span />}
              <button
                type="button"
                className="code-copy"
                title={t('复制代码')}
                onClick={() => copyCode({ lang: b.lang ?? '', code: b.code ?? '' }, i)}
              >
                {copiedIdx === i ? <Check size={12} /> : <Copy size={12} />}
              </button>
            </div>
            <pre className="code-block-pre"><code>{b.code}</code></pre>
          </div>
        ),
      )}
    </div>
  );
}
