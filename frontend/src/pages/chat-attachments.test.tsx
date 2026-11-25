/**
 * /chat 的附件接线（判据在 `components/chat/asst-attach.ts`）。
 *
 * 这里只钉**页面接线**这一层 —— 上限/类型/二进制那些判据本身在
 * `asst-attach.test.ts` 里逐条测过。三条断言都能区分接线前后：
 * 1. 文本附件上限是 **3**（接线前页面自己写的是 `.slice(0, 6)`，多收一倍）；
 * 2. 粘贴/拖入 `.json` 会被分流到文本附件（接线前 `addImages` 按 `image/`
 *    过滤，`.json` 被静默丢掉，用户以为「贴进去了」）；
 * 3. 不支持的类型不落进附件行（接线前任何文件都当文本收下）。
 *
 * 刻意不测图片：`prepareImage` 要走 canvas 解码，jsdom 不解码图片，
 * `Image.onload` 永不触发 —— 那会测出一个永远「读取中」的假绿。
 */
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import type { ReactElement } from 'react';

/**
 * jsdom 的 `File` 没有 `.text()`（实测 `typeof file.text === 'undefined'`），
 * 而浏览器的 `Blob.text()` 是标准 API、仓里多处已经在用（`DataManagement`、
 * `AccountSessions`、`PurgeConsole`）。所以**补在测试侧**，不改生产实现：
 * 把读文件降级成 FileReader 等于为了测试让真实浏览器也走旧路径。
 */
if (typeof File !== 'undefined' && typeof File.prototype.text !== 'function') {
  File.prototype.text = function readText(this: File): Promise<string> {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result ?? ''));
      reader.onerror = () => reject(reader.error ?? new Error('read failed'));
      reader.readAsText(this);
    });
  };
}

const toastAdd = (): void => undefined;
vi.mock('../components/Toast', () => ({
  useToast: () => toastAdd,
  ToastProvider: ({ children }: { children: ReactElement }) => children,
}));

const apiMock = vi.hoisted(() => ({
  listModels: vi.fn(),
  listChatSessions: vi.fn(),
  getChatSession: vi.fn(),
  createChatSession: vi.fn(),
  deleteChatSession: vi.fn(),
  renameChatSession: vi.fn(),
  forkChatSession: vi.fn(),
  replaceChatMessages: vi.fn(),
  testChannelChat: vi.fn(),
  getAgentConfig: vi.fn(),
  getChatSettings: vi.fn(),
  listChannels: vi.fn(),
  saveChatSettings: vi.fn(),
}));

vi.mock('../api', () => ({ api: apiMock }));

import Chat from './Chat';

const jsonFile = (name: string): File => new File(['{"a":1}'], name, { type: '' });
const zipFile = (name: string): File => new File(['PK'], name, { type: 'application/zip' });

function paste(files: File[]): void {
  const textarea = screen.getByRole('textbox');
  fireEvent.paste(textarea, {
    clipboardData: {
      items: files.map((file) => ({ kind: 'file', getAsFile: () => file })),
    },
  });
}

beforeEach(() => {
  localStorage.clear();
  apiMock.listModels.mockResolvedValue({ data: [{ id: 'gpt-4o-mini' }] });
  apiMock.listChatSessions.mockResolvedValue({ data: [] });
  apiMock.createChatSession.mockResolvedValue({ data: { id: 'sess-1', title: '', model: '' } });
  apiMock.getChatSession.mockResolvedValue({ data: { session: { id: 'sess-1', title: '', model: '' }, messages: [] } });
  apiMock.replaceChatMessages.mockResolvedValue({ data: { count: 0 } });
  apiMock.getAgentConfig.mockResolvedValue({ data: { tools: [] } });
  apiMock.getChatSettings.mockResolvedValue({ data: {} });
  apiMock.listChannels.mockResolvedValue({ data: [] });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('/chat 附件接线', () => {
  it('文本附件上限 3 个（接线前是 6）', async () => {
    render(<Chat />);
    await screen.findByText('开始一个新对话');

    paste([jsonFile('a.json'), jsonFile('b.json'), jsonFile('c.json'), jsonFile('d.json')]);
    await waitFor(() => expect(screen.getByText('a.json')).toBeTruthy());
    expect(screen.getByText('b.json')).toBeTruthy();
    expect(screen.getByText('c.json')).toBeTruthy();
    // 第 4 个超上限：不进附件行（给的是 toast 提示，不是静默收下）
    expect(screen.queryByText('d.json')).toBeNull();
  });

  it('粘贴 .json 会进文本附件（接线前按 image/ 过滤后静默丢弃）', async () => {
    render(<Chat />);
    await screen.findByText('开始一个新对话');

    paste([jsonFile('account.json')]);
    await waitFor(() => expect(screen.getByText('account.json')).toBeTruthy());
  });

  it('不支持的类型不落进附件行', async () => {
    render(<Chat />);
    await screen.findByText('开始一个新对话');

    paste([zipFile('bundle.zip')]);
    // 给它一拍时间去落地，然后断言什么都没加
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(screen.queryByText('bundle.zip')).toBeNull();
  });

  it('移除附件后附件行消失', async () => {
    render(<Chat />);
    await screen.findByText('开始一个新对话');

    paste([jsonFile('x.json')]);
    await waitFor(() => expect(screen.getByText('x.json')).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: '移除' }));
    await waitFor(() => expect(screen.queryByText('x.json')).toBeNull());
  });
});
