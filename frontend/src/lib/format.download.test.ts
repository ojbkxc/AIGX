import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { downloadText } from './format';

describe('downloadText', () => {
  let clickSpy: ReturnType<typeof vi.fn>;
  let created: HTMLAnchorElement[];

  beforeEach(() => {
    vi.useFakeTimers();
    created = [];
    clickSpy = vi.fn();
    // jsdom 没有 createObjectURL/revokeObjectURL：先定义再 spy
    const urlAny = URL as unknown as Record<string, unknown>;
    urlAny.createObjectURL = vi.fn().mockReturnValue('blob:mock');
    urlAny.revokeObjectURL = vi.fn();
    vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:mock');
    vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
    const realCreate = document.createElement.bind(document);
    vi.spyOn(document, 'createElement').mockImplementation(((tag: string) => {
      const el = realCreate(tag);
      if (tag === 'a') {
        (el as HTMLAnchorElement).click = clickSpy as unknown as () => void;
        created.push(el as HTMLAnchorElement);
      }
      return el;
    }) as typeof document.createElement);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('写入 BOM 且按 UTF-8 建 blob（Excel 中文不乱码）', () => {
    const BlobSpy = vi.fn().mockReturnValue({ size: 0 } as unknown as Blob);
    vi.stubGlobal('Blob', BlobSpy as unknown as typeof Blob);

    downloadText('a.csv', '表头\n值');
    expect(BlobSpy).toHaveBeenCalledTimes(1);
    const parts = BlobSpy.mock.calls[0][0] as string[];
    expect(parts[0].startsWith('\uFEFF')).toBe(true);
    expect(BlobSpy.mock.calls[0][1]).toMatchObject({ type: 'text/csv;charset=utf-8' });
    vi.unstubAllGlobals();
  });

  it('click 时链接已在文档里，事后被移除（老 Firefox 需要）', () => {
    let connectedAtClick: boolean | null = null;
    let linkAtClick: HTMLAnchorElement | null = null;
    clickSpy.mockImplementation(() => {
      linkAtClick = created[created.length - 1] ?? null;
      connectedAtClick = linkAtClick ? document.body.contains(linkAtClick) : null;
    });

    downloadText('b.csv', 'x');
    expect(connectedAtClick).toBe(true);
    // 函数返回后必须清理，不能留游离节点
    expect(linkAtClick ? document.body.contains(linkAtClick) : null).toBe(false);
  });

  it('revoke 延后执行，不在同步栈里释放（避免 0 字节下载）', () => {
    downloadText('c.csv', 'x');
    expect(URL.revokeObjectURL).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1000);
    expect(URL.revokeObjectURL).toHaveBeenCalledWith('blob:mock');
  });
});
