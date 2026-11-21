import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ChangeEvent } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api } from '../api';
import type {
  PurgeData,
  PurgePresetDetail,
  PurgePresetList,
  PurgeRewriteRule,
  PurgeSettings,
  PurgeStats,
  PurgeTemplate,
} from '../types';
import {
  Button,
  Card,
  Input,
  Select,
  Skeleton,
  SkeletonCards,
  SkeletonList,
  StatCard,
  Switch,
  Textarea,
} from '../components/ui';
import { downloadText } from '../lib/format';
import { useToast } from '../components/Toast';
import './PurgeConsole.css';

type BoolKey =
  | 'enabled'
  | 'system_normalize'
  | 'dead_zone_rewrite'
  | 'synonym_enabled'
  | 'reduce_reasoning'
  | 'cross_source_fallback'
  | 'skill_inject'
  | 'structured_prompt';

const SWITCHES: Array<{ key: BoolKey; label: string; desc: string }> = [
  {
    key: 'enabled',
    label: '破甲总开关',
    desc: '关闭后只做纯转发，不判定、不重试、不改写',
  },
  {
    key: 'system_normalize',
    label: 'system 归一化',
    desc: '合并连续 system、去重、折叠空行，功能性约束保留',
  },
  {
    key: 'dead_zone_rewrite',
    label: '死区词改写',
    desc: '按下方词表把易触发拒绝的措辞换成中性表述',
  },
  {
    key: 'synonym_enabled',
    label: '同义词替换',
    desc: '按同义词表把易触发拒绝的词换成语义相近的中性词（与死区词改写并列）',
  },
  {
    key: 'reduce_reasoning',
    label: '降思考',
    desc: '把 reasoning_effort 压到下限，防预算烧在思考上',
  },
  {
    key: 'cross_source_fallback',
    label: '异源递进',
    desc: '主模型重试到触发轮次仍失败时改用异源模型重新采样',
  },
  {
    key: 'skill_inject',
    label: '技能注入',
    desc: '命中关键词后把对应 SKILL.md 追加到 user 消息尾部',
  },
  {
    key: 'structured_prompt',
    label: '结构化提示',
    desc: '首轮追加格式指令，减少空答',
  },
];

/** 可编辑数字字段：输入态用字符串镜像（允许清空/半输入），保存时统一校验并夹取，杜绝 NaN */
type NumKey = 'skill_inject_limit' | 'fallback_trigger_attempt' | 'retry_limit';

const NUM_FIELDS: Array<{ key: NumKey; label: string; min: number; max: number }> = [
  { key: 'skill_inject_limit', label: '技能注入上限（字符）', min: 16, max: 20000 },
  { key: 'fallback_trigger_attempt', label: '异源触发轮次', min: 1, max: 32 },
  { key: 'retry_limit', label: '重试上限', min: 1, max: 32 },
];

function splitList(value: string): string[] {
  return value
    .split(/[,，\s]+/)
    .map((item) => item.trim())
    .filter(Boolean);
}

/** 换行分隔的词表（保留含逗号/空格的短语原文，拒绝话术词表用） */
function splitLines(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter(Boolean);
}

/**
 * 找出词表中重复的 key（大小写不敏感）。
 * 返回值是「重复的原文 → 出现下标列表」，首个出现项不算冲突目标，
 * 用于行内提示「该原文与第 N 行重复」。
 */
function findDuplicates(items: string[]): Map<string, number[]> {
  const seen = new Map<string, number[]>();
  items.forEach((item, index) => {
    const key = item.trim().toLowerCase();
    if (!key) return;
    const list = seen.get(key);
    if (list) list.push(index);
    else seen.set(key, [index]);
  });
  // 只保留真正出现 ≥2 次的
  for (const [key, list] of seen) {
    if (list.length < 2) seen.delete(key);
  }
  return seen;
}

/** 降档目标候选（clamp_reasoning 只压 high/medium/xhigh/max 档） */
const REASONING_FLOORS: Array<{ value: string; label: string }> = [
  { value: 'low', label: 'low（默认）' },
  { value: 'medium', label: 'medium' },
  { value: 'high', label: 'high' },
  { value: 'minimal', label: 'minimal' },
];

