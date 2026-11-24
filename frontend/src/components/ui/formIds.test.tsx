/**
 * ui 表单组件的 id 口径：显式 id 优先，否则 useId() 兜底。
 *
 * 复现的原始缺陷：`id` 由 label 文本派生（`'input-' + label`）。同一页出现两个
 * 同 label 的字段时（AccountSessions 就有「优先级 / 权重 / 并发上限」三对），
 * DOM 里出现**重复 id**，`htmlFor` 只认第一个元素——点第二个标签会跳到第一个
 * 输入框，屏幕阅读器也念错控件。
 */
import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import Input from './Input';
import Select from './Select';
import Textarea from './Textarea';
import PasswordInput from './PasswordInput';
import FileInput from './FileInput';

describe('ui 表单组件 id 唯一性', () => {
  it('同 label 的两个 Input 不会撞 id，且 label 各指各的', () => {
    const { container } = render(
      <div>
        <Input label="优先级" defaultValue="1" />
        <Input label="优先级" defaultValue="2" />
      </div>,
    );
    const inputs = Array.from(container.querySelectorAll('input'));
    expect(inputs).toHaveLength(2);
    const ids = inputs.map((i) => i.id);
    expect(ids[0]).toBeTruthy();
    expect(ids[0]).not.toBe(ids[1]);

    // htmlFor 指向各自那个 input
    const labels = Array.from(container.querySelectorAll('label'));
    expect(labels[0].htmlFor).toBe(ids[0]);
    expect(labels[1].htmlFor).toBe(ids[1]);
  });

  it('显式 id 仍然优先（调用方显式指定时不被覆盖）', () => {
    const { container } = render(<Input label="名称" id="my-explicit-id" />);
    const input = container.querySelector('input');
    expect(input?.id).toBe('my-explicit-id');
    expect(container.querySelector('label')?.htmlFor).toBe('my-explicit-id');
  });

  it('Select / Textarea / PasswordInput / FileInput 同样不撞 id', () => {
    const { container } = render(
      <div>
        <Select label="分组"><option>a</option></Select>
        <Select label="分组"><option>b</option></Select>
        <Textarea label="备注" />
        <Textarea label="备注" />
        <PasswordInput label="密码" />
        <PasswordInput label="密码" />
        <FileInput label="附件" />
        <FileInput label="附件" />
      </div>,
    );
    const allIds = Array.from(container.querySelectorAll('input, select, textarea'))
      .map((el) => el.id)
      .filter(Boolean);
    expect(allIds).toHaveLength(8);
    expect(new Set(allIds).size).toBe(allIds.length);
  });

  it('无 label 时保持无 id（不凭空造 id）', () => {
    const { container } = render(<Input placeholder="无标签" />);
    expect(container.querySelector('input')?.id).toBe('');
  });
});
