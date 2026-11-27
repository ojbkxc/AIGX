/**
 * 破甲 · 技能注入的「新建 / 粘贴 / 上传」面板。
 *
 * ── 为什么单独一个组件 ────────────────────────────────────────
 * 技能注入的入口原本只有一个「导入技能包（ZIP/TAR/TGZ）」，而最常用的两条路径
 * ——**新建一份带正确 frontmatter 的 SKILL.md** 和**把手上写好的 .md 粘进去**
 * ——都没有入口。用户对着「技能目录扫不到任何 SKILL.md」的告警只能自己去
 * `~/.aigx/skills/<名字>/SKILL.md` 手搓，而 frontmatter 只要少个 `---`、
 * `keywords` 拼错一个字母，技能就永远不命中且**界面上看起来完全正常**。
 *
 * ── frontmatter 格式的依据 ───────────────────────────────────
 * 骨架由后端 `GET /api/purge/skills/skeleton` 生成（`src/purge/skill_doc.rs::
 * skeleton`），与扫描器的解析函数 `src/purge/mod.rs::parse_skill_parts` 是
 * **同一份契约**（`skill_doc` 的模板测试会用解析器回读自己）。前端**不自己拼
 * frontmatter**——两份实现一旦漂移，就是「模板生成的技能永远不生效」。
 *
 * ── 校验前置 ─────────────────────────────────────────────────
 * 粘贴/上传的文本先经后端 `POST /api/purge/skills` 的解析回读（写盘前就拒掉
 * 「解析器读不出正文」的内容），返回的 `warnings`（缺 keywords / 正文超上限）
 * 直接展示——不写盘也能知道「这份技能会不会生效」。
 */
