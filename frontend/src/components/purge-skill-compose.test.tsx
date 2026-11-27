/**
 * 破甲 · 新建技能面板的判据。
 *
 * 关键点（也是最容易写错的地方）：
 * 1. **前端不拼 frontmatter**——骨架来自后端 `GET /api/purge/skills/skeleton`，
 *    这里用 mock 断言「确实发了那个请求、并把返回的正文原样填进编辑器」，
 *    而不是断言前端拼出的字符串（那会把契约的单一来源挪到前端来）。
 * 2. 上传 .md 之后**不直接写盘**，只填编辑器（用户先看一眼）。
 * 3. 写盘失败把后端原文抛给用户（不吞成「写入失败」）。
 */
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, fireEvent, waitFor } from '@testing-library/react';
import type { TFunction } from 'i18next';
import { SkillComposePanel, nameFromContent } from './purge-skill-compose';

const t = ((key: string, params?: Record<string, string | number>): string =>
  params
    ? Object.entries(params).reduce(
        (acc, [k, v]) => acc.split(`{{${k}}}`).join(String(v)),
        key,
      )
    : key) as unknown as TFunction;

const mocks = vi.hoisted(() => ({
  getPurgeSkillSkeleton: vi.fn(),
  writePurgeSkill: vi.fn(),
}));

vi.mock('../api', () => ({
  api: {
    getPurgeSkillSkeleton: mocks.getPurgeSkillSkeleton,
    writePurgeSkill: mocks.writePurgeSkill,
  },
}));

const SKELETON = '---\nname: pdf-report\nkeywords: 报表, 对账\n---\n\n（正文占位）\n';

function setup(over: Partial<Parameters<typeof SkillComposePanel>[0]> = {}) {
  const addToast = vi.fn();
  const onWritten = vi.fn();
  const view = render(
    <SkillComposePanel
      t={t}
      addToast={addToast}
      skillDirResolved="/root/.aigx/skills"
      skillDirExists={false}
      injectLimit={1500}
      onWritten={onWritten}
      {...over}
    />,
  );
  return { ...view, addToast, onWritten };
}

describe('nameFromContent', () => {
  it('从 frontmatter 的 name 行取名字（带或不带引号）', () => {
    expect(nameFromContent('---\nname: pdf-report\n---\n正文')).toBe('pdf-report');
    expect(nameFromContent('---\nname: "my skill"\n---\n正文')).toBe('my skill');
  });

  it('没有 frontmatter / 没有 name 行 → 空串（不猜）', () => {
    expect(nameFromContent('直接是正文')).toBe('');
    expect(nameFromContent('---\ndescription: x\n---\n正文')).toBe('');
  });

  it('只认首段的 frontmatter：正文里出现 name: 不算', () => {
    expect(nameFromContent('---\nname: real\n---\nname: fake')).toBe('real');
  });
});

