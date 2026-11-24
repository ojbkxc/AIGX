/**
 * 定时条目的编辑器（对齐 Agent2API `checkin-schedule-editor.tsx` 的字段面）。
 *
 * ── 为什么是「一条条目的完整配置」而不是「只改时刻」 ────────
 * 一条条目就是一次「区间签到」的完整配置：时刻 / 区间结束 / 逐家覆盖 / 重试 /
 * 补签。编辑别家字段不该把没动的字段清掉（后端 `schedules::upsert` 对未传字段
 * 继承存量），所以这里**始终提交全量 5 字段**，不做差分提交。
 *
 * ── 1.0.49 契约字段已冻结 ───────────────────────────────────
 * `time_end` / `randomize` / `retry_limit` / `retry_gap_minutes` / `makeup_time`
 * 一律提交；旧后端有 `serde(default)` 会忽略未知字段，所以不做 feature-detect
 * （否则「零条目」时新建表单会缺控件）。
 */

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../api';
import Switch from '../components/ui/Switch';
import Tooltip from '../components/ui/Tooltip';
import type { CheckinScheduleEntry } from '../types';

/** 时刻格式（`HH:MM` 24 小时制） */
export const HHMM_RE = /^([01]\d|2[0-3]):[0-5]\d$/;

export interface ScheduleForm {
  id: string;
  label: string;
  enabled: boolean;
  time: string;
  providers: string[];
  /** 逐家时刻覆盖（仅非空项提交；空 = 回落内置专属/条目时刻） */
  providerTimes: Record<string, string>;
  /** 随机签到区间结束 HH:MM（空 = 无窗口，仅按 time 固定执行） */
  timeEnd: string;
  /** true = 每个账号在 [time, timeEnd] 内取确定性随机时刻 */
  randomize: boolean;
  /** 每账号每天重试上限（0 = 关闭重试；缺省 3，与后端 default_retry_limit 一致） */
  retryLimit: string;
  /** 重试间隔分钟（<=0 回落全局 retry_gap_ms；缺省 5） */
  retryGapMinutes: string;
  /** 补签轮时刻 HH:MM（空 = 不跑补签轮） */
  makeupTime: string;
}

export const EMPTY_SCHEDULE: ScheduleForm = {
  id: '',
  label: '每日签到',
  enabled: true,
  time: '09:00',
  providers: [],
  providerTimes: {},
  timeEnd: '',
  randomize: false,
  retryLimit: '3',
  retryGapMinutes: '5',
  makeupTime: '',
};

/** 条目 → 表单（时间字段一律字符串，避免受控组件 undefined 告警） */
export function formOf(schedule: CheckinScheduleEntry): ScheduleForm {
  return {
    id: schedule.id,
    label: schedule.label,
    enabled: schedule.enabled,
    time: schedule.time,
    providers: schedule.providers,
    providerTimes: { ...(schedule.providerTimes ?? {}) },
    timeEnd: schedule.timeEnd ?? '',
    randomize: schedule.randomize ?? false,
    retryLimit: String(schedule.retryLimit ?? 3),
    retryGapMinutes: String(schedule.retryGapMinutes ?? 5),
    makeupTime: schedule.retryTime ?? '',
  };
}

/** 生效时刻三档回落：逐家覆盖 > 内置专属 > 条目时刻（对齐上游 `effectiveScheduleTime`） */
export function effectiveTime(
  provider: string,
  form: ScheduleForm,
  builtinTimes: Record<string, string>,
): string {
  return form.providerTimes[provider] || builtinTimes[provider] || form.time;
}

