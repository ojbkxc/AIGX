import { describe, expect, it, vi, afterEach } from 'vitest';
import { copyText } from './clipboard';

const originalClipboard = navigator.clipboard;

afterEach(() => {
  vi.restoreAllMocks();
  Object.defineProperty(navigator, 'clipboard', {
    value: originalClipboard,
    configurable: true,
  });
});

describe('copyText', () => {
  it('clipboard API 可用时走它，不碰 execCommand', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    const exec = vi.fn().mockReturnValue(true);
    document.execCommand = exec;

    await expect(copyText('hello')).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith('hello');
    expect(exec).not.toHaveBeenCalled();
  });

  it('非安全上下文（clipboard 不存在）→ 降级 execCommand', async () => {
    Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
    const exec = vi.fn().mockReturnValue(true);
    document.execCommand = exec;

    await expect(copyText('fallback')).resolves.toBe(true);
    expect(exec).toHaveBeenCalledWith('copy');
    // 临时 textarea 必须被清掉，不能留在 body 里
    expect(document.querySelectorAll('textarea')).toHaveLength(0);
  });

  it('clipboard 拒绝时也降级（不是直接判失败）', async () => {
    const writeText = vi.fn().mockRejectedValue(new Error('denied'));
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    const exec = vi.fn().mockReturnValue(true);
    document.execCommand = exec;

    await expect(copyText('retry')).resolves.toBe(true);
    expect(writeText).toHaveBeenCalled();
    expect(exec).toHaveBeenCalledWith('copy');
  });

  it('两条路都失败 → false（调用方据此给错误提示）', async () => {
    Object.defineProperty(navigator, 'clipboard', { value: undefined, configurable: true });
    document.execCommand = vi.fn().mockReturnValue(false);

    await expect(copyText('nope')).resolves.toBe(false);
  });
});