describe('SkillComposePanel', () => {
  beforeEach(() => {
    mocks.getPurgeSkillSkeleton.mockReset();
    mocks.writePurgeSkill.mockReset();
  });

  it('未填名字就点「生成骨架」→ 提示而不是发请求', () => {
    const view = setup();
    fireEvent.click(view.getByText('生成骨架'));
    expect(mocks.getPurgeSkillSkeleton).not.toHaveBeenCalled();
    expect(view.addToast).toHaveBeenCalledWith(expect.stringContaining('先填技能名'), 'warning');
  });

  it('生成骨架：请求后端、把返回正文原样填进编辑器（前端不自己拼）', async () => {
    mocks.getPurgeSkillSkeleton.mockResolvedValue({
      data: {
        content: SKELETON,
        dir: 'pdf-report',
        path: '/root/.aigx/skills/pdf-report/SKILL.md',
        skill_dir_resolved: '/root/.aigx/skills',
        validate: {
          name: 'pdf-report',
          keywords: ['pdf-report', '报表', '对账'],
          body_chars: 20,
          has_keywords_key: true,
          warnings: [],
        },
        validate_error: null,
      },
    });
    const view = setup();
    fireEvent.change(view.getByTestId('purge-skill-name'), {
      target: { value: 'pdf-report' },
    });
    fireEvent.change(view.getByTestId('purge-skill-keywords'), {
      target: { value: '报表, 对账' },
    });
    fireEvent.click(view.getByText('生成骨架'));

    await waitFor(() => expect(mocks.getPurgeSkillSkeleton).toHaveBeenCalled());
    expect(mocks.getPurgeSkillSkeleton).toHaveBeenCalledWith({
      name: 'pdf-report',
      description: '',
      keywords: '报表, 对账',
      body: '',
    });
    const editor = view.getByTestId('purge-skill-content') as HTMLTextAreaElement;
    await waitFor(() => expect(editor.value).toBe(SKELETON));
    // 目录名也跟着后端给的（前端不自己 slugify 一遍）
    const dirInput = view.getByTestId('purge-skill-dir') as HTMLInputElement;
    expect(dirInput.value).toBe('pdf-report');
  });

  it('骨架校验出问题时把后端原文报出来（不吞成「生成失败」）', async () => {
    mocks.getPurgeSkillSkeleton.mockResolvedValue({
      data: {
        content: 'x',
        dir: '',
        path: '',
        skill_dir_resolved: '/r',
        validate: null,
        validate_error: '缺少 name（frontmatter 第一段里的 `name: <技能名>`）',
      },
    });
    const view = setup();
    fireEvent.change(view.getByTestId('purge-skill-name'), { target: { value: 'x' } });
    fireEvent.click(view.getByText('生成骨架'));
    await waitFor(() =>
      expect(view.addToast).toHaveBeenCalledWith(expect.stringContaining('缺少 name'), 'error'),
    );
  });

  it('正文为空就点「写入」→ 提示而不是发请求', () => {
    const view = setup();
    fireEvent.click(view.getByTestId('purge-skill-write'));
    expect(mocks.writePurgeSkill).not.toHaveBeenCalled();
    expect(view.addToast).toHaveBeenCalledWith(expect.stringContaining('正文为空'), 'warning');
  });

  it('写入成功：回显路径/字符数/关键词数，并把 warnings 就地列出', async () => {
    mocks.writePurgeSkill.mockResolvedValue({
      data: {
        mode: 'text',
        dir: 'pdf-report',
        files: 1,
        bytes: SKELETON.length,
        has_skill_md: true,
        name: 'pdf-report',
        keywords: ['pdf-report', '报表'],
        body_chars: 20,
        has_keywords_key: true,
        warnings: ['正文 2000 字符，超过注入上限 1500：命中后只注入前 1500 字符'],
        path: '/root/.aigx/skills/pdf-report/SKILL.md',
      },
    });
    const view = setup();
    fireEvent.change(view.getByTestId('purge-skill-content'), { target: { value: SKELETON } });
    fireEvent.click(view.getByTestId('purge-skill-write'));

    await waitFor(() => expect(view.onWritten).toHaveBeenCalledTimes(1));
    expect(mocks.writePurgeSkill).toHaveBeenCalledWith({
      content: SKELETON,
      dir: undefined,
      overwrite: false,
    });
    expect(view.addToast).toHaveBeenCalledWith(
      expect.stringContaining('/root/.aigx/skills/pdf-report/SKILL.md'),
      'success',
    );
    const warnings = view.getByTestId('purge-skill-warnings');
    expect(warnings.textContent).toContain('超过注入上限');
  });

  it('写入冲突（目录已存在）→ 后端原文透给用户，不重试、不静默', async () => {
    mocks.writePurgeSkill.mockRejectedValue(
      new Error('目标目录已存在：pdf-report（如需覆盖请勾选「覆盖同名技能」）'),
    );
    const view = setup();
    fireEvent.change(view.getByTestId('purge-skill-content'), { target: { value: SKELETON } });
    fireEvent.click(view.getByTestId('purge-skill-write'));

    await waitFor(() =>
      expect(view.addToast).toHaveBeenCalledWith(
        expect.stringContaining('已存在'),
        'error',
      ),
    );
    expect(view.onWritten).not.toHaveBeenCalled();
  });

  it('技能目录不存在时明说「写入会顺手建出来」（这是线上那种空目录态）', () => {
    const view = setup({ skillDirExists: false });
    expect(view.getByTestId('purge-skill-compose-dir').textContent).toContain('自动建出来');
  });
});