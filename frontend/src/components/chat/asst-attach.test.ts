import { describe, expect, it } from 'vitest';
import {
  assistantFiles,
  formatFileSize,
  imageIncompatibility,
  isTextCredentialFile,
  jsonTopLevelKeys,
  MAX_IMAGE_BYTES,
  MAX_IMAGES,
  MAX_TEXT_FILES,
  MAX_TEXT_FILE_BYTES,
  planAttachmentAdd,
  textFileOutcome,
  userContentWithFiles,
} from './asst-attach';

const file = (name: string, type: string, size = 10): { name: string; type: string; size: number } => ({ name, type, size });

describe('asst-attach — 类型识别', () => {
  it('Windows 上 .json 的 MIME 常为空，必须看扩展名', () => {
    expect(isTextCredentialFile(file('a.json', ''))).toBe(true);
    expect(isTextCredentialFile(file('a.info', ''))).toBe(true);
    expect(isTextCredentialFile(file('a.JSON', 'text/plain'))).toBe(true);
    expect(isTextCredentialFile(file('a.png', 'image/png'))).toBe(false);
  // 与 AsstComposer 的 accept 对齐：界面允许选 .txt/.md，判据就必须收
  expect(isTextCredentialFile(file('notes.txt', 'text/plain'))).toBe(true);
  expect(isTextCredentialFile(file('readme.md', 'text/markdown'))).toBe(true);
  expect(isTextCredentialFile(file('x.md', ''))).toBe(true);
  });

  it('宽松读取会话里存的文件（手改过的历史不炸）', () => {
    expect(assistantFiles([{ name: 'a.json', text: '{}' }, { name: 1 }, null])).toEqual([
      { name: 'a.json', size: 2, text: '{}' },
    ]);
    expect(assistantFiles('nope')).toEqual([]);
  });
});

describe('asst-attach — 上限分拣', () => {
  it('图片 6 张、单个 10 MB；超限给出逐条原因', () => {
    const many = Array.from({ length: MAX_IMAGES + 1 }, (_, i) => file(`i${i}.png`, 'image/png'));
    const plan = planAttachmentAdd({ imageCount: 0, fileCount: 0 }, many);
    expect(plan.images).toHaveLength(MAX_IMAGES);
    expect(plan.skipped).toEqual([{ name: `i${MAX_IMAGES}.png`, reason: 'too-many-images' }]);

    const big = planAttachmentAdd({ imageCount: 0, fileCount: 0 }, [
      file('big.png', 'image/png', MAX_IMAGE_BYTES + 1),
    ]);
    expect(big.images).toHaveLength(0);
    expect(big.skipped).toEqual([{ name: 'big.png', reason: 'image-too-large' }]);
  });

  it('读取中的张数也占额度（异步读取期间连拖不能突破上限）', () => {
    const plan = planAttachmentAdd(
      { imageCount: 5, fileCount: 0, pendingImages: 1 },
      [file('x.png', 'image/png')],
    );
    expect(plan.images).toHaveLength(0);
    expect(plan.skipped[0].reason).toBe('too-many-images');
  });

  it('文本附件 3 个、单个 512 KB；不支持的类型单列', () => {
    const plan = planAttachmentAdd({ imageCount: 0, fileCount: 2 }, [
      file('a.json', ''),
      file('b.json', ''),
      file('c.zip', 'application/zip'),
      file('huge.json', '', MAX_TEXT_FILE_BYTES + 1),
    ]);
    expect(plan.textFiles.map((f) => f.name)).toEqual(['a.json']);
    expect(plan.skipped).toEqual([
      { name: 'b.json', reason: 'too-many-files' },
      { name: 'c.zip', reason: 'unsupported-type' },
      { name: 'huge.json', reason: 'file-too-large' },
    ]);
    expect(MAX_TEXT_FILES).toBe(3);
  });

  it('图片与文本分开计数（互不吃额度）', () => {
    const plan = planAttachmentAdd({ imageCount: MAX_IMAGES, fileCount: 0 }, [
      file('a.json', ''),
      file('b.png', 'image/png'),
    ]);
    expect(plan.textFiles).toHaveLength(1);
    expect(plan.images).toHaveLength(0);
  });
});

describe('asst-attach — 文本读取结果', () => {
  it('BOM 去掉；空文件与二进制分别判出', () => {
    expect(textFileOutcome('a.json', 10, '\uFEFF{"a":1}')).toEqual({
      kind: 'ok',
      file: { name: 'a.json', size: 10, text: '{"a":1}' },
    });
    expect(textFileOutcome('a.json', 0, '   ')).toEqual({ kind: 'empty' });
    expect(textFileOutcome('a.bin', 3, 'a\u0000b')).toEqual({ kind: 'binary' });
  });

  it('体积与顶层字段摘要', () => {
    expect(formatFileSize(512)).toBe('512 B');
    expect(formatFileSize(2048)).toBe('2 KB');
    expect(formatFileSize(1.5 * 1024 * 1024)).toBe('1.5 MB');
    expect(jsonTopLevelKeys('[1,2,3]')).toEqual(['数组（3 项）']);
    expect(jsonTopLevelKeys('{"a":1,"b":2}')).toEqual(['a', 'b']);
    expect(jsonTopLevelKeys('not json')).toEqual([]);
  });
});

describe('asst-attach — 送给模型的正文', () => {
  it('无附件时正文逐字不变', () => {
    expect(userContentWithFiles('hi', [])).toBe('hi');
  });

  it('附件以「元信息 + 顶层字段 + 预览」追加，并给 attachment:N 引用', () => {
    const text = userContentWithFiles('看下', [{ name: 'acc.json', size: 100, text: '{"api_key":"k"}' }]);
    expect(text.startsWith('看下\n\n【用户上传的文件】')).toBe(true);
    expect(text).toContain('【附件 1：acc.json（100 B');
    expect(text).toContain('顶层字段：api_key');
    expect(text).toContain('attachment:1');
  });

  it('超长内容只带前 1200 字符并说明剩余量', () => {
    const long = 'x'.repeat(1300);
    const text = userContentWithFiles('', [{ name: 'a.json', size: 1300, text: long }]);
    expect(text).toContain('预览已截断');
    expect(text).toContain('剩余 100 字符未显示');
    expect(text.length).toBeLessThan(1300 + 400);
  });
});

describe('asst-attach — 与模型能力的兼容', () => {
  it('有图且模型明确不支持才提示；「不知道」不提示', () => {
    expect(imageIncompatibility(['data:'], false)).toBe(true);
    expect(imageIncompatibility(['data:'], true)).toBe(false);
    expect(imageIncompatibility(['data:'], undefined)).toBe(false);
    expect(imageIncompatibility([], false)).toBe(false);
  });
});
