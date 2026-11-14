import { describe, expect, it, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import i18n from 'i18next';
import LiveCards from './LiveCards';
import '../../i18n';

// 全量套件里别的测试文件会把 localStorage 的 i18n_lang 污染成 en（jsdom 共享环境），
// 断言依赖中文文案 → 每个用例前钉回 zh
beforeEach(async () => {
  localStorage.setItem('i18n_lang', 'zh');
  await i18n.changeLanguage('zh');
});

const rt = {
  qps: 1.2,
  avg_latency_ms: 100,
  error_rate: 2.5,
  errors: 3,
  total_requests: 120,
  active_users: 2,
  active_channels: 1,
};

describe('LiveCards — 实时错误率卡（#22）', () => {
  it('显示 realtime 端点真实返回的 error_rate/errors', () => {
    const { container } = render(
      <LiveCards realtime={rt} realtimePending={false} monthlyUsed={0} monthlyLimit={null} />,
    );
    const text = container.textContent ?? '';
    expect(text).toContain('错误率');
    expect(text).toContain('2.5%');
    expect(text).toContain('3');
    expect(text).toContain('近 5 分钟');
  });

  it('realtime 缺 error_rate 字段时显示 —（不造假）', () => {
    const { container } = render(
      <LiveCards realtime={{ qps: 1 }} realtimePending={false} monthlyUsed={0} monthlyLimit={null} />,
    );
    expect(container.textContent).toContain('—');
  });
});
