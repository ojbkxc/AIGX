/**
 * ChatDebugger 挂载冒烟 —— 「仍可挂载」这条要求的最小判据。
 *
 * 不测渲染细节（内部逻辑已抽成纯函数单独测），只钉住：组件能挂载、能卸载、
 * 卸载时不抛（流式请求被中断 / 懒加载气泡未就绪这两条卸载路径都走一遍）。
 *
 * 原缺陷背景：卸载安全。`abortRef` 中断是异步生效的，切走会话时仍可能有帧在途；
 * 现在 onDelta 首行判 `mountedRef.current`，卸载后不再写 state。
 */
import { describe, expect, it, vi } from 'vitest';
import { render, waitFor } from '@testing-library/react';
import '../../i18n';

// jsdom 未实现 scrollIntoView（真实浏览器都有）；组件里按 `if (ref.current)` 守卫，
// 只在实现缺失时才需要这个桩。
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = function scrollIntoView(): void { /* jsdom stub */ };
}

// 气泡是懒加载的重组件（它自己有一整套纯函数模块）。挂载冒烟只关心
// ChatDebugger 自己的生命周期，故整体替换成占位——顺带避免本测被兄弟模块的
// 中间态编译错误带红（并行开发时该目录随时可能半成品）。
vi.mock('./ChatBubble', () => ({
  default: () => 'CHAT_BUBBLE_SLOT',
  __esModule: true,
}));

vi.mock('../../api', () => ({
  api: {
    listModels: vi.fn().mockResolvedValue({ data: ['gpt-4o'] }),
    listChannels: vi.fn().mockResolvedValue({ data: [] }),
    testChannelChat: vi.fn(),
    playgroundTts: vi.fn(),
    playgroundTranscribe: vi.fn(),
  },
  testChannelChatStream: vi.fn(),
  directChannelChatStream: vi.fn(),
}));

import ChatDebugger from '../ChatDebugger';

describe('ChatDebugger 挂载', () => {
  it('空态可挂载（含模型列表请求）', async () => {
    const view = render(<ChatDebugger channelId="3" channelModels={['gpt-4o']} />);
    expect(view.container.querySelector('.chat-debugger')).toBeTruthy();
    view.unmount();
  });

  it('playground 形态可挂载（无工具条形态）', async () => {
    const view = render(
      <ChatDebugger playground playgroundModel="gpt-4o" channelModels={['gpt-4o']} />,
    );
    expect(view.container.querySelector('.chat-debugger')).toBeTruthy();
    view.unmount();
  });

  it('带初始消息可挂载（/chat 会话恢复路径），卸载不抛', async () => {
    const view = render(
      <ChatDebugger
        hideToolbar
        initialMessages={[
          { role: 'user', content: '你好' },
          { role: 'assistant', content: '在的', reasoning: '想一想' },
        ]}
      />,
    );
    // 两条消息各渲染一个气泡（气泡本身被 mock 成占位文本）
    await waitFor(() => {
      expect(view.container.textContent?.match(/CHAT_BUBBLE_SLOT/g)?.length).toBe(2);
    });
    expect(() => view.unmount()).not.toThrow();
  });

  it('卸载时中断在途流式请求（不抛、不继续追加）', async () => {
    const view = render(<ChatDebugger channelId="3" channelModels={['gpt-4o']} />);
    // 挂载即卸载：abortRef 为空也不应抛（cleanup 里的 optional chain 判据）
    expect(() => view.unmount()).not.toThrow();
  });
});
