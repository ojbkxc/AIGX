/**
 * AsstComposer —— 判据层接线的可见行为（对齐 A2A `assistant-composer.tsx`）。
 *
 * 钉住五件事，每一条都能区分「接线前 / 接线后」：
 * 1. 有图但模型**明确**不支持读图 → 提示出现，但发送键**仍可用**（只提示不拦）；
 * 2. 模型未声明能力（`undefined`）→ 不提示（「不知道」≠「不支持」）；
 * 3. 当前模型不在可用列表 → 提示出现，胶囊**仍显示已选模型**（不清空）；
 * 4. 推理档位候选按传入的收窄表渲染，超范围档位如实列出而不是静默消失；
 * 5. 粘贴按类型分流：`.json` 走文本附件、`png` 走图片（旧实现一律当图片）。
 */
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import AsstComposer, { type AsstComposerProps } from './AsstComposer';

vi.mock('react-i18next', () => ({
  useTranslation: () => ({ t: (key: string, options?: Record<string, unknown>) => (
    options ? `${key}|${Object.entries(options).map(([k, v]) => `${k}=${String(v)}`).join(',')}` : key
  ) }),
}));

function baseProps(patch: Partial<AsstComposerProps> = {}): AsstComposerProps {
  return {
    draft: 'hi',
    onDraftChange: () => undefined,
    images: [],
    files: [],
    onAddImages: () => undefined,
    onAddFiles: () => undefined,
    onRemoveImage: () => undefined,
    onRemoveFile: () => undefined,
    models: ['gpt-4o-mini', 'claude-3-5-sonnet'],
    model: 'gpt-4o-mini',
    modelsLoading: false,
    onRefreshModels: () => undefined,
    onPickModel: () => undefined,
    reasoning: null,
    onPickReasoning: () => undefined,
    mode: 'confirm',
    onModeChange: () => undefined,
    effort: 'off',
    onPickEffort: () => undefined,
    busy: false,
    awaiting: false,
    canSend: true,
    onSend: () => undefined,
    onStop: () => undefined,
    ...patch,
  };
}

describe('AsstComposer — 读图兼容（只提示不拦）', () => {
  it('有图且模型明确不支持 → 提示出现，发送键仍可用', () => {
    render(<AsstComposer {...baseProps({ images: ['data:image/png;base64,AA'], modelSupportsImages: false })} />);
    expect(screen.getByTestId('asst-image-warning')).toBeTruthy();
    const send = screen.getByRole('button', { name: '发送' }) as HTMLButtonElement;
    expect(send.disabled).toBe(false);
  });

  it('模型未声明能力（undefined）→ 不提示', () => {
    render(<AsstComposer {...baseProps({ images: ['data:image/png;base64,AA'] })} />);
    expect(screen.queryByTestId('asst-image-warning')).toBeNull();
  });

  it('模型声明支持 → 不提示', () => {
    render(<AsstComposer {...baseProps({ images: ['data:image/png;base64,AA'], modelSupportsImages: true })} />);
    expect(screen.queryByTestId('asst-image-warning')).toBeNull();
  });

  it('没图时不提示（哪怕模型不支持）', () => {
    render(<AsstComposer {...baseProps({ modelSupportsImages: false })} />);
    expect(screen.queryByTestId('asst-image-warning')).toBeNull();
  });
});

describe('AsstComposer — 模型可用性', () => {
  it('不在可用列表 → 提示出现，但胶囊仍显示已选模型', () => {
    render(<AsstComposer {...baseProps({ model: 'gone-model', modelAvailable: false })} />);
    expect(screen.getByTestId('asst-model-warning')).toBeTruthy();
    expect(screen.getByTitle('gone-model')).toBeTruthy();
  });

  it('列表为空（未知）→ 不提示', () => {
    render(<AsstComposer {...baseProps({ modelAvailable: true })} />);
    expect(screen.queryByTestId('asst-model-warning')).toBeNull();
  });

  it('正在拉清单时不提示（避免闪一下「不可用」）', () => {
    render(<AsstComposer {...baseProps({ modelAvailable: false, modelsLoading: true })} />);
    expect(screen.queryByTestId('asst-model-warning')).toBeNull();
  });
});

describe('AsstComposer — 推理档位候选', () => {
  it('候选按传入表渲染（收窄到本网关值域），不含被剔除的档位', () => {
    render(<AsstComposer {...baseProps({ effortLevels: ['off', 'low', 'high'] })} />);
    fireEvent.click(screen.getByTitle('gpt-4o-mini'));
    fireEvent.click(screen.getByText('推理等级'));
    // 收窄表里的三档都在
    expect(screen.getByText('off')).toBeTruthy();
    expect(screen.getByText('low')).toBeTruthy();
    expect(screen.getByText('high')).toBeTruthy();
    // A2A 八档里本网关不认的档位不该出现
    expect(screen.queryByText('minimal')).toBeNull();
    expect(screen.queryByText('xhigh')).toBeNull();
  });

  it('被剔除的档位如实报出来，不静默丢弃', () => {
    render(<AsstComposer {...baseProps({ effortLevels: ['off', 'high'], effortDropped: ['minimal', 'max'] })} />);
    fireEvent.click(screen.getByTitle('gpt-4o-mini'));
    fireEvent.click(screen.getByText('推理等级'));
    const note = screen.getByTestId('asst-effort-dropped');
    expect(note.textContent).toContain('minimal / max');
  });

  it('未传候选时退回 A2A 八档表（与接线前一致）', () => {
    render(<AsstComposer {...baseProps()} />);
    fireEvent.click(screen.getByTitle('gpt-4o-mini'));
    fireEvent.click(screen.getByText('推理等级'));
    expect(screen.getByText('minimal')).toBeTruthy();
    expect(screen.getByText('xhigh')).toBeTruthy();
  });
});

describe('AsstComposer — 粘贴分流', () => {
  const file = (name: string, type: string): File => new File(['x'], name, { type });

  it('粘贴 .json 走文本附件，png 走图片（旧实现一律当图片）', () => {
    const onAddImages = vi.fn();
    const onAddFiles = vi.fn();
    render(<AsstComposer {...baseProps({ onAddImages, onAddFiles })} />);
    const textarea = screen.getByRole('textbox');
    const jsonFile = file('acc.json', '');
    const pngFile = file('shot.png', 'image/png');
    fireEvent.paste(textarea, {
      clipboardData: {
        items: [
          { kind: 'file', getAsFile: () => jsonFile },
          { kind: 'file', getAsFile: () => pngFile },
        ],
      },
    });
    expect(onAddFiles).toHaveBeenCalledTimes(1);
    expect((onAddFiles.mock.calls[0][0] as File[])[0].name).toBe('acc.json');
    expect(onAddImages).toHaveBeenCalledTimes(1);
    expect((onAddImages.mock.calls[0][0] as File[])[0].name).toBe('shot.png');
  });
});
