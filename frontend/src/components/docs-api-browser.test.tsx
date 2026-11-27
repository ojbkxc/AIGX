/**
 * docs-api-browser 测试 —— 「完整接入文档」浏览器。
 *
 * 钉住四件事（都能真变红，不是自指断言）：
 * 1. 文档正文里**只出现 AIGX 真实存在的端点**：显式断言 A2A 有、AIGX 没有的路径
 *    （`/v1/images/files/`）不出现，而 AIGX 路由表里的 `/v1/rerank` 等必须出现；
 * 2. 章节导航默认落在「接口地址」，切章后正文换掉、分页按钮给出下一章；
 * 3. 搜索能过滤章节（无命中给空态）并在正文里高亮关键词；
 * 4. 一键复制把代码原文交给剪贴板，且示例里不含真实凭据（只有 sk-xxx 占位）。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import DocsApiBrowser from './docs-api-browser';
import { DOCS_ENDPOINTS, docsApiChapters } from '../lib/docs-api-content';
import '../i18n';

/** 收集所有章节的「文本 + 示例代码」，用于内容级断言 */
function allChapterText(): string {
  return docsApiChapters('http://gw.local:9527')
    .map((chapter) =>
      [
        chapter.title,
        chapter.summary,
        chapter.path ?? '',
        ...(chapter.paragraphs ?? []),
        chapter.warning ?? '',
        ...(chapter.points ?? []),
        ...(chapter.fields ?? []).flatMap((field) => [field.name, field.type, field.description]),
        ...(chapter.examples ?? []).flatMap((example) => [example.label, example.code]),
      ].join('\n'),
    )
    .join('\n');
}

describe('docs-api-content 内容口径', () => {
  it('端点路径全部来自 AIGX 真实路由（不含 A2A 独有的 files 下载路径）', () => {
    const paths = DOCS_ENDPOINTS.map((endpoint) => endpoint.path).join('\n');
    // A2A 的临时图片下载是 /v1/images/files/{id}；AIGX 是 /v1/images/assets/:name
    expect(paths).not.toContain('/v1/images/files');
    expect(paths).toContain('/v1/images/assets/:name');
    // AIGX 路由表里真实存在、A2A 文档没有的端点必须写进文档
    for (const path of [
      '/v1/chat/completions',
      '/v1/responses',
      '/v1/messages',
      '/v1/messages/count_tokens',
      '/v1/embeddings',
      '/v1/rerank',
      '/v1/completions',
      '/v1/models',
      '/v1/audio/transcriptions',
      '/v1/audio/speech',
    ]) {
      expect(paths).toContain(path);
    }
  });

  it('正文不含 A2A 独有的模型条目字段（AIGX 清单里没有这些键）', () => {
    const text = allChapterText();
    // AIGX list_item 无 kind / supports_image_generation / max_input_tokens
    expect(text).not.toContain('supports_image_generation');
    expect(text).not.toContain('supports_image_editing');
    expect(text).not.toContain('max_input_tokens');
    // 而 AIGX 真实字段必须在
    expect(text).toContain('supports_rerank');
    expect(text).toContain('reasoning_levels');
    expect(text).toContain('cap_sources');
  });

  it('示例里只有 sk-xxx 占位凭据', () => {
    const text = allChapterText();
    expect(text).toContain('sk-xxx');
    // 真实形态的 key（sk- 后接长串）不得出现
    expect(/(sk-[A-Za-z0-9]{8,})/.test(text)).toBe(false);
  });
});