import { memo, useCallback, useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import type { TFunction } from 'i18next';
import type { PurgeSkillWriteResult } from '../types';
import { api } from '../api';
import { Button, Card, Input, Textarea } from './ui';

export interface SkillComposePanelProps {
  t: TFunction;
  addToast: (message: string, type?: 'success' | 'error' | 'warning') => void;
  /** 技能目录的展开后绝对路径（显示「会写到哪」） */
  skillDirResolved: string;
  /** 目录当前是否真的存在（不存在时提示「保存会顺手建出来」） */
  skillDirExists?: boolean;
  /** 当前注入上限（正文超限会截断） */
  injectLimit?: number;
  /** 写盘成功后刷新技能快照 */
  onWritten: () => void;
}

/** 从粘贴的文本里粗取 frontmatter 的 name（只为预填「目录名」，最终以后端解析为准）。 */
export function nameFromContent(content: string): string {
  const trimmed = content.trimStart();
  if (!trimmed.startsWith('---')) return '';
  const end = trimmed.indexOf('\n---');
  if (end < 0) return '';
  for (const line of trimmed.slice(3, end).split('\n')) {
    const match = /^\s*name:\s*(.+)$/i.exec(line);
    if (match) return match[1].trim().replace(/^"(.*)"$/, '$1');
  }
  return '';
}

export const SkillComposePanel = memo(function SkillComposePanel({
  t,
  addToast,
  skillDirResolved,
  skillDirExists,
  injectLimit,
  onWritten,
}: SkillComposePanelProps): JSX.Element {
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [keywords, setKeywords] = useState('');
  const [body, setBody] = useState('');
  const [content, setContent] = useState('');
  const [overwrite, setOverwrite] = useState(false);
  const [dirName, setDirName] = useState('');
  const [busy, setBusy] = useState(false);
  const [warnings, setWarnings] = useState<string[]>([]);
  const fileRef = useRef<HTMLInputElement>(null);

  /** 生成骨架（后端拼 frontmatter，前端只填进编辑器） */
  const buildSkeleton = useCallback(async (): Promise<void> => {
    if (!name.trim()) {
      addToast(t('先填技能名：它同时是默认关键词与目录名来源'), 'warning');
      return;
    }
    setBusy(true);
    try {
      const res = await api.getPurgeSkillSkeleton({
        name: name.trim(),
        description: description.trim(),
        keywords: keywords.trim(),
        body,
      });
      const data = res?.data;
      if (!data?.content) throw new Error(t('骨架生成失败'));
      setContent(data.content);
      setDirName(data.dir || '');
      const warn = data.validate?.warnings ?? [];
      setWarnings(warn);
      if (data.validate_error) {
        addToast(data.validate_error, 'error');
      } else {
        addToast(
          t('骨架已生成（前端不拼 frontmatter，格式由后端解析器保证）'),
          'success',
        );
      }
    } catch (error) {
      addToast((error as Error)?.message || t('骨架生成失败'), 'error');
    } finally {
      setBusy(false);
    }
  }, [addToast, body, description, keywords, name, t]);

  /** 上传单个 .md：读成文本填进编辑器（不直接写盘，让用户先看一眼） */
  const handleFile = useCallback(
    async (event: ChangeEvent<HTMLInputElement>): Promise<void> => {
      const file = event.target.files?.[0];
      event.target.value = '';
      if (!file) return;
      if (file.size > 2 * 1024 * 1024) {
        addToast(t('文件超过 2 MiB：技能正文是注进 system 的文本，几 MB 已离谱'), 'error');
        return;
      }
      try {
        const text = await file.text();
        setContent(text);
        const guessed = nameFromContent(text);
        if (guessed) {
          setName(guessed);
          setDirName(guessed);
        }
        addToast(t('已读入 {{name}}，确认内容后点「写入技能目录」', { name: file.name }), 'success');
      } catch (error) {
        addToast((error as Error)?.message || t('读取文件失败'), 'error');
      }
    },
    [addToast, t],
  );

  const write = useCallback(async (): Promise<void> => {
    if (!content.trim()) {
      addToast(t('正文为空：先点「生成骨架」或上传一份 .md'), 'warning');
      return;
    }
    setBusy(true);
    try {
      const res = await api.writePurgeSkill({
        content,
        dir: dirName.trim() || undefined,
        overwrite,
      });
      const data: PurgeSkillWriteResult | undefined = res?.data;
      if (!data) throw new Error(t('写入失败'));
      setWarnings(data.warnings ?? []);
      addToast(
        t('已写入 {{path}}（{{chars}} 字符，关键词 {{n}} 个）', {
          path: data.path,
          chars: data.body_chars,
          n: data.keywords.length,
        }),
        'success',
      );
      onWritten();
    } catch (error) {
      addToast((error as Error)?.message || t('写入失败'), 'error');
    } finally {
      setBusy(false);
    }
  }, [addToast, content, dirName, onWritten, overwrite, t]);

  return (
    <Card
      title={t('新建技能（SKILL.md）')}
      actions={
        <Button variant="outline" size="sm" onClick={() => fileRef.current?.click()}>
          {t('上传 .md')}
        </Button>
      }
    >
      <p className="purge-hint" data-testid="purge-skill-compose-dir">
        {skillDirResolved
          ? t('写入位置：{{dir}}/<技能名>/SKILL.md', { dir: skillDirResolved })
          : t('技能目录未解析出来（检查「技能目录」配置）')}
        {skillDirExists === false
          ? t(' · 目录还不存在，写入时会自动建出来')
          : ''}
      </p>
      <div className="purge-form-grid">
        <Input
          label={t('技能名（name）')}
          hint={t('必填；也是默认关键词与目录名来源（目录名会规整成小写连字符）')}
          value={name}
          placeholder={t('例如 pdf-report')}
          onChange={(event) => setName(event.target.value)}
          data-testid="purge-skill-name"
        />
        <Input
          label={t('一句话说明（description）')}
          hint={t('可选；它的词会被解析器派生成关键词（≥2 字符的词）')}
          value={description}
          placeholder={t('例如 导出 PDF 报表：给模型报表结构与口径约束')}
          onChange={(event) => setDescription(event.target.value)}
        />
        <Input
          label={t('关键词（keywords，逗号分隔）')}
          hint={t('可选但强烈建议：消息里命中任一个词就把正文注入 system')}
          value={keywords}
          placeholder={t('例如 报表, 对账, 月结')}
          onChange={(event) => setKeywords(event.target.value)}
          data-testid="purge-skill-keywords"
        />
        <Input
          label={t('目录名（可选）')}
          hint={t('留空则用技能名规整后的结果；同名目录已存在需勾选覆盖')}
          value={dirName}
          placeholder={t('例如 pdf-report')}
          onChange={(event) => setDirName(event.target.value)}
          data-testid="purge-skill-dir"
        />
      </div>
      <Textarea
        label={t('正文占位（可选，生成骨架时用）')}
        rows={2}
        value={body}
        onChange={(event) => setBody(event.target.value)}
        hint={t('留空则由后端给一段占位说明（空正文会被解析器丢弃）')}
      />
      <div className="purge-actions">
        <Button size="sm" disabled={busy} onClick={() => void buildSkeleton()}>
          {busy ? t('处理中…') : t('生成骨架')}
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={busy}
          data-testid="purge-skill-write"
          onClick={() => void write()}
        >
          {t('写入技能目录')}
        </Button>
        <label style={{ display: 'flex', alignItems: 'center', gap: 6, whiteSpace: 'nowrap' }}>
          <input
            type="checkbox"
            checked={overwrite}
            onChange={(event) => setOverwrite(event.target.checked)}
          />
          {t('覆盖同名技能')}
        </label>
        <input
          ref={fileRef}
          type="file"
          accept=".md,text/markdown"
          hidden
          onChange={(event) => void handleFile(event)}
        />
      </div>
      {injectLimit ? (
        <p className="purge-hint">
          {t('当前注入上限 {{limit}} 字符：正文超过它，命中后只注入前面这一截。', {
            limit: injectLimit,
          })}
        </p>
      ) : null}
      <Textarea
        label={t('SKILL.md 内容（可直接编辑）')}
        rows={10}
        value={content}
        onChange={(event) => setContent(event.target.value)}
        hint={t('frontmatter 的 `---` 与 `name:` / `keywords:` 由「生成骨架」产出，手改时别删')}
        data-testid="purge-skill-content"
      />
      {warnings.length > 0 ? (
        <ul className="purge-skill-warnings" data-testid="purge-skill-warnings">
          {warnings.map((warning) => (
            <li key={warning}>{warning}</li>
          ))}
        </ul>
      ) : null}
    </Card>
  );
});