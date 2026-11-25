/**
 * AsstMessages —— 消息操作判据接线（对齐 A2A `assistant-messages.tsx` 的
 * `AssistantMessageActions` + `assistant-message-operations.ts`）。
 *
 * 钉住四件事（每条都能区分接线前后）：
 * 1. 忙碌时**不渲染**编辑/删除/分叉/重生成（A2A 是 `if (busy()) return`，
 *    不是「灰着但还能点」）；
 * 2. 空消息不给复制与重生成（占位气泡不该能点出一次新请求）；
 * 3. 重生成只给「最后一条有正文的助手消息」，分叉只给助手消息；
 * 4. 工具调用按**每条调用**着色（一成一败不该同色）。
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import AsstMessages from './AsstMessages';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

const noop = (): void => undefined;

function renderMessages(
  messages: Parameters<typeof AsstMessages>[0]['messages'],
  patch: Partial<Parameters<typeof AsstMessages>[0]> = {},
): ReturnType<typeof render> {
  return render(
    <AsstMessages
      messages={messages}
      model="gpt-4o-mini"
      busy={false}
      copiedIndex={null}
      onCopy={noop}
      onBeginEdit={noop}
      onDelete={noop}
      onRegenerate={noop}
      onFork={noop}
      {...patch}
    />,
  );
}

describe('AsstMessages — 忙碌时不给操作', () => {
  it('busy：用户消息只剩可复制（编辑/删除都不渲染）', () => {
    renderMessages([{ role: 'user', content: '你好' }], { busy: true });
    expect(screen.getByRole('button', { name: '复制' })).toBeTruthy();
    expect(screen.queryByRole('button', { name: '编辑并重发' })).toBeNull();
    expect(screen.queryByRole('button', { name: '删除这条' })).toBeNull();
  });

  it('busy：助手消息不给重生成/分叉/删除', () => {
    renderMessages([{ role: 'assistant', content: '回复', toolState: 'done' }], { busy: true });
    expect(screen.queryByRole('button', { name: '重新生成' })).toBeNull();
    expect(screen.queryByRole('button', { name: '从此条分叉' })).toBeNull();
    expect(screen.queryByRole('button', { name: '删除这条' })).toBeNull();
  });
});

describe('AsstMessages — 空消息与最后一条', () => {
  it('空助手消息：不给复制与重生成（避免复制空串 / 对空回复发新请求）', () => {
    // 只放这一条：用户消息自己也有「复制」，全局查询会命中它（断言会失去意义）
    renderMessages([{ role: 'assistant', content: '   ', toolState: 'done' }]);
    expect(screen.queryByRole('button', { name: '复制' })).toBeNull();
    expect(screen.queryByRole('button', { name: '重新生成' })).toBeNull();
    // 删除仍然可用：空回复正是用户想删掉的东西
    expect(screen.getByRole('button', { name: '删除这条' })).toBeTruthy();
  });

  it('非最后一条助手消息：有分叉，没有重生成', () => {
    renderMessages([
      { role: 'assistant', content: '第一条回复', toolState: 'done' },
      { role: 'user', content: '继续' },
    ]);
    expect(screen.queryByRole('button', { name: '重新生成' })).toBeNull();
    expect(screen.getAllByRole('button', { name: '从此条分叉' })).toHaveLength(1);
  });

  it('最后一条助手消息：重生成与分叉都在', () => {
    renderMessages([{ role: 'user', content: '问' }, { role: 'assistant', content: '答', toolState: 'done' }]);
    expect(screen.getByRole('button', { name: '重新生成' })).toBeTruthy();
    expect(screen.getByRole('button', { name: '从此条分叉' })).toBeTruthy();
  });

  it('用户消息没有分叉（分叉语义是「从这一轮起复制」，只有助手消息是完整一轮）', () => {
    renderMessages([{ role: 'user', content: '问' }]);
    expect(screen.queryByRole('button', { name: '从此条分叉' })).toBeNull();
    expect(screen.getByRole('button', { name: '编辑并重发' })).toBeTruthy();
  });

  it('流式中的最后一条：整条操作栏隐藏', () => {
    renderMessages([{ role: 'assistant', content: '正在写', toolState: 'streaming' }]);
    expect(screen.queryByRole('button', { name: '复制' })).toBeNull();
    expect(screen.queryByRole('button', { name: '删除这条' })).toBeNull();
  });
});

describe('AsstMessages — 附件与工具卡', () => {
  it('文本附件与图片同泡显示（文件名 + 体积）', () => {
    renderMessages([{
      role: 'user',
      content: '看下这个',
      attachments: [{ kind: 'image', url: 'data:image/png;base64,AA' }],
      files: [{ name: 'acc.json', size: 2048, text: '{}' }],
    }]);
    expect(screen.getByText('acc.json')).toBeTruthy();
    expect(screen.getByText('2 KB')).toBeTruthy();
  });

  it('工具卡按每条调用着色：一成一败不同色', () => {
    renderMessages([{
      role: 'assistant',
      content: '处理完了',
      toolState: 'done',
      toolCalls: [
        { index: 0, name: 'list_accounts', arguments: '{}', result: '共 2 个' },
        { index: 1, name: 'create_key', arguments: '{"name":"x"}', result: '{"ok":false,"error":"quota"}' },
      ],
    }]);
    const phases = screen.getAllByTestId('tool-call-phase').map((node) => node.textContent);
    expect(phases).toHaveLength(2);
    expect(phases[0]).toContain('完成');
    expect(phases[1]).toContain('失败');
  });

  it('无工具调用但有 usage：仍渲染用量收尾', () => {
    renderMessages([{ role: 'assistant', content: '答', toolState: 'done', usage: { total_tokens: 12 } }]);
    expect(screen.getByTestId('usage-summary').textContent).toContain('12');
  });
});