describe('DocsApiBrowser 交互', () => {
  beforeEach(() => {
    localStorage.clear();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
  });

  it('默认落在「接口地址」，正文列出当前 Base URL 拼出的端点', () => {
    render(<DocsApiBrowser origin="http://gw.local:9527" />);
    expect(document.querySelector('#docs-api-article-title')?.textContent).toBe('接口地址');
    expect(document.querySelector('.docs-api-ep-value')?.textContent).toContain('http://gw.local:9527');
    // token 计数端点必须出现在地址表里（AIGX 独有）
    expect(document.body.textContent).toContain('/v1/messages/count_tokens');
  });

  it('点目录切章后正文换成该章，分页出现下一章', () => {
    render(<DocsApiBrowser origin="http://gw.local:9527" />);
    const navButtons = () => Array.from(document.querySelectorAll('.docs-api-nav-item'));
    const target = navButtons().find((button) => button.textContent?.includes('文本补全、向量与重排'));
    expect(target).toBeTruthy();
    fireEvent.click(target as HTMLElement);
    expect(document.querySelector('#docs-api-article-title')?.textContent).toBe('文本补全、向量与重排');
    // 该章正文里应出现 rerank 的 documents 字段说明
    expect(document.body.textContent).toContain('/v1/rerank');
    // 分页按钮存在（非首章 → 有上一章）
    expect(document.querySelector('.docs-api-pager-btn')).toBeTruthy();
  });

  it('搜索过滤章节并在正文里高亮关键词', () => {
    render(<DocsApiBrowser origin="http://gw.local:9527" />);
    const input = document.querySelector('.docs-api-search input') as HTMLInputElement;
    // reasoning_levels 只在「模型发现与健康检查」一章出现（字段名），命中唯一
    fireEvent.change(input, { target: { value: 'reasoning_levels' } });
    expect(document.querySelector('.docs-api-hit')?.textContent).toContain('找到');
    const titles = Array.from(document.querySelectorAll('.docs-api-nav-item')).map((n) => n.textContent ?? '');
    expect(titles).toEqual(['模型发现与健康检查']);
    // Enter 跳到第一个命中，正文里出现 <mark> 高亮（不是只在目录里高亮）
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(document.querySelector('#docs-api-article-title')?.textContent).toBe('模型发现与健康检查');
    expect(document.querySelectorAll('.docs-api-article .docs-api-mark').length).toBeGreaterThan(0);
    // 清空后恢复全部章节、高亮消失
    fireEvent.change(input, { target: { value: '' } });
    expect(document.querySelector('.docs-api-hit')).toBeNull();
    expect(document.querySelectorAll('.docs-api-article .docs-api-mark').length).toBe(0);
    expect(document.querySelectorAll('.docs-api-nav-item').length).toBe(docsApiChapters('').length + 1);
  });

  it('搜索无命中时给空态与清除按钮', () => {
    render(<DocsApiBrowser origin="http://gw.local:9527" />);
    const input = document.querySelector('.docs-api-search input') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'zzz-no-such-topic-zzz' } });
    expect(document.querySelector('.docs-api-empty')?.textContent).toContain('没有找到匹配的文档');
    const clear = Array.from(document.querySelectorAll('.docs-api-empty button'))[0];
    expect(clear).toBeTruthy();
    fireEvent.click(clear as HTMLElement);
    expect(document.querySelector('.docs-api-empty')).toBeNull();
  });

  it('复制示例把代码原文交给剪贴板', async () => {
    render(<DocsApiBrowser origin="http://gw.local:9527" />);
    const navButtons = Array.from(document.querySelectorAll('.docs-api-nav-item'));
    const chat = navButtons.find((button) => button.textContent?.includes('OpenAI Chat Completions'));
    fireEvent.click(chat as HTMLElement);
    const copyButtons = Array.from(document.querySelectorAll('.docs-api-example-head .docs-api-copy'));
    expect(copyButtons.length).toBeGreaterThan(0);
    // clipboard.writeText 是 promise：在 act 内 await，避免复制成功的 setState 落到 act 外
    await act(async () => {
      fireEvent.click(copyButtons[0] as HTMLElement);
    });
    const writeText = navigator.clipboard.writeText as unknown as ReturnType<typeof vi.fn>;
    expect(writeText).toHaveBeenCalledTimes(1);
    const copied = String(writeText.mock.calls[0][0]);
    expect(copied).toContain('/v1/chat/completions');
    expect(copied).toContain('sk-xxx');
  });

  it('复制接口地址用的是完整 URL', async () => {
    render(<DocsApiBrowser origin="http://gw.local:9527" />);
    const iconCopy = document.querySelector('.docs-api-ep-line .docs-api-copy') as HTMLElement;
    await act(async () => {
      fireEvent.click(iconCopy);
    });
    const writeText = navigator.clipboard.writeText as unknown as ReturnType<typeof vi.fn>;
    const copied = String(writeText.mock.calls[0][0]);
    expect(copied.startsWith('http://gw.local:9527')).toBe(true);
  });

  it('章节导航用「上一章 / 下一章」标签与标题', () => {
    render(<DocsApiBrowser origin="http://gw.local:9527" />);
    expect(screen.getAllByText('下一章').length).toBeGreaterThan(0);
  });
});
