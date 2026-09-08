import { describe, expect, it } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import MessageViewer from './MessageViewer';
import '../i18n';

describe('MessageViewer — Markdown 渲染', () => {
  it('粗体与行内 code', () => {
    render(<MessageViewer content={'**重点** `fn()`'} />);
    const body = document.querySelector('.message-viewer-body');
    expect(body?.innerHTML).toContain('<strong>重点</strong>');
    expect(body?.innerHTML).toContain('<code>fn()</code>');
  });

  it('fenced code 独立成块（语言标签 + 复制按钮）', () => {
    render(<MessageViewer content={'```ts\nconst a = 1;\n```'} />);
    expect(document.querySelector('.code-lang')?.textContent).toBe('ts');
    expect(document.querySelector('.code-block-pre code')?.textContent).toBe('const a = 1;');
  });

  it('流式截断未闭合 fence 兜底为 code 块', () => {
    render(<MessageViewer content={'```rust\nlet x = 42;'} />);
    expect(document.querySelector('.code-block-pre code')?.textContent).toBe('let x = 42;');
  });

  it('XSS 转义：上游 HTML 永不执行', () => {
    render(<MessageViewer content={'<script>alert(1)</script>'} />);
    expect(document.querySelector('.message-viewer-body')?.innerHTML).toContain('&lt;script&gt;');
    expect(document.querySelector('.message-viewer-body script')).toBeNull();
  });

  it('reasoning 默认收起、点击展开', () => {
    render(<MessageViewer content="正文" reasoning="思考过程" />);
    expect(document.querySelector('.reasoning-content')).toBeNull();
    fireEvent.click(document.querySelector('.reasoning-toggle') as HTMLButtonElement);
    expect(document.querySelector('.reasoning-content')?.textContent).toBe('思考过程');
  });
});