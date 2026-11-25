import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import ToolCallCard from './ToolCallCard';
import { RESULT_PREVIEW_CHARS } from './tool-calls';

// i18n：组件用 react-i18next 的 t()，测试里把 key 原样返回即可（断言的是 key）
vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

const call = (patch: Partial<{ index: number; id: string; name: string; arguments: string; result: string }> = {}) => ({
  index: 0,
  name: 'list_accounts',
  arguments: '{"limit":5}',
  ...patch,
});

describe('ToolCallCard — 三态与展开形态', () => {
  it('流式中：参数默认展开、结果区未出现、有「进行中」', () => {
    render(<ToolCallCard calls={[call()]} state="streaming" />);
    expect(screen.getByTestId('tool-call-card')).toBeTruthy();
    expect(screen.getByTestId('tool-call-args').textContent).toBe('{"limit":5}');
    expect(screen.queryByTestId('tool-call-result')).toBeNull();
    expect(screen.getByTestId('tool-call-phase').textContent).toContain('进行中');
  });

  it('完成态：结果可见，单条徽标是「完成」', () => {
    render(<ToolCallCard calls={[call({ result: '共 2 个账号' })]} state="done" />);
    expect(screen.getByTestId('tool-call-result').textContent).toBe('共 2 个账号');
    expect(screen.getByTestId('tool-call-phase').textContent).toContain('完成');
  });

  it('失败态：结果里的失败位把这条标成失败，并单独着色', () => {
    render(<ToolCallCard calls={[call({ result: '{"ok":false,"error":"quota"}' })]} state="done" />);
    expect(screen.getByTestId('tool-call-phase').textContent).toContain('失败');
  });

  it('中断态：无结果的调用标「停止」，不伪造结果', () => {
    render(<ToolCallCard calls={[call()]} state="interrupted" />);
    expect(screen.getByTestId('tool-call-phase').textContent).toContain('停止');
    expect(screen.queryByTestId('tool-call-result')).toBeNull();
  });

  it('同一条回复里两成一败：按调用分别着色，不被消息级状态覆盖', () => {
    render(
      <ToolCallCard
        calls={[call({ index: 0, name: 'a', result: 'ok' }), call({ index: 1, name: 'b', result: '{"ok":false}' })]}
        state="done"
      />,
    );
    const phases = screen.getAllByTestId('tool-call-phase').map((node) => node.textContent);
    expect(phases[0]).toContain('完成');
    expect(phases[1]).toContain('失败');
  });
});

describe('ToolCallCard — 参数与结果的边界', () => {
  it('半截 JSON 判「非合法 JSON」但原文照显', () => {
    render(<ToolCallCard calls={[call({ arguments: '{"path":' })]} state="streaming" />);
    expect(screen.getByTestId('tool-call-invalid')).toBeTruthy();
    expect(screen.getByTestId('tool-call-args').textContent).toBe('{"path":');
  });

  it('凭证键被遮蔽且原文不进 DOM', () => {
    render(<ToolCallCard calls={[call({ arguments: '{"api_key":"sk-secret","model":"gpt"}' })]} state="done" />);
    // 完成态参数默认收起：先点开「输入」
    fireEvent.click(screen.getByRole('button', { name: '输入' }));
    const args = screen.getByTestId('tool-call-args').textContent ?? '';
    expect(args).toContain('***');
    expect(args).not.toContain('sk-secret');
    expect(screen.getByTestId('tool-call-redacted')).toBeTruthy();
  });

  it('超长结果默认截断，点「展开全部」后全文可见', async () => {
    const long = 'x'.repeat(RESULT_PREVIEW_CHARS + 500);
    render(<ToolCallCard calls={[call({ result: long })]} state="done" />);
    expect(screen.getByTestId('tool-call-result').textContent).toHaveLength(RESULT_PREVIEW_CHARS);
    fireEvent.click(screen.getByTestId('tool-call-expand'));
    await waitFor(() => {
      expect(screen.getByTestId('tool-call-result').textContent).toHaveLength(long.length);
    });
  });

  it('工具中文名与风险徽标来自后端下发的表（不编造）', () => {
    render(
      <ToolCallCard
        calls={[call({ name: 'create_key' })]}
        state="done"
        labels={{ create_key: '创建密钥' }}
        risks={{ create_key: 'high' }}
      />,
    );
    const item = screen.getAllByTestId('tool-call-item')[0];
    expect(within(item).getByText('创建密钥')).toBeTruthy();
    expect(within(item).getByText('高危')).toBeTruthy();
  });

  it('未登记的工具回退工具 id（不显示编造的中文名）', () => {
    render(<ToolCallCard calls={[call({ name: 'unknown_tool' })]} state="done" />);
    expect(screen.getByText('unknown_tool')).toBeTruthy();
  });

  it('用量：缺 total 时用 prompt + completion 合成；估算角标只在 estimated 时出现', () => {
    const { rerender } = render(
      <ToolCallCard calls={[call()]} state="done" usage={{ prompt_tokens: 3, completion_tokens: 4 }} />,
    );
    const usage = screen.getByTestId('usage-summary');
    expect(usage.textContent).toContain('7');
    expect(screen.queryByTestId('usage-estimated')).toBeNull();

    rerender(<ToolCallCard calls={[call()]} state="done" usage={{ total_tokens: 9, estimated: true }} />);
    expect(screen.getByTestId('usage-estimated')).toBeTruthy();
  });

  it('无结果且已完成时如实标「结果未回灌」', () => {
    render(<ToolCallCard calls={[call()]} state="done" />);
    expect(screen.getByText('结果未回灌')).toBeTruthy();
  });
});
