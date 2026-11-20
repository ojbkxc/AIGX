/**
 * PluginIcon —— 图标地址归一化 + 加载失败回落。
 *
 * 覆盖本次插件页图标整改的两条关键行为：
 * 1) 相对路径按站点根拼接（`icons/a.png` → `/icons/a.png`），data/http(s) 原样；
 * 2) <img> 404/解码失败时不撑破卡片，依次尝试候选后回落首字母。
 */
import { describe, expect, it } from 'vitest';
import { render, fireEvent, cleanup } from '@testing-library/react';
import { PluginIcon, resolveIconSrc } from './PluginIcon';

describe('resolveIconSrc', () => {
  it('data / blob / http(s) / 协议相对 原样返回', () => {
    expect(resolveIconSrc('data:image/png;base64,AAAA')).toBe('data:image/png;base64,AAAA');
    expect(resolveIconSrc('blob:https://x/y')).toBe('blob:https://x/y');
    expect(resolveIconSrc('https://cdn.example/x.png')).toBe('https://cdn.example/x.png');
    expect(resolveIconSrc('//cdn.example/x.png')).toBe('//cdn.example/x.png');
  });

  it('根相对路径原样，裸相对路径补前导斜杠', () => {
    expect(resolveIconSrc('/plugins/script-icons/abc.png')).toBe('/plugins/script-icons/abc.png');
    expect(resolveIconSrc('icons/logo.png')).toBe('/icons/logo.png');
    expect(resolveIconSrc('./icons/logo.png')).toBe('/icons/logo.png');
  });

  it('空/非字符串返回空串', () => {
    expect(resolveIconSrc(undefined)).toBe('');
    expect(resolveIconSrc(null)).toBe('');
    expect(resolveIconSrc('   ')).toBe('');
    expect(resolveIconSrc(42)).toBe('');
  });
});

describe('PluginIcon', () => {
  it('渲染归一化后的 <img src>', () => {
    const { container } = render(<PluginIcon src="icons/a.png" label="Alpha" />);
    const img = container.querySelector('img');
    expect(img?.getAttribute('src')).toBe('/icons/a.png');
    expect(container.querySelector('.pl-card-icon-fallback')).toBeNull();
    cleanup();
  });

  it('加载失败回落首字母，且不再保留 img', () => {
    const { container } = render(<PluginIcon src="/broken.png" label="beta" />);
    const img = container.querySelector('img') as HTMLImageElement;
    fireEvent.error(img);
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('.pl-card-icon-fallback')?.textContent).toBe('B');
    cleanup();
  });

  it('多候选按序回退：第一张失败后加载第二张', () => {
    const { container } = render(
      <PluginIcon sources={['/one.png', '/two.png']} label="Gamma" />,
    );
    expect(container.querySelector('img')?.getAttribute('src')).toBe('/one.png');
    fireEvent.error(container.querySelector('img') as HTMLImageElement);
    expect(container.querySelector('img')?.getAttribute('src')).toBe('/two.png');
    fireEvent.error(container.querySelector('img') as HTMLImageElement);
    expect(container.querySelector('.pl-card-icon-fallback')?.textContent).toBe('G');
    cleanup();
  });

  it('无图标时直接回落（空名为 🧩）', () => {
    const { container } = render(<PluginIcon label="" />);
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('.pl-card-icon-fallback')?.textContent).toBe('🧩');
    cleanup();
  });
});