function formatUptime(total: number, units: { sec: string; min: string; hour: string; day: string }): string {
  const seconds = Math.max(0, Math.floor(total));
  if (seconds < 60) return `${seconds} ${units.sec}`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} ${units.min}`;
  if (seconds < 86400) {
    return `${Math.floor(seconds / 3600)} ${units.hour} ${Math.floor((seconds % 3600) / 60)} ${units.min}`;
  }
  return `${Math.floor(seconds / 86400)} ${units.day}`;
}

/** 数字字段字符串镜像初值（从服务端配置派生） */
function numRawOf(s: PurgeSettings): Record<NumKey, string> {
  return {
    skill_inject_limit: String(s.skill_inject_limit),
    fallback_trigger_attempt: String(s.fallback_trigger_attempt),
    retry_limit: String(s.retry_limit),
  };
}

/** 按 NumKey 写回数字字段（显式分支，避免计算键的宽化类型） */
function withNum(settings: PurgeSettings, key: NumKey, value: number): PurgeSettings {
  const next = { ...settings };
  if (key === 'skill_inject_limit') next.skill_inject_limit = value;
  else if (key === 'fallback_trigger_attempt') next.fallback_trigger_attempt = value;
  else next.retry_limit = value;
  return next;
}

/**
 * 词表单行（memo）：长词表下编辑一行只重渲染该行——行内两个 Input 是全页最贵的子树，
 * 而父组件每次按键都会新建 draft（未命中行的 rule 引用不变）。
 * 标签按值传递（字符串）而非对象，memo 的浅比较才有意义。
 */
const RuleRow = memo(function RuleRow({
  rule,
  index,
  fromLabel,
  toLabel,
  removeLabel,
  dupHint,
  onPatch,
  onRemove,
}: {
  rule: PurgeRewriteRule;
  index: number;
  fromLabel: string;
  toLabel: string;
  removeLabel: string;
  /** 与其他行原文重复时的行内提示（后端会静默截断重复规则，前端显式提示） */
  dupHint?: string;
  onPatch: (index: number, part: Partial<PurgeRewriteRule>) => void;
  onRemove: (index: number) => void;
}): JSX.Element {
  return (
    <div className="purge-rule-row">
      <div>
        <Input
          placeholder={fromLabel}
          aria-label={fromLabel}
          value={rule.from}
          onChange={(event) => onPatch(index, { from: event.target.value })}
        />
        {dupHint ? <span className="purge-dup-hint">{dupHint}</span> : null}
      </div>
      <span className="purge-arrow" aria-hidden="true">→</span>
      <Input
        placeholder={toLabel}
        aria-label={toLabel}
        value={rule.to}
        onChange={(event) => onPatch(index, { to: event.target.value })}
      />
      <Button variant="outline" size="sm" onClick={() => onRemove(index)}>
        {removeLabel}
      </Button>
    </div>
  );
});

/**
 * 统计卡组（memo）：入参只有服务端快照派生值，编辑表单文本时不重画。
 */
const StatsGrid = memo(function StatsGrid({
  stats,
  retryRate,
  t,
}: {
  stats: PurgeStats;
  retryRate: string;
  t: TFunction;
}): JSX.Element {
  return (
    <div className="purge-stats" role="group" aria-label={t('统计')}>
      <StatCard title={t('响应总数')} value={stats.responses} />
      <StatCard title={t('重试率')} value={`${retryRate}%`} />
      <StatCard title={t('重试次数')} value={stats.retries} />
      <StatCard title={t('提前收效')} value={stats.early_success} />
      <StatCard title={t('技能命中')} value={stats.skill_hits} />
      <StatCard title={t('拒绝样本')} value={stats.refusals} />
      <StatCard title={t('同义词命中')} value={stats.synonym_hits} />
    </div>
  );
});

/**
 * 失败分类条（memo）：把 refusal/empty/filter/other 四个终态失败计数
 * 从参数网格里的「N / N / N / N」文本升级为带语义色的横条，一眼看出失败构成。
 */
const FailureBar = memo(function FailureBar({
  refusal,
  empty,
  filter,
  other,
  t,
}: {
  refusal: number;
  empty: number;
  filter: number;
  other: number;
  t: TFunction;
}): JSX.Element {
  const items = [
    { label: t('拒绝'), value: refusal, tone: 'danger' },
    { label: t('空答'), value: empty, tone: 'warning' },
    { label: t('过滤'), value: filter, tone: 'accent' },
    { label: t('其它'), value: other, tone: 'muted' },
  ];
  return (
    <div className="purge-failure-bar" role="group" aria-label={t('失败分类')}>
      {items.map((item) => (
        <div key={item.label} className={`purge-failure-cell tone-${item.tone}`}>
          <span className="purge-failure-value">{item.value}</span>
          <span className="purge-failure-label">{item.label}</span>
        </div>
      ))}
    </div>
  );
});

/**
 * 开关行（memo）：`flags` 由父组件 useMemo 按 8 个原语派生，数组引用只在开关本身变化时变，
 * 故编辑文本/词表不会连带重渲染这 8 行。
 */
const SwitchRows = memo(function SwitchRows({
  flags,
  disabled,
  onToggle,
  t,
}: {
  flags: boolean[];
  disabled: boolean;
  onToggle: (key: BoolKey, next: boolean) => void;
  t: TFunction;
}): JSX.Element {
  return (
    <div className="purge-rows">
      {SWITCHES.map((item, index) => (
        <div key={item.key} className="purge-switch-row">
          <div className="purge-switch-text">
            <div className="purge-row-title">{t(item.label)}</div>
            <div className="purge-row-desc">{t(item.desc)}</div>
          </div>
          <Switch
            checked={flags[index]}
            onChange={(next) => onToggle(item.key, next)}
            disabled={disabled}
            ariaLabel={t(item.label)}
          />
        </div>
      ))}
    </div>
  );
});

/**
 * 只读参数网格（memo）：全部原语入参（含 runtime.inflight 即时并发数），文本编辑不牵连。
 */
const ParamGrid = memo(function ParamGrid({
  retryLimit,
  failureRefusal,
  failureEmpty,
  failureFilter,
  failureOther,
  fallbackModels,
  fallbackTriggerAttempt,
  skillInjectLimit,
  inflight,
  upstream,
  t,
}: {
  retryLimit: number;
  failureRefusal: number;
  failureEmpty: number;
  failureFilter: number;
  failureOther: number;
  fallbackModels: string;
  fallbackTriggerAttempt: number;
  skillInjectLimit: number;
  inflight: number | undefined;
  upstream: string;
  t: TFunction;
}): JSX.Element {
  return (
    <div className="purge-param-grid">
      <div className="purge-param">
        <span>{t('重试上限')}</span>
        <strong>{retryLimit}</strong>
      </div>
      <div className="purge-param">
        <span>{t('失败分类（拒绝/空答/过滤/其它）')}</span>
        <strong>
          {failureRefusal} / {failureEmpty} / {failureFilter} / {failureOther}
        </strong>
      </div>
      <div className="purge-param">
        <span>{t('异源模型')}</span>
        <strong>{fallbackModels || '—'}</strong>
      </div>
      <div className="purge-param">
        <span>{t('异源触发轮次')}</span>
        <strong>{fallbackTriggerAttempt}</strong>
      </div>
      <div className="purge-param">
        <span>{t('技能注入上限')}</span>
        <strong>{skillInjectLimit}</strong>
      </div>
      <div className="purge-param">
        <span>{t('并发')}</span>
        <strong>{typeof inflight === 'number' ? inflight : '—'}</strong>
      </div>
      <div className="purge-param">
        <span>{t('上游')}</span>
        <strong>{upstream || '—'}</strong>
      </div>
    </div>
  );
});

/**
 * 破甲预设库：内置 prompt 集合的浏览与落盘。
 *
 * 三条动作各自独立：
 * - 设为结构化模板：把正文写进父组件的草稿（不直接 PUT，需手动保存）；
 * - 复制全文：只进剪贴板；
 * - 写入技能目录：后端直接写盘，成功后刷新技能快照。
 *
 * 搜索防抖 250ms：预设库有 50+ 条，每次按键都打后端没必要。
 */
const PresetsSection = memo(function PresetsSection({
  t,
  addToast,
  onApplyTemplate,
  onSkillWritten,
}: {
  t: TFunction;
  addToast: (message: string, type?: 'success' | 'error' | 'warning') => void;
  onApplyTemplate: (name: string, text: string) => void;
  onSkillWritten: () => void;
}): JSX.Element {
  const [q, setQ] = useState('');
  const [debouncedQ, setDebouncedQ] = useState('');
  const [vendor, setVendor] = useState('');
  const [list, setList] = useState<PurgePresetList | null>(null);
  const [loading, setLoading] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<PurgePresetDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedQ(q.trim()), 250);
    return () => window.clearTimeout(timer);
  }, [q]);

  // 列表：搜索词 / 厂商变化时重拉（不传 limit，用后端默认 50）
  useEffect(() => {
    let alive = true;
    setLoading(true);
    void (async () => {
      try {
        const res = await api.listPurgePresets({
          q: debouncedQ || undefined,
          vendor: vendor || undefined,
        });
        if (!alive) return;
        if (!res.success || !res.data) throw new Error('list failed');
        setList(res.data);
      } catch {
        if (alive) addToast(t('预设列表加载失败'), 'error');
      } finally {
        if (alive) setLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [addToast, debouncedQ, t, vendor]);

  // 详情：选中项变化时拉全文
  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    let alive = true;
    setDetailLoading(true);
    void (async () => {
      try {
        const res = await api.getPurgePreset(selectedId);
        if (!alive) return;
        if (!res.success || !res.data) throw new Error('detail failed');
        setDetail(res.data);
      } catch {
        if (alive) {
          setDetail(null);
          addToast(t('预设正文加载失败'), 'error');
        }
      } finally {
        if (alive) setDetailLoading(false);
      }
    })();
    return () => {
      alive = false;
    };
  }, [addToast, selectedId, t]);

  const items = list?.items ?? [];

  /** 拉全文（供复制用；详情已在时直接用缓存） */
  const fetchBody = useCallback(
    async (id: string): Promise<PurgePresetDetail | null> => {
      if (detail && detail.id === id) return detail;
      const res = await api.getPurgePreset(id);
      if (!res.success || !res.data) throw new Error('detail failed');
      return res.data;
    },
    [detail],
  );

  const handleApplyTemplate = useCallback(
    async (id: string): Promise<void> => {
      setBusy(`template:${id}`);
      try {
        const full = await fetchBody(id);
        if (!full) throw new Error('detail failed');
        onApplyTemplate(full.title, full.body);
        addToast(t('已写入模板草稿，点底部「保存」后生效'), 'success');
      } catch {
        addToast(t('应用预设失败'), 'error');
      } finally {
        setBusy(null);
      }
    },
    [addToast, fetchBody, onApplyTemplate, t],
  );

  const handleCopy = useCallback(
    async (id: string): Promise<void> => {
      setBusy(`copy:${id}`);
      try {
        const full = await fetchBody(id);
        if (!full) throw new Error('detail failed');
        await navigator.clipboard.writeText(full.body);
        addToast(t('全文已复制到剪贴板'), 'success');
      } catch {
        addToast(t('复制失败（浏览器可能禁用了剪贴板权限）'), 'error');
      } finally {
        setBusy(null);
      }
    },
    [addToast, fetchBody, t],
  );

  const handleApplySkill = useCallback(
    async (id: string): Promise<void> => {
      setBusy(`skill:${id}`);
      try {
        const res = await api.applyPurgePreset(id, 'skill_file');
        if (!res.success || !res.data) throw new Error('apply failed');
        addToast(
          t('已写入技能目录：{{path}}', { path: res.data.applied.path ?? '' }),
          'success',
        );
        onSkillWritten();
      } catch {
        addToast(t('写入技能目录失败'), 'error');
      } finally {
        setBusy(null);
      }
    },
    [addToast, onSkillWritten, t],
  );

  return (
    <Card title={t('破甲预设库')}>
      {/* 授权边界必须常驻可见：正文是第三方公开 prompt 原文，这里只搬不改 */}
      <p className="purge-preset-notice">
        {t(
          '内置 {{count}} 条第三方公开 prompt 原文，仅供授权的防御性安全评估使用。内容原样搬运、不改写；合规责任在使用方。',
          { count: list?.total ?? 0 },
        )}
      </p>
      <div className="purge-preset-toolbar">
        <Input
          placeholder={t('搜索标题 / 厂商 / 模型 / 标签')}
          aria-label={t('搜索预设')}
          value={q}
          onChange={(event) => setQ(event.target.value)}
        />
        <span className="purge-preset-total">
          {t('共 {{total}} 条', { total: list?.total ?? 0 })}
        </span>
      </div>
      <div className="purge-preset-chips" role="group" aria-label={t('按厂商过滤')}>
        <button
          type="button"
          className={`purge-preset-chip${vendor === '' ? ' is-active' : ''}`}
          onClick={() => setVendor('')}
        >
          {t('全部')}
        </button>
        {(list?.vendors ?? []).map((item) => (
          <button
            key={item.vendor}
            type="button"
            className={`purge-preset-chip${vendor === item.vendor ? ' is-active' : ''}`}
            onClick={() => setVendor(item.vendor)}
          >
            {item.vendor}
            <span className="purge-preset-chip-count">{item.count}</span>
          </button>
        ))}
      </div>

      {loading ? (
        <SkeletonList rows={4} />
      ) : items.length === 0 ? (
        <p className="purge-empty">{t('没有匹配的预设')}</p>
      ) : (
        <ul className="purge-preset-list">
          {items.map((item) => {
            const active = selectedId === item.id;
            return (
              <li key={item.id}>
                <button
                  type="button"
                  className={`purge-preset-item${active ? ' is-active' : ''}`}
                  data-testid="purge-preset-item"
                  aria-pressed={active}
                  onClick={() => setSelectedId(active ? null : item.id)}
                >
                  <span className="purge-preset-item-main">
                    <strong>{item.title}</strong>
                    <span className="purge-preset-item-meta">
                      {item.vendor} · {item.model}
                    </span>
                  </span>
                  <span className="purge-preset-item-side">
                    <span>{(item.bytes / 1024).toFixed(1)} KB</span>
                    <code title={item.sha256}>{item.sha256.slice(0, 8)}</code>
                  </span>
                </button>
                {active ? (
                  <div className="purge-preset-detail">
                    <div className="purge-actions">
                      <Button
                        variant="outline"
                        size="sm"
                        data-testid="purge-preset-apply-template"
                        disabled={busy !== null}
                        onClick={() => void handleApplyTemplate(item.id)}
                      >
                        {busy === `template:${item.id}` ? t('处理中…') : t('设为结构化模板')}
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={busy !== null}
                        onClick={() => void handleCopy(item.id)}
                      >
                        {busy === `copy:${item.id}` ? t('处理中…') : t('复制全文')}
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        data-testid="purge-preset-apply-skill"
                        disabled={busy !== null}
                        onClick={() => void handleApplySkill(item.id)}
                      >
                        {busy === `skill:${item.id}` ? t('处理中…') : t('写入技能目录')}
                      </Button>
                    </div>
                    {detailLoading ? (
                      <SkeletonList rows={6} />
                    ) : (
                      <pre className="purge-preset-preview" data-testid="purge-preset-preview">
                        {detail && detail.id === item.id ? detail.body : item.preview}
                      </pre>
                    )}
                  </div>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
});

export default function PurgeConsole(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const [data, setData] = useState<PurgeData | null>(null);
  const [draft, setDraft] = useState<PurgeSettings | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  /**
   * 未保存修改标记：任何编辑置真，加载/保存成功置假。
   * 不用 `JSON.stringify(draft) !== JSON.stringify(data.settings)` 对比——长词表/长后缀下
   * 那是每按键一次的 O(全量) 序列化；草稿变化点全在本组件内，布尔标记语义等价且零分配。
   * 开关是「点一下立即持久化」，故不入 dirty。
   */
  const [dirty, setDirty] = useState(false);
  const [numRaw, setNumRaw] = useState<Record<NumKey, string>>({
    skill_inject_limit: '',
    fallback_trigger_attempt: '',
    retry_limit: '',
  });
  const [numErrors, setNumErrors] = useState<Partial<Record<NumKey, string>>>({});
  // 改写预览（dry-run）：样例输入 + 改写结果（不写统计）
  const [previewInput, setPreviewInput] = useState('');
  const [previewOut, setPreviewOut] = useState<string | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const [previewMeta, setPreviewMeta] = useState<{ changed: boolean; skill_hit: boolean } | null>(null);
  // 配置体检告警 + 技能快照（GET /api/purge 附带；旧版后端不返回则无展示）
  const [configWarnings, setConfigWarnings] = useState<string[]>([]);
  const [skills, setSkills] = useState<NonNullable<PurgeData['skills']>>([]);
  // 导入配置文件选择
  const importInputRef = useRef<HTMLInputElement | null>(null);
  // 技能包导入（ZIP/TAR/TGZ → 技能目录）
  const [skillPkgName, setSkillPkgName] = useState('');
  const [skillPkgOverwrite, setSkillPkgOverwrite] = useState(false);
  const [skillPkgImporting, setSkillPkgImporting] = useState(false);
  const skillPkgInputRef = useRef<HTMLInputElement | null>(null);

  // PUT 互斥：开关乐观保存与底部保存共用一把锁，防止并发 PUT 互相覆盖
  const savingRef = useRef(false);
  // 服务端快照 / 草稿的非渲染引用：开关保存要以「最近一次服务端配置」为基（不夹带未保存编辑），
  // 同时让 toggle 保持稳定引用（Switch 行 memo 才有意义），故不闭包捕获 state。
  const dataRef = useRef<PurgeData | null>(null);
  const draftRef = useRef<PurgeSettings | null>(null);
  useEffect(() => {
    dataRef.current = data;
    draftRef.current = draft;
  }, [data, draft]);
  // 词表增删后的焦点回归：'add' → 最后一行首个输入；数字 → 该下标行（删除后补位行）
  const rulesRef = useRef<HTMLDivElement | null>(null);
  const focusRule = useRef<number | 'add' | null>(null);
  // 同义词词表增删后的焦点回归（与死区词表独立，互不干扰）
  const synRulesRef = useRef<HTMLDivElement | null>(null);
  const focusSynRule = useRef<number | 'add' | null>(null);

  // 规则重复检测（大小写不敏感，按 trim 后原文）：
  // 死区词表 / 同义词表 / 模板名各自独立检测；重复项保存前给行内提示（不阻断保存，
  // 与后端 normalize 的静默去重互补——后端截断静默丢弃，前端显式提示）。
  const dupRules = useMemo(
    () => findDuplicates(draft?.rewrite_rules.map((r) => r.from) ?? []),
    [draft?.rewrite_rules],
  );
  const dupSynonyms = useMemo(
    () => findDuplicates(draft?.synonym_pairs.map((r) => r.from) ?? []),
    [draft?.synonym_pairs],
  );
  const dupTemplates = useMemo(
    () => findDuplicates(draft?.structured_templates?.map((tpl) => tpl.name) ?? []),
    [draft?.structured_templates],
  );

  // 未保存离开保护：关闭/刷新页签前弹浏览器原生确认（dirty 时才挂监听）
  useEffect(() => {
    if (!dirty) return;
    const handler = (event: BeforeUnloadEvent): void => {
      event.preventDefault();
      // Chrome 需要returnValue 才会弹确认框；文案由浏览器统一提供，不可自定义
      event.returnValue = '';
    };
    window.addEventListener('beforeunload', handler);
    return () => window.removeEventListener('beforeunload', handler);
  }, [dirty]);

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const res = await api.getPurge();
      if (!res.success || !res.data) throw new Error('load failed');
      setData(res.data);
      setDraft(res.data.settings);
      setNumRaw(numRawOf(res.data.settings));
      setNumErrors({});
      setConfigWarnings(res.data.config_warnings ?? []);
      setSkills(res.data.skills ?? []);
      setDirty(false);
    } catch {
      addToast(t('加载失败'), 'error');
    } finally {
      setLoading(false);
    }
  }, [addToast, t]);

  useEffect(() => {
    void load();
  }, [load]);

  /** 底部保存：校验数字 → 夹取到合法区间 → PUT 整份草稿 */
  const save = useCallback(async (): Promise<void> => {
    const snapshot = dataRef.current;
    const current = draftRef.current;
    if (!current || !snapshot || savingRef.current) return;
    const invalid = NUM_FIELDS.filter((f) => {
      const raw = numRaw[f.key].trim();
      return raw === '' || !Number.isFinite(Number(raw));
    });
    if (invalid.length > 0) {
      setNumErrors(Object.fromEntries(invalid.map((f) => [f.key, t('请输入有效数字')])));
      addToast(t('存在无效输入，请检查标红的字段'), 'warning');
      return;
    }
    // 模板重名阻断保存：active_template 按名匹配，重名会让「当前启用模板」悬空命中错行
    if (dupTemplates.size > 0) {
      addToast(t('存在重复的模板名称，请先修改再保存'), 'warning');
      return;
    }
    let next = current;
    const rawNext = { ...numRaw };
    for (const f of NUM_FIELDS) {
      const clamped = Math.min(f.max, Math.max(f.min, Math.trunc(Number(numRaw[f.key]))));
      next = withNum(next, f.key, clamped);
      rawNext[f.key] = String(clamped);
    }
    savingRef.current = true;
    setSaving(true);
    setNumRaw(rawNext);
    try {
      const res = await api.savePurge(next);
      if (!res.success || !res.data) throw new Error('save failed');
      setData(res.data);
      setDraft(res.data.settings);
      setNumRaw(numRawOf(res.data.settings));
      setNumErrors({});
      setDirty(false);
      addToast(t('已保存'), 'success');
    } catch {
      addToast(t('保存失败'), 'error');
    } finally {
      savingRef.current = false;
      setSaving(false);
    }
  }, [addToast, dupTemplates.size, numRaw, t]);

  /**
   * 开关：乐观翻本键，失败只回滚该键。
   * PUT 以服务端最近一次已知配置为基（只翻这一个键），不夹带草稿里未保存的文本编辑；
   * 成功后同步 data（服务端真相）并把该键对齐服务端归一化结果，draft 的其它未保存编辑原样保留。
   * 引用稳定（依赖 refs 而非 state），使 Switch 行可被 memo 跳过重渲染。
   */
  const toggle = useCallback((key: BoolKey, next: boolean): void => {
    const snapshot = dataRef.current;
    const current = draftRef.current;
    if (!snapshot || !current || savingRef.current) return;
    const prev = Boolean(current[key]);
    if (prev === next) return;
    savingRef.current = true;
    setSaving(true);
    setDraft({ ...current, [key]: next } as PurgeSettings);
    void (async () => {
      try {
        const res = await api.savePurge({ ...snapshot.settings, [key]: next } as PurgeSettings);
        if (!res.success || !res.data) throw new Error('toggle failed');
        const fresh = res.data;
        setData(fresh);
        setDraft((d) => (d ? ({ ...d, [key]: Boolean(fresh.settings[key]) } as PurgeSettings) : d));
      } catch {
        setDraft((d) => (d ? ({ ...d, [key]: prev } as PurgeSettings) : d));
        addToast(t('保存失败'), 'error');
      } finally {
        savingRef.current = false;
        setSaving(false);
      }
    })();
  }, [addToast, t]);

  const patch = useCallback((part: Partial<PurgeSettings>): void => {
    setDirty(true);
    setDraft((d) => (d ? { ...d, ...part } : d));
  }, []);

  const patchRule = useCallback((index: number, part: Partial<PurgeRewriteRule>): void => {
    setDirty(true);
    setDraft((d) =>
      d
        ? {
            ...d,
            rewrite_rules: d.rewrite_rules.map((rule, i) => (i === index ? { ...rule, ...part } : rule)),
          }
        : d,
    );
  }, []);

  const addRule = useCallback((): void => {
    setDirty(true);
    focusRule.current = 'add';
    setDraft((d) => (d ? { ...d, rewrite_rules: [...d.rewrite_rules, { from: '', to: '' }] } : d));
  }, []);

  const removeRule = useCallback((index: number): void => {
    setDirty(true);
    focusRule.current = index;
    setDraft((d) => (d ? { ...d, rewrite_rules: d.rewrite_rules.filter((_, i) => i !== index) } : d));
  }, []);

  const patchSynonymRule = useCallback((index: number, part: Partial<PurgeRewriteRule>): void => {
    setDirty(true);
    setDraft((d) =>
      d
        ? {
            ...d,
            synonym_pairs: d.synonym_pairs.map((rule, i) => (i === index ? { ...rule, ...part } : rule)),
          }
        : d,
    );
  }, []);

  const addSynonymRule = useCallback((): void => {
    setDirty(true);
    focusSynRule.current = 'add';
    setDraft((d) => (d ? { ...d, synonym_pairs: [...d.synonym_pairs, { from: '', to: '' }] } : d));
  }, []);

  const removeSynonymRule = useCallback((index: number): void => {
    setDirty(true);
    focusSynRule.current = index;
    setDraft((d) => (d ? { ...d, synonym_pairs: d.synonym_pairs.filter((_, i) => i !== index) } : d));
  }, []);

  /** 结构化模板编辑：空模板表按空数组处理（旧后端不返回该字段） */
  const patchTemplate = useCallback((index: number, part: Partial<PurgeTemplate>): void => {
    setDirty(true);
    setDraft((d) =>
      d
        ? {
            ...d,
            structured_templates: (d.structured_templates ?? []).map((tpl, i) =>
              i === index ? { ...tpl, ...part } : tpl,
            ),
          }
        : d,
    );
  }, []);

  /**
   * 预设落盘后的技能快照刷新。
   *
   * 只重拉 `skills`，**不动 draft**——预设页与表单是两套状态，整页 `load()` 会把
   * 用户未保存的编辑抹掉。
   */
  const refreshSkills = useCallback(async (): Promise<void> => {
    try {
      const res = await api.getPurge();
      if (res.success && res.data) setSkills(res.data.skills ?? []);
    } catch {
      // 刷新失败不影响写入结果，静默即可
    }
  }, []);

  /** 技能包导入（ZIP/TAR/TGZ → 技能目录）；成功后只刷新技能快照，不动草稿 */
  const handleImportSkill = useCallback(
    async (event: ChangeEvent<HTMLInputElement>): Promise<void> => {
      const file = event.target.files?.[0];
      event.target.value = '';
      if (!file) return;
      setSkillPkgImporting(true);
      try {
        const res = await api.importPurgeSkills(file, {
          name: skillPkgName.trim() || undefined,
          overwrite: skillPkgOverwrite,
        });
        if (!res.success || !res.data) throw new Error(t('导入技能包失败'));
        addToast(
          t('已导入技能包：{{dir}}（{{files}} 个文件）', {
            dir: res.data.dir,
            files: res.data.files,
          }),
          'success',
        );
        await refreshSkills();
      } catch (err) {
        addToast(err instanceof Error ? err.message : t('导入技能包失败'), 'error');
      } finally {
        setSkillPkgImporting(false);
      }
    },
    [addToast, refreshSkills, skillPkgName, skillPkgOverwrite, t],
  );

  /** 预设 → 结构化模板草稿（同名 upsert；不直接 PUT，用户手动保存） */
  const applyPresetTemplate = useCallback((name: string, text: string): void => {
    setDirty(true);
    setDraft((d) => {
      if (!d) return d;
      const templates = [...(d.structured_templates ?? [])];
      const index = templates.findIndex((tpl) => tpl.name === name);
      if (index >= 0) templates[index] = { name, text };
      else templates.push({ name, text });
      return { ...d, structured_templates: templates };
    });
  }, []);

  const addTemplate = useCallback((): void => {
    setDirty(true);
    setDraft((d) =>
      d ? { ...d, structured_templates: [...(d.structured_templates ?? []), { name: '', text: '' }] } : d,
    );
  }, []);

  const removeTemplate = useCallback((index: number): void => {
    setDirty(true);
    setDraft((d) => {
      if (!d) return d;
      const templates = (d.structured_templates ?? []).filter((_, i) => i !== index);
      // 删除的正是当前启用模板 → 清空选择（回退 structured_suffix），避免悬空引用
      const removed = (d.structured_templates ?? [])[index];
      const active = d.active_template && removed && d.active_template === removed.name ? '' : d.active_template;
      return { ...d, structured_templates: templates, active_template: active ?? '' };
    });
  }, []);

  /** 改写预览：以当前草稿为配置试跑样例输入（dry-run，不写统计、不落库） */
  const runPreview = useCallback(async (): Promise<void> => {
    const current = draftRef.current;
    if (!current || previewing) return;
    if (!previewInput.trim()) {
      addToast(t('请输入预览样例'), 'warning');
      return;
    }
    setPreviewing(true);
    try {
      const res = await api.previewPurge({
        settings: current,
        messages: [{ role: 'user', content: previewInput }],
      });
      if (!res.success || !res.data) throw new Error('preview failed');
      const lastUser = [...res.data.messages].reverse().find((m) => m.role === 'user');
      setPreviewOut(lastUser?.content ?? '');
      setPreviewMeta(res.data.applied);
    } catch {
      addToast(t('预览失败'), 'error');
    } finally {
      setPreviewing(false);
    }
  }, [addToast, previewInput, previewing, t]);

  /** 导出配置：当前服务端快照 → JSON 文件（含全部词表/模板/开关） */
  const exportConfig = useCallback((): void => {
    const snapshot = dataRef.current;
    if (!snapshot) return;
    const filename = `aigx-purge-config-${new Date().toISOString().slice(0, 10)}.json`;
    downloadText(filename, JSON.stringify({ version: 1, settings: snapshot.settings }, null, 2) + '\n');
    addToast(t('配置已导出'), 'success');
  }, [addToast, t]);

  /** 导入配置：读取 JSON → 校验结构 → 填入草稿（不直接保存，确认后再点保存） */
  const importConfig = useCallback(async (file: File | null): Promise<void> => {
    if (!file) return;
    try {
      const text = await file.text();
      const parsed = JSON.parse(text) as { version?: number; settings?: PurgeSettings };
      const settings = parsed?.settings;
      if (!settings || typeof settings !== 'object' || !Array.isArray(settings.rewrite_rules)) {
        throw new Error('invalid shape');
      }
      setDraft(settings);
      setNumRaw(numRawOf(settings));
      setNumErrors({});
      setDirty(true);
      setPreviewOut(null);
      setPreviewMeta(null);
      addToast(t('配置已导入草稿，检查无误后点「保存」生效'), 'success');
    } catch {
      addToast(t('导入失败：不是有效的破甲配置文件'), 'error');
    }
  }, [addToast, t]);

  // 词表增删后把焦点落回列表内，避免按钮随行卸载后焦点掉到 body
  useEffect(() => {
    if (focusRule.current === null || !draft) return;
    const target = focusRule.current;
    focusRule.current = null;
    const rows = rulesRef.current?.querySelectorAll<HTMLElement>('.purge-rule-row');
    if (!rows || rows.length === 0) return;
    const idx = target === 'add' ? rows.length - 1 : Math.min(target, rows.length - 1);
    rows[idx]?.querySelector<HTMLInputElement>('input')?.focus();
  }, [draft]);

  // 同义词词表增删后焦点回归（与死区词表独立）
  useEffect(() => {
    if (focusSynRule.current === null || !draft) return;
    const target = focusSynRule.current;
    focusSynRule.current = null;
    const rows = synRulesRef.current?.querySelectorAll<HTMLElement>('.purge-rule-row');
    if (!rows || rows.length === 0) return;
    const idx = target === 'add' ? rows.length - 1 : Math.min(target, rows.length - 1);
    rows[idx]?.querySelector<HTMLInputElement>('input')?.focus();
  }, [draft]);

  /** 数字输入：字符串镜像实时更新；能解析出有限数才提交进 draft，杜绝 NaN/空值进配置 */
  const handleNumChange = useCallback((key: NumKey, raw: string): void => {
    setDirty(true);
    setNumRaw((prev) => ({ ...prev, [key]: raw }));
    const n = Number(raw);
    if (raw.trim() !== '' && Number.isFinite(n)) {
      setNumErrors((prev) => (prev[key] ? { ...prev, [key]: undefined } : prev));
      const truncated = Math.trunc(n);
      setDraft((d) => (d ? withNum(d, key, truncated) : d));
    }
  }, []);

  // 开关行入参：按 8 个原语派生，数组引用只在开关本身变化时改变——
  // 文本/词表编辑不会让 SwitchRows 命中重渲染（memo 才有意义）。
  const flags = useMemo(
    () =>
      SWITCHES.map((item) => Boolean(draft?.[item.key])),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 逐键列依赖：整体 draft 每次编辑都换引用
    [
      draft?.enabled,
      draft?.system_normalize,
      draft?.dead_zone_rewrite,
      draft?.synonym_enabled,
      draft?.reduce_reasoning,
      draft?.cross_source_fallback,
      draft?.skill_inject,
      draft?.structured_prompt,
    ],
  );

  if (loading) {
    return (
      <div className="purge-console">
        <div className="page-header">
          <h1>{t('破甲控制台')}</h1>
        </div>
        <SkeletonCards count={7} />
        <Card title={t('开关')}>
          <SkeletonList rows={8} />
        </Card>
        <Card title={t('只读参数')}>
          <div className="purge-skeleton-params" role="status" aria-busy="true">
            {Array.from({ length: 8 }, (_, i) => (
              <Skeleton key={i} height={12} />
            ))}
          </div>
        </Card>
      </div>
    );
  }

  if (!data || !draft) {
    return (
      <div className="purge-console">
        <div className="page-header">
          <h1>{t('破甲控制台')}</h1>
        </div>
        <Card>
          <p className="purge-empty">{t('网络异常或服务不可用，请稍后重试')}</p>
          <Button variant="primary" onClick={() => void load()}>
            {t('重试')}
          </Button>
        </Card>
      </div>
    );
  }

  const stats = data.stats;
  const retryRate = stats.responses > 0 ? ((stats.retries / stats.responses) * 100).toFixed(2) : '0.00';
  const uptime = formatUptime(data.runtime.uptime_secs, {
    sec: t('秒'),
    min: t('分钟'),
    hour: t('小时'),
    day: t('天'),
  });

  return (
    <div className="purge-console" aria-busy={saving || undefined}>
      <div className="page-header">
        <h1>{t('破甲控制台')}</h1>
        <div className="purge-meta">
          PID {data.runtime.pid} · {uptime} · {t('上游')}{' '}
          {data.runtime.upstream || '—'}{' '}
          {/* 刷新统计：load 会重置草稿，有未保存编辑时禁用避免吞掉改动 */}
          <Button
            variant="outline"
            size="sm"
            disabled={saving || dirty || loading}
            title={dirty ? t('有未保存的修改，保存或重新加载后才能刷新') : undefined}
            onClick={() => void load()}
          >
            {t('刷新')}
          </Button>
        </div>
      </div>

      {/* 未保存编辑警示条：离开保护之外的页内显性提示，提供「放弃修改」出口 */}
      {dirty && (
        <div className="purge-dirty-bar" role="status">
          <span>{t('有未保存的修改')}</span>
          <Button
            variant="outline"
            size="sm"
            disabled={saving}
            onClick={() => {
              const snapshot = dataRef.current;
              if (!snapshot) return;
              setDraft(snapshot.settings);
              setNumRaw(numRawOf(snapshot.settings));
              setNumErrors({});
              setDirty(false);
              setPreviewOut(null);
              setPreviewMeta(null);
            }}
          >
            {t('放弃修改')}
          </Button>
        </div>
      )}

      <StatsGrid stats={stats} retryRate={retryRate} t={t} />

      <FailureBar
        refusal={stats.failure_refusal}
        empty={stats.failure_empty}
        filter={stats.failure_filter}
        other={stats.failure_other}
        t={t}
      />

      {/* 配置体检告警：开了但不生效 / 配置自相矛盾（静态可判定项） */}
      {configWarnings.length > 0 && (
        <div className="purge-warning-box" role="alert">
          {configWarnings.map((warning, index) => (
            <p key={index}>{warning}</p>
          ))}
        </div>
      )}

      <Card
        title={t('开关')}
        actions={<span className="purge-hint">{t('点一下立即生效，重启后保留')}</span>}
      >
        <SwitchRows flags={flags} disabled={saving} onToggle={toggle} t={t} />
      </Card>

      <Card
        title={t('只读参数')}
        actions={
          <>
            <Button variant="outline" size="sm" onClick={exportConfig}>
              {t('导出配置')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => importInputRef.current?.click()}
            >
              {t('导入配置')}
            </Button>
            <input
              ref={importInputRef}
              type="file"
              accept=".json,application/json"
              style={{ display: 'none' }}
              onChange={(event) => {
                void importConfig(event.target.files?.[0] ?? null);
                // 允许连续导入同名文件
                event.target.value = '';
              }}
            />
          </>
        }
      >
        {/* 只读区一律读服务端快照（data.settings），不读草稿——未保存的编辑
            不应提前出现在「只读」区（与失败分类、在途数等运行时字段同口径）。 */}
        <ParamGrid
          retryLimit={data.settings.retry_limit}
          failureRefusal={stats.failure_refusal}
          failureEmpty={stats.failure_empty}
          failureFilter={stats.failure_filter}
          failureOther={stats.failure_other}
          fallbackModels={data.settings.fallback_models.join(', ')}
          fallbackTriggerAttempt={data.settings.fallback_trigger_attempt}
          skillInjectLimit={data.settings.skill_inject_limit}
          inflight={data.runtime.inflight}
          upstream={data.runtime.upstream}
          t={t}
        />
      </Card>

      <Card
        title={t('死区词改写词表')}
        actions={
          <Button variant="outline" size="sm" onClick={addRule}>
            {t('添加规则')}
          </Button>
        }
      >
        {draft.rewrite_rules.length === 0 ? (
          <p className="purge-empty">{t('暂无改写规则，点击右上角「添加规则」新建一条')}</p>
        ) : (
          <div className="purge-rules" ref={rulesRef}>
            {draft.rewrite_rules.map((rule, index) => (
              <RuleRow
                key={index}
                rule={rule}
                index={index}
                fromLabel={t('原文')}
                toLabel={t('改写为')}
                removeLabel={t('删除')}
                dupHint={
                  dupRules.get(rule.from.trim().toLowerCase())
                    ? t('与其他行的原文重复，保存时后端只保留最早出现的一条')
                    : undefined
                }
                onPatch={patchRule}
                onRemove={removeRule}
              />
            ))}
          </div>
        )}
      </Card>

      <Card
        title={t('同义词表')}
        actions={
          <Button variant="outline" size="sm" onClick={addSynonymRule}>
            {t('添加规则')}
          </Button>
        }
      >
        {draft.synonym_pairs.length === 0 ? (
          <p className="purge-empty">{t('暂无同义词，点击右上角「添加规则」新建一条')}</p>
        ) : (
          <div className="purge-rules" ref={synRulesRef}>
            {draft.synonym_pairs.map((rule, index) => (
              <RuleRow
                key={index}
                rule={rule}
                index={index}
                fromLabel={t('敏感词')}
                toLabel={t('同义词')}
                removeLabel={t('删除')}
                dupHint={
                  dupSynonyms.get(rule.from.trim().toLowerCase())
                    ? t('与其他行的敏感词重复，保存时后端只保留最早出现的一条')
                    : undefined
                }
                onPatch={patchSynonymRule}
                onRemove={removeSynonymRule}
              />
            ))}
          </div>
        )}
      </Card>

      <Card title={t('技能与结构化提示')}>
        <div className="purge-form-grid">
          <Input
            label={t('技能目录')}
            hint={t('扫描该目录下的 SKILL.md，按 frontmatter 关键词命中')}
            value={draft.skill_dir}
            onChange={(event) => patch({ skill_dir: event.target.value })}
          />
          <Input
            label={t('技能注入上限（字符）')}
            type="number"
            min={16}
            max={20000}
            value={numRaw.skill_inject_limit}
            error={numErrors.skill_inject_limit}
            onChange={(event) => handleNumChange('skill_inject_limit', event.target.value)}
          />
          <Input
            label={t('异源模型（逗号分隔）')}
            value={draft.fallback_models.join(', ')}
            onChange={(event) => patch({ fallback_models: splitList(event.target.value) })}
          />
          <Input
            label={t('异源触发轮次')}
            type="number"
            min={1}
            max={32}
            value={numRaw.fallback_trigger_attempt}
            error={numErrors.fallback_trigger_attempt}
            onChange={(event) => handleNumChange('fallback_trigger_attempt', event.target.value)}
          />
          <Input
            label={t('重试上限')}
            type="number"
            min={1}
            max={32}
            value={numRaw.retry_limit}
            error={numErrors.retry_limit}
            onChange={(event) => handleNumChange('retry_limit', event.target.value)}
          />
          <Input
            label={t('system 剥离前缀（逗号分隔）')}
            value={draft.normalize_strip_prefixes.join(', ')}
            onChange={(event) => patch({ normalize_strip_prefixes: splitList(event.target.value) })}
          />
          <Select
            label={t('降档目标（reasoning_floor）')}
            hint={t('降思考开启后，把 high/medium/xhigh/max 压到该档位')}
            value={draft.reasoning_floor}
            onChange={(event) => patch({ reasoning_floor: event.target.value })}
          >
            {REASONING_FLOORS.map((item) => (
              <option key={item.value} value={item.value}>
                {t(item.label)}
              </option>
            ))}
          </Select>
        </div>
        <Textarea
          label={t('拒绝话术词表（每行一条，空表 = 关闭字面拒绝检测）')}
          rows={4}
          value={(draft.refusal_patterns ?? []).join('\n')}
          onChange={(event) => patch({ refusal_patterns: splitLines(event.target.value) })}
        />
        <div className="purge-form-grid">
          <Textarea
            label={t('「内容过滤」类失败关键词（每行一条）')}
            rows={3}
            value={(draft.failure_filter_keywords ?? []).join('\n')}
            onChange={(event) => patch({ failure_filter_keywords: splitLines(event.target.value) })}
          />
          <Textarea
            label={t('「空答」类失败关键词（每行一条）')}
            rows={3}
            value={(draft.failure_empty_keywords ?? []).join('\n')}
            onChange={(event) => patch({ failure_empty_keywords: splitLines(event.target.value) })}
          />
        </div>
        <Textarea
          label={t('结构化输出后缀')}
          rows={4}
          value={draft.structured_suffix}
          onChange={(event) => patch({ structured_suffix: event.target.value })}
        />
      </Card>

      {/* 技能包导入：ZIP/TAR/TGZ 安全解压到技能目录（包内须含 SKILL.md） */}
      <Card title={t('导入技能包')}>
        <p style={{ margin: '0 0 10px', fontSize: 13 }}>
          {t(
            '上传 ZIP / TAR / TGZ 技能包，安全解压到上面的「技能目录」。包内须含 SKILL.md；导入后会立即出现在技能快照。',
          )}
        </p>
        <div style={{ display: 'flex', gap: 12, alignItems: 'flex-end', flexWrap: 'wrap' }}>
          <div style={{ flex: '1 1 220px', minWidth: 220 }}>
            <Input
              label={t('目录名（可选）')}
              hint={t('留空则按文件名推导，仅允许字母/数字/连字符')}
              value={skillPkgName}
              placeholder={t('例如 my-skill-pack')}
              onChange={(event) => setSkillPkgName(event.target.value)}
            />
          </div>
          <label style={{ display: 'flex', alignItems: 'center', gap: 6, whiteSpace: 'nowrap' }}>
            <input
              type="checkbox"
              checked={skillPkgOverwrite}
              onChange={(event) => setSkillPkgOverwrite(event.target.checked)}
            />
            {t('覆盖同名目录')}
          </label>
          <Button disabled={skillPkgImporting} onClick={() => skillPkgInputRef.current?.click()}>
            {skillPkgImporting ? t('导入中…') : t('选择文件并导入')}
          </Button>
          <input
            ref={skillPkgInputRef}
            type="file"
            accept=".zip,.tar,.tgz,.tar.gz"
            hidden
            onChange={(event) => void handleImportSkill(event)}
          />
        </div>
      </Card>

      {/* 技能扫描快照：让 skill_inject 配置可验证（目录里到底扫到了什么） */}
      {skills.length > 0 && (
        <Card title={t('技能扫描快照')}>
          <ul className="purge-skill-list">
            {skills.map((skill) => (
              <li key={skill.name}>
                <strong>{skill.name}</strong>
                <span className="purge-skill-meta">
                  {t('{{count}} 个关键词 · 正文 {{chars}} 字符', {
                    count: skill.keywords.length,
                    chars: skill.body_chars,
                  })}
                </span>
                <span className="purge-skill-keywords">{skill.keywords.slice(0, 6).join(' / ')}</span>
              </li>
            ))}
          </ul>
        </Card>
      )}

      {/* 预设库：内置 prompt 集合浏览 + 一键落盘（放在结构化模板之上，应用后直接在下方看到结果） */}
      <PresetsSection
        t={t}
        addToast={addToast}
        onApplyTemplate={applyPresetTemplate}
        onSkillWritten={() => void refreshSkills()}
      />

      <Card
        title={t('结构化模板')}
        actions={
          <Button variant="outline" size="sm" onClick={addTemplate}>
            {t('添加模板')}
          </Button>
        }
      >
        <Select
          label={t('当前启用模板')}
          hint={t('结构化提示开启时，命中模板则用其文本替代默认后缀；选「使用默认后缀」即回退')}
          value={draft.active_template ?? ''}
          onChange={(event) => patch({ active_template: event.target.value })}
        >
          <option value="">{t('（使用默认后缀）')}</option>
          {(draft.structured_templates ?? []).map((tpl, index) => (
            <option key={`tpl-${index}`} value={tpl.name} disabled={!tpl.name.trim()}>
              {tpl.name.trim() || t('（未命名）')}
            </option>
          ))}
        </Select>
        {(draft.structured_templates ?? []).length === 0 ? (
          <p className="purge-empty">{t('暂无模板，点击右上角「添加模板」新建一条')}</p>
        ) : (
          <div className="purge-rules">
            {(draft.structured_templates ?? []).map((tpl, index) => (
              <div className="purge-rule-row" key={`tpl-${index}`}>
                <div>
                  <Input
                    placeholder={t('模板名称')}
                    aria-label={t('模板名称')}
                    value={tpl.name}
                    error={
                      dupTemplates.get(tpl.name.trim().toLowerCase())
                        ? t('模板名称重复，保存前请先修改')
                        : undefined
                    }
                    onChange={(event) => patchTemplate(index, { name: event.target.value })}
                  />
                </div>
                <Textarea
                  rows={3}
                  placeholder={t('模板文本')}
                  aria-label={t('模板文本')}
                  value={tpl.text}
                  onChange={(event) => patchTemplate(index, { text: event.target.value })}
                />
                <Button variant="outline" size="sm" onClick={() => removeTemplate(index)}>
                  {t('删除')}
                </Button>
              </div>
            ))}
          </div>
        )}
      </Card>

      <Card title={t('改写预览')}>
        <Textarea
          label={t('样例输入')}
          rows={3}
          placeholder={t('粘贴一段 user 消息，按当前草稿配置试跑改写效果（dry-run，不发送请求、不写统计）')}
          value={previewInput}
          onChange={(event) => setPreviewInput(event.target.value)}
        />
        <div className="purge-actions">
          {/* 预览用草稿里的未保存配置：保存前即可验证词表/模板/技能注入效果 */}
          <Button variant="outline" disabled={previewing || saving} onClick={() => void runPreview()}>
            {previewing ? t('预览中…') : t('预览改写')}
          </Button>
        </div>
        {previewOut !== null ? (
          <>
            {previewMeta ? (
              <div className="purge-preview-meta">
                {previewMeta.changed ? t('已应用改写') : t('无改写命中（原样返回）')}
                {previewMeta.skill_hit ? ` · ${t('技能已注入')}` : ''}
              </div>
            ) : null}
            <pre className="purge-preview-out" aria-label={t('改写结果')}>
              {previewOut}
            </pre>
          </>
        ) : null}
      </Card>

      <div className="purge-actions">
        {/* 保存态播报：按钮文案变化对读屏不可见，用 live region 显式播报 */}
        <span className="sr-only" role="status" aria-live="polite">
          {saving ? t('保存中…') : ''}
        </span>
        <Button
          variant="primary"
          disabled={saving || !dirty}
          title={!dirty && !saving ? t('没有需要保存的修改') : undefined}
          onClick={() => void save()}
        >
          {saving ? t('保存中…') : t('保存')}
        </Button>
      </div>
    </div>
  );
}
