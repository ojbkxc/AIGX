import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api } from '../api';
import type { PurgeData, PurgeRewriteRule, PurgeSettings, PurgeStats } from '../types';
import {
  Button,
  Card,
  Input,
  Skeleton,
  SkeletonCards,
  SkeletonList,
  StatCard,
  Switch,
  Textarea,
} from '../components/ui';
import { useToast } from '../components/Toast';
import './PurgeConsole.css';

type BoolKey =
  | 'enabled'
  | 'system_normalize'
  | 'dead_zone_rewrite'
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
  onPatch,
  onRemove,
}: {
  rule: PurgeRewriteRule;
  index: number;
  fromLabel: string;
  toLabel: string;
  removeLabel: string;
  onPatch: (index: number, part: Partial<PurgeRewriteRule>) => void;
  onRemove: (index: number) => void;
}): JSX.Element {
  return (
    <div className="purge-rule-row">
      <Input
        placeholder={fromLabel}
        aria-label={fromLabel}
        value={rule.from}
        onChange={(event) => onPatch(index, { from: event.target.value })}
      />
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
    </div>
  );
});

/**
 * 开关行（memo）：`flags` 由父组件 useMemo 按 7 个原语派生，数组引用只在开关本身变化时变，
 * 故编辑文本/词表不会连带重渲染这 7 行。
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
        <span>{t('synonym 模式')}</span>
        <strong>{t('未启用')}</strong>
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

  const load = useCallback(async (): Promise<void> => {
    setLoading(true);
    try {
      const res = await api.getPurge();
      if (!res.success || !res.data) throw new Error('load failed');
      setData(res.data);
      setDraft(res.data.settings);
      setNumRaw(numRawOf(res.data.settings));
      setNumErrors({});
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
  }, [addToast, numRaw, t]);

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

  // 开关行入参：按 7 个原语派生，数组引用只在开关本身变化时改变——
  // 文本/词表编辑不会让 SwitchRows 命中重渲染（memo 才有意义）。
  const flags = useMemo(
    () =>
      SWITCHES.map((item) => Boolean(draft?.[item.key])),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- 逐键列依赖：整体 draft 每次编辑都换引用
    [
      draft?.enabled,
      draft?.system_normalize,
      draft?.dead_zone_rewrite,
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
        <SkeletonCards count={6} />
        <Card title={t('开关')}>
          <SkeletonList rows={7} />
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
          {data.runtime.upstream || '—'}
        </div>
      </div>

      <StatsGrid stats={stats} retryRate={retryRate} t={t} />

      <Card
        title={t('开关')}
        actions={<span className="purge-hint">{t('点一下立即生效，重启后保留')}</span>}
      >
        <SwitchRows flags={flags} disabled={saving} onToggle={toggle} t={t} />
      </Card>

      <Card title={t('只读参数')}>
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
                onPatch={patchRule}
                onRemove={removeRule}
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
        </div>
        <Textarea
          label={t('结构化输出后缀')}
          rows={4}
          value={draft.structured_suffix}
          onChange={(event) => patch({ structured_suffix: event.target.value })}
        />
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
