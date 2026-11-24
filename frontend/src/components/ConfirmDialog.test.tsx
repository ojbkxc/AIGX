/**
 * ConfirmDialog —— onConfirm 失败路径必须"看得见"。
 *
 * 复现的原始缺陷：`onConfirm` **同步**抛错时，异常从 `async handleConfirm`
 * 逃成 rejected promise，调用方写的是 `void handleConfirm()`，异常被丢掉：
 * 弹窗不关、没有任何文案，用户只看到按钮从「处理中…」恢复，以为操作成功。
 * 异步分支的 catch 覆盖不到这条路径。
 *
 * 注意：每个用例都显式传 `confirmText`。默认文案走 `t('确定')`，而测试环境的
 * 初始语言取自 navigator（jsdom 是 en-US），同一个按钮名会随环境在「确定 / OK」
 * 之间漂移——按文字点按钮的纪律要求先把文字钉死。
 */
import { describe, expect, it, vi } from 'vitest';
import { render, fireEvent, screen, act } from '@testing-library/react';
import ConfirmDialog, { type ConfirmState } from './ConfirmDialog';
import '../i18n';

const CONFIRM_TEXT = '确认执行';

function renderDialog(state: ConfirmState, onClose = vi.fn()) {
  const view = render(<ConfirmDialog state={{ ...state, confirmText: CONFIRM_TEXT }} onClose={onClose} />);
  return { ...view, onClose };
}

/** 点确认按钮（按文字点，不按位置取首/末按钮） */
function clickConfirm(): void {
  act(() => {
    fireEvent.click(screen.getByRole('button', { name: CONFIRM_TEXT }));
  });
}

describe('ConfirmDialog onConfirm 失败路径', () => {
  it('同步抛错：显示错误文案且弹窗不关', () => {
    const onClose = vi.fn();
    renderDialog(
      {
        message: '要删除吗',
        onConfirm: () => {
          throw new Error('前置条件不满足');
        },
      },
      onClose,
    );

    clickConfirm();

    // 弹窗还在
    expect(onClose).not.toHaveBeenCalled();
    // 错误文案可见（原来这条路径一个字都没有）
    expect(screen.getByText('前置条件不满足')).toBeTruthy();
  });

  it('异步失败：同样显示错误文案且弹窗不关（既有行为不得回退）', async () => {
    const onClose = vi.fn();
    renderDialog(
      {
        message: '要删除吗',
        onConfirm: async () => {
          throw new Error('后端拒绝了');
        },
      },
      onClose,
    );

    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: CONFIRM_TEXT }));
    });

    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByText('后端拒绝了')).toBeTruthy();
  });

  it('正向对照：同步成功仍然关闭弹窗', () => {
    const onClose = vi.fn();
    const onConfirm = vi.fn();
    renderDialog({ message: '要删除吗', onConfirm }, onClose);

    clickConfirm();

    expect(onConfirm).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
