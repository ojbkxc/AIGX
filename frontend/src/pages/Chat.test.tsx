/**
 * /chat 对话形态回归（对齐 A2A v3.5.1 assistant 页）。
 *
 * 钉住四件事（都对着「用户看得见的结构」，不看内部实现）：
 * 1. **左栏**：新建对话 + 历史列表 + 空态文案（A2A assistant-shell 的分区）；
 * 2. **右栏**：顶栏会话标题、消息列空态「开始一个新对话」；
 * 3. **输入区**：文本域 + 发送键 + 模型·等级胶囊 + 权限胶囊；
 * 4. **AIGX 独有项不许因对齐而消失**：顶栏「调试」能切到调试视图并返回；
 *    设置抽屉里有「AIGX 独有项」一节。
 *
 * 旧的 /chat 把这四样全部外包给 ChatDebugger 的 playground 形态：那时页面上
 * 没有左栏、没有 840px 消息列、也没有「调试」入口（调试器自己就是界面）。
 * 所以下面每一条断言都能区分改造前后 —— 全绿不代表改造前也绿。
 */
import { describe, expect, it, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import type { ReactElement } from 'react';

/** ChatDebugger 是 63KB 的调试器，本测只关心它是否被挂到「调试视图」里，
 *  故整体替换成一个带标记的占位（顺带避免它自己的 api 调用与懒加载）。 */
vi.mock('../components/ChatDebugger', () => ({
  default: () => 'CHAT_DEBUGGER_SLOT',
  __esModule: true,
}));

/** Toast 替身：**必须是稳定引用**。若每次渲染都返回新函数，页面里挂在它上面的
 *  useCallback / useEffect 会每帧重建（真实 Provider 的 addToast 是 useCallback，
 *  稳定）。页面侧也已走 ref 兜底，这里照样钉住稳定，两道都硬。 */
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

beforeEach(() => {
  localStorage.clear();
  apiMock.listModels.mockResolvedValue({ data: [{ id: 'gpt-4o-mini' }, { id: 'claude-3-5-sonnet' }] });
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

describe('/chat 对话形态', () => {
  it('左栏 + 顶栏 + 空态齐备（A2A 分区）', async () => {
    render(<Chat />);

    // 左栏：A2A assistant-shell 的「新对话」按钮
    expect(await screen.findByRole('button', { name: /新对话/ })).toBeTruthy();
    // 首启拉列表；列表为空时页面会立刻建一条空白会话（A2A 的 activate 同语义：
    // 进来就能打字，不必先点「新对话」），所以左栏出现的是「未命名对话」项，
    // 而不是列表空态 —— 空态只在「用户删光全部会话」时出现。
    await waitFor(() => expect(apiMock.listChatSessions).toHaveBeenCalled());
    await waitFor(() => expect(apiMock.createChatSession).toHaveBeenCalled());
    expect(await screen.findByText('未命名对话')).toBeTruthy();

    // 右栏空态（A2A 的 asst-empty 文案）
    expect(await screen.findByText('开始一个新对话')).toBeTruthy();

    // 顶栏：标题兜底「新对话」+ 调试入口 + 设置入口
    expect(screen.getByRole('button', { name: '助手设置' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '调试' })).toBeTruthy();
  });

  it('会话列表非空时左栏逐项显示标题与时间', async () => {
    const now = Math.floor(Date.now() / 1000);
    apiMock.listChatSessions.mockResolvedValue({
      data: [
        { id: 'old-1', title: '排查 502', model: 'gpt-4o-mini', updated_at: now, title_source: 'user' },
        { id: 'old-2', title: '总结日志', model: 'gpt-4o-mini', updated_at: now - 86400, title_source: 'auto' },
      ],
    });
    apiMock.getChatSession.mockResolvedValue({
      data: { session: { id: 'old-1', title: '排查 502', model: 'gpt-4o-mini' }, messages: [] },
    });

    const view = render(<Chat />);
    await waitFor(() => expect(apiMock.listChatSessions).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 80));

    // 标题会同时出现在左栏列表项与顶栏（当前会话标题），所以用 getAllByText。
    expect(view.container.textContent).toContain('排查 502');
    expect(view.container.textContent).toContain('总结日志');
    // AIGX 独有：命名来源徽标（手工 / 自动）
    expect(screen.getByText('手工')).toBeTruthy();
    expect(screen.getByText('自动')).toBeTruthy();
    // 时间/模型副行：今天显示 HH:MM，昨天显示 M/D —— 只断言「有 · 模型名」的
    // 副行存在，不钉时间格式（toLocaleTimeString 随运行环境变）。
    expect(screen.getAllByText(/· gpt-4o-mini/).length).toBeGreaterThan(0);
  });

  it('输入区有文本域 / 发送键 / 模型胶囊 / 权限胶囊', async () => {
    render(<Chat />);
    await screen.findByText('开始一个新对话');

    const textarea = screen.getByRole('textbox') as HTMLTextAreaElement;
    expect(textarea).toBeTruthy();
    expect(screen.getByRole('button', { name: '发送' })).toBeTruthy();
    // 模型胶囊：模型列表拉回后默认选中第一个，胶囊里应有模型名
    await waitFor(() => expect(screen.getAllByText('gpt-4o-mini').length).toBeGreaterThan(0));
    // 权限胶囊（A2A 的第二个胶囊；AIGX 侧标注为「暂无执行通道」）
    expect(screen.getByRole('button', { name: /变更前确认|完全访问/ })).toBeTruthy();
    // AIGX 独有：思考强度快捷 pill
    expect(screen.getByRole('button', { name: /思考/ })).toBeTruthy();
  });

  it('顶栏「调试」切到调试视图，再返回对话（AIGX 独有入口）', async () => {
    render(<Chat />);
    await screen.findByText('开始一个新对话');

    fireEvent.click(screen.getByRole('button', { name: '调试' }));
    // 调试视图里挂的是 ChatDebugger（此处被替换成占位文本）
    expect(await screen.findByText('CHAT_DEBUGGER_SLOT')).toBeTruthy();
    // 对话态被换下：空态不再渲染
    expect(screen.queryByText('开始一个新对话')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: '返回对话' }));
    expect(await screen.findByText('开始一个新对话')).toBeTruthy();
  });

  it('设置抽屉落在右侧且含「AIGX 独有项」一节', async () => {
    render(<Chat />);
    await screen.findByText('开始一个新对话');

    fireEvent.click(screen.getByRole('button', { name: '助手设置' }));
    const drawer = await screen.findByRole('dialog', { name: '助手设置' });
    expect(drawer.className).toContain('asst-drawer');
    // A2A 的三节
    expect(screen.getByText('默认行为')).toBeTruthy();
    expect(screen.getByText('系统提示词')).toBeTruthy();
    expect(screen.getByText('工具')).toBeTruthy();
    // AIGX 独有项（A2A 没有，必须保留入口）
    expect(screen.getByText('AIGX 独有项')).toBeTruthy();
    expect(screen.getByText(/打开调试视图（SSE 报文/)).toBeTruthy();
  });
});