export default function CheckinScheduleEditor({
  form,
  onChange,
  onClose,
  onSaved,
  onError,
  pickOptions,
  builtinTimes,
  unsupported,
  saving,
}: {
  form: ScheduleForm;
  onChange: (next: ScheduleForm) => void;
  onClose: () => void;
  onSaved: () => void;
  onError: (message: string) => void;
  pickOptions: Array<{ id: string; label: string; builtinTime?: string | null }>;
  builtinTimes: Record<string, string>;
  /** 主仓未接入实现的家（只做说明，不出现在可选范围） */
  unsupported: string[];
  saving: boolean;
}): JSX.Element {
  const { t } = useTranslation();
  const [showTips, setShowTips] = useState(false);

  const set = (patch: Partial<ScheduleForm>): void => onChange({ ...form, ...patch });

  const save = async (): Promise<void> => {
    // 逐家覆盖校验：填了就必须是合法 HH:MM（空串 = 撤销覆盖）
    for (const [provider, value] of Object.entries(form.providerTimes)) {
      if (value && !HHMM_RE.test(value)) {
        onError(t('{{provider}} 的覆盖时刻格式应为 HH:MM', { provider }));
        return;
      }
    }
    if (form.timeEnd && !HHMM_RE.test(form.timeEnd)) {
      onError(t('区间结束时刻格式应为 HH:MM'));
      return;
    }
    if (form.makeupTime && !HHMM_RE.test(form.makeupTime)) {
      onError(t('补签时刻格式应为 HH:MM'));
      return;
    }
    if (form.randomize && (!form.timeEnd || form.timeEnd <= form.time)) {
      onError(t('开启区间随机时，结束时刻须晚于开始时刻'));
      return;
    }
    const providerTimes = Object.fromEntries(
      Object.entries(form.providerTimes).filter(([, value]) => value),
    );
    try {
      await api.upsertCheckinSchedule({
        id: form.id,
        label: form.label,
        enabled: form.enabled,
        time: form.time,
        providers: form.providers,
        provider_times: providerTimes,
        time_end: form.timeEnd,
        randomize: form.randomize,
        retry_limit: Number(form.retryLimit) || 0,
        retry_gap_minutes: Number(form.retryGapMinutes) || 0,
        makeup_time: form.makeupTime,
      });
      onSaved();
    } catch (err) {
      onError(err instanceof Error ? err.message : String(err));
    }
  };

  /** 实际签到安排预览：按生效时刻分组（对齐上游编辑器右侧栏） */
  const preview = (() => {
    if (form.providers.length === 0) return [] as Array<{ time: string; list: string[] }>;
    const groups = new Map<string, string[]>();
    for (const provider of form.providers) {
      const at = effectiveTime(provider, form, builtinTimes);
      groups.set(at, [...(groups.get(at) ?? []), provider]);
    }
    return [...groups.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([time, list]) => ({ time, list }));
  })();

  return (
    <div className="ck-editor">
      <div className="ck-editor-grid">
        <label className="form-group">
          <span className="ck-editor-label">{t('名称')}</span>
          <input
            className="form-input"
            value={form.label}
            onChange={(e) => set({ label: e.target.value })}
          />
        </label>
        <label className="form-group">
          <span className="ck-editor-label">{t('每天时刻（按下方时区·错过自动补签）')}</span>
          <input
            type="time"
            className="form-input ck-time-input"
            value={form.time}
            onChange={(e) => set({ time: e.target.value })}
          />
        </label>
        <label className="ck-editor-enabled">
          <Switch
            checked={form.enabled}
            onChange={(next) => set({ enabled: next })}
            ariaLabel={t('启用')}
          />
          <span>{t('启用')}</span>
        </label>
      </div>

      <div className="form-group">
        <div className="ck-scope-head">
          <span className="ck-editor-label">
            {t('签到范围（可逐家覆盖时刻）')}
            <button
              type="button"
              className="ck-tip-dot"
              aria-label={t('说明')}
              onClick={() => setShowTips((open) => !open)}
            >
              ?
            </button>
          </span>
          <span className="ck-scope-tools">
            <button
              type="button"
              onClick={() => set({ providers: pickOptions.map((option) => option.id) })}
            >
              {t('全选')}
            </button>
            <button type="button" onClick={() => set({ providers: [] })}>
              {t('清空')}
            </button>
          </span>
        </div>
        {showTips && (
          <span className="form-hint">
            {t('逐家覆盖或内置专属时刻优先生效；勾掉某家会同时清掉它的时刻覆盖。留空范围 = 全部可用上游。')}
          </span>
        )}
        <div className="ck-provider-picks">
          {pickOptions.map((option) => {
            const provider = option.id;
            const checked = form.providers.includes(provider);
            const override = form.providerTimes[provider] ?? '';
            const eff = effectiveTime(provider, form, builtinTimes);
            const isBuiltin = !override && Boolean(builtinTimes[provider] || option.builtinTime);
            return (
              <div
                key={provider}
                className={`ck-provider-pick ck-provider-pick-row${checked ? ' is-checked' : ''}`}
              >
                <label className="ck-provider-pick-name">
                  <input
                    type="checkbox"
                    checked={checked}
                    onChange={(e) => {
                      const next = e.target.checked
                        ? [...form.providers, provider]
                        : form.providers.filter((item) => item !== provider);
                      const times = { ...form.providerTimes };
                      if (!e.target.checked) delete times[provider];
                      onChange({ ...form, providers: next, providerTimes: times });
                    }}
                  />
                  {option.label || provider}
                  {isBuiltin && <span className="ck-pick-tag">{t('平台专属')}</span>}
                </label>
                {checked && (
                  <span className="ck-pick-time">
                    <input
                      type="time"
                      className="form-input ck-time-input"
                      aria-label={t('{{provider}} 签到时刻', { provider: option.label || provider })}
                      value={override || eff}
                      onChange={(e) =>
                        onChange({
                          ...form,
                          providerTimes: { ...form.providerTimes, [provider]: e.target.value },
                        })
                      }
                    />
                    {override ? (
                      <button
                        type="button"
                        className="ck-pick-reset"
                        onClick={() => {
                          const times = { ...form.providerTimes };
                          delete times[provider];
                          onChange({ ...form, providerTimes: times });
                        }}
                      >
                        {t('恢复默认')}
                      </button>
                    ) : (
                      <span className="ck-pick-tag ck-pick-tag-mute">
                        {isBuiltin ? t('内置时刻') : t('跟随默认')}
                      </span>
                    )}
                  </span>
                )}
              </div>
            );
          })}
        </div>
        {form.providers.length === 0 && (
          <span className="form-hint">{t('不选 = 全部可用上游')}</span>
        )}
        {unsupported.length > 0 && (
          <span className="form-hint">
            {t('以下上游的实现随上游进入插件仓、主仓未接入，不出现在可选范围：')}
            {unsupported.join('、')}
          </span>
        )}
      </div>

      {/* 实际签到安排预览（对齐上游编辑器右侧栏）：按生效时刻分组 */}
      {preview.length > 0 && (
        <div className="form-group">
          <span className="ck-editor-label">{t('实际签到安排（按生效时刻分组）')}</span>
          <div className="ck-preview-groups">
            {preview.map((group) => (
              <div key={group.time} className="ck-preview-group">
                <code>{group.time}</code>
                <span>{group.list.join('、')}</span>
                <span className="badge badge-neutral">{t('{{n}} 家', { n: group.list.length })}</span>
              </div>
            ))}
          </div>
          <span className="form-hint">
            {t('逐家覆盖或内置专属时刻优先生效；未覆盖的按条目时刻执行。')}
          </span>
        </div>
      )}

      {/* 随机签到区间：time_end + randomize */}
      <div className="form-group">
        <span className="ck-editor-label">{t('随机签到区间（可选）')}</span>
        <div className="ck-editor-grid">
          <label className="form-group">
            <span className="ck-editor-label ck-label-with-tip">
              {t('区间结束时刻')}
              <Tooltip
                label={t(
                  '窗口结束时刻（HH:MM）。开启随机后，每个账号在 [开始, 结束] 内取确定性随机时刻（同账号同日同刻、重启不漂）；留空 = 仅按开始时刻执行。内置专属时刻（如 qoder 10:01）与逐家覆盖的家不参与随机。',
                )}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>
                  ?
                </span>
              </Tooltip>
            </span>
            <input
              type="time"
              className="form-input ck-time-input"
              aria-label={t('区间结束时刻')}
              value={form.timeEnd}
              onChange={(e) => set({ timeEnd: e.target.value })}
            />
          </label>
          <label className="ck-editor-enabled ck-random-toggle">
            <Switch
              checked={form.randomize}
              onChange={(next) => set({ randomize: next })}
              ariaLabel={t('区间内随机')}
            />
            <span className="ck-label-with-tip">
              {t('区间内随机')}
              <Tooltip
                label={t(
                  '开启后每个账号在 [开始, 结束] 区间内随机取一个时刻执行，避免同刻集中触发上游风控；关闭 = 所有账号按开始时刻执行。',
                )}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>
                  ?
                </span>
              </Tooltip>
            </span>
          </label>
        </div>
        <span className="form-hint">
          {t('开启随机时结束时刻必须晚于开始时刻；留空结束时刻 = 固定时刻执行。')}
        </span>
      </div>

      {/* 逐条目重试 + 补签轮：retry_limit / retry_gap_minutes / makeup_time */}
      <div className="form-group ck-retry-block">
        <span className="ck-editor-label">{t('补签与重试')}</span>
        <div className="ck-editor-grid">
          <label className="form-group">
            <span className="ck-editor-label ck-label-with-tip">
              {t('每账号每天最多重试（次）')}
              <Tooltip
                label={t(
                  '当天未签上时最多重试几次；0 = 关闭本条目重试，仅靠补签轮兜底。次数按账号每天单独计算、次日重置。',
                )}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>
                  ?
                </span>
              </Tooltip>
            </span>
            <input
              type="number"
              min={0}
              max={20}
              className="form-input"
              value={form.retryLimit}
              onChange={(e) => set({ retryLimit: e.target.value })}
            />
          </label>
          <label className="form-group">
            <span className="ck-editor-label ck-label-with-tip">
              {t('重试间隔（分钟）')}
              <Tooltip
                label={t('每次失败重试之间等待多少分钟；填 0 = 回落全局「失败/窗口重试间隔」设置。')}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>
                  ?
                </span>
              </Tooltip>
            </span>
            <input
              type="number"
              min={0}
              max={120}
              className="form-input"
              value={form.retryGapMinutes}
              onChange={(e) => set({ retryGapMinutes: e.target.value })}
            />
          </label>
          <label className="form-group">
            <span className="ck-editor-label ck-label-with-tip">
              {t('补签时刻（留空 = 关闭）')}
              <Tooltip
                label={t(
                  '到该时刻挑当天本条目下仍未签上的账号再跑一次（类型记为「补签」），每天至多一轮；留空 = 不跑补签轮。',
                )}
              >
                <span className="ck-tip-dot" aria-label={t('说明')}>
                  ?
                </span>
              </Tooltip>
            </span>
            <input
              type="time"
              className="form-input ck-time-input"
              aria-label={t('补签时刻')}
              value={form.makeupTime}
              onChange={(e) => set({ makeupTime: e.target.value })}
            />
          </label>
        </div>
        <span className="form-hint">
          {t('重试与补签只处理当天未签上的账号；补签轮在指定时刻兜底一次。')}
        </span>
      </div>

      <div className="ck-form-actions">
        <button type="button" className="btn btn-outline btn-sm" onClick={onClose}>
          {t('取消')}
        </button>
        <button
          type="button"
          className="btn btn-primary btn-sm"
          onClick={() => void save()}
          disabled={saving}
        >
          {saving ? t('保存中...') : t('保存')}
        </button>
      </div>
    </div>
  );
}
