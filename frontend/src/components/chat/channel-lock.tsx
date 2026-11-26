/**
 * AI 运维 · 渠道锁定（AIGX 独有面：`[agent].channel` 非空时只走该渠道）。
 *
 * 现状（`pages/Agent.tsx`）：配置里 `channel` 非空 → 拉渠道列表取名字与模型，
 * `ModelPicker` 用 `lockToChannel={!!(agentChannel && channelModels.length > 0)}`
 * 锁定；拉列表失败时 `.catch(() => {})` **静默降级**。
 *
 * 两个后果（本模块修的就是这两个）：
 * 1. **锁定状态说不清**：拉列表失败 / 渠道没有模型列表时，徽标仍显示「已锁定」，
 *    但选择器实际解锁——用户能选任意模型，发出去必被后端拒（后端只走锁定渠道）。
 *    徽标与选择器互相矛盾，且没有任何提示。
 * 2. **来源不可见**：徽标文案取渠道名，拿不到名字时退回渠道 id，用户不知道
 *    这是「渠道名」还是「渠道 id」，也不知道列表为什么没加载出来。
 *
 * 语义口径（与后端一致，改动前已核对 `src/agent/api.rs` 的 config 下发）：
 * - `channel` 非空 = 后端**强制**该渠道，这是硬事实，不因前端拉列表失败而改变；
 * - 模型列表只是「选择器的可选项来源」，拿不到就只能不锁选择器，但必须**说出来**。
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Lock } from 'lucide-react';
import { api } from '../../api';

export interface ChannelInfoLike {
  id: string | number;
  name?: string;
  models?: string[];
  discovered_models?: string[];
}

export interface ChannelLockState {
  /** 后端是否锁定了渠道（配置非空即为 true，与列表加载成败无关） */
  locked: boolean;
  /** 渠道 id（配置原值） */
  channelId: string;
  /** 展示用名称：渠道名 → 渠道 id（后者要标出来，别让用户以为是名字） */
  label: string;
  /** 名称是否来自渠道列表（false = 回落成 id） */
  nameResolved: boolean;
  /** 该渠道可用模型（去重保序；空 = 未知或无限制） */
  models: string[];
  /** 选择器是否可以锁到该渠道模型列表（只有拿到非空列表才行） */
  lockPicker: boolean;
  /**
   * 降级原因：'no-list'（没拉到渠道列表）/ 'no-models'（渠道无模型列表）/
   * null（正常）。非 null 时界面必须显式提示，不能静默。
   */
  degraded: 'no-list' | 'no-models' | null;
}

const EMPTY_LOCK: ChannelLockState = {
  locked: false,
  channelId: '',
  label: '',
  nameResolved: false,
  models: [],
  lockPicker: false,
  degraded: null,
};

/**
 * 计算渠道锁定的展示与锁定状态（纯函数）。
 *
 * @param channel 配置里的 `[agent].channel`（空/空白 = 不锁定）
 * @param channels 渠道列表；`null` 表示拉取失败（与「拉到了但没找到该渠道」区分）
 */
export function resolveChannelLock(
  channel: string | undefined | null,
  channels: ChannelInfoLike[] | null,
): ChannelLockState {
  const channelId = (channel ?? '').trim();
  if (!channelId) return EMPTY_LOCK;

  if (channels === null) {
    // 列表拉失败：后端仍然锁渠道（硬事实），但名字与模型都不知道
    return {
      locked: true,
      channelId,
      label: channelId,
      nameResolved: false,
      models: [],
      lockPicker: false,
      degraded: 'no-list',
    };
  }

  const found = channels.find((c) => String(c.id) === channelId);
  if (!found) {
    // 拉到了列表但没这个渠道（已删除/被禁用/无权限）：同样锁着，但不可锁选择器
    return {
      locked: true,
      channelId,
      label: channelId,
      nameResolved: false,
      models: [],
      lockPicker: false,
      degraded: 'no-list',
    };
  }

  const models = dedupe([...(found.models ?? []), ...(found.discovered_models ?? [])]);
  const name = (found.name ?? '').trim();
  return {
    locked: true,
    channelId,
    label: name || channelId,
    nameResolved: Boolean(name),
    models,
    lockPicker: models.length > 0,
    degraded: models.length > 0 ? null : 'no-models',
  };
}

/** 去重保序（渠道 models 与 discovered_models 常有重叠） */
function dedupe(list: string[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const v of list) {
    const s = String(v ?? '').trim();
    if (!s || seen.has(s)) continue;
    seen.add(s);
    out.push(s);
  }
  return out;
}

/**
 * 徽标文案：正常只说渠道名；降级时把原因写进 title，不静默。
 *
 * 返回 `{ text, titleKey }`，由组件套 i18n（本函数保持纯，便于单测）。
 * `titleKey` 是**中文原文**，组件里必须用字面量 t 调用分支取用（不能把
 * `badge.titleKey` 当参数传进去）：i18n 扫描器只认字面量，动态 key 会让英文
 * 界面回落中文。
 */
export function channelLockBadge(lock: ChannelLockState): { text: string; titleKey: string } | null {
  if (!lock.locked) return null;
  const text = lock.nameResolved ? lock.label : `${lock.channelId}`;
  if (lock.degraded === 'no-list') {
    return { text, titleKey: '已锁定该供应商（渠道列表加载失败，模型列表未知）' };
  }
  if (lock.degraded === 'no-models') {
    return { text, titleKey: '已锁定该供应商（该渠道未配置模型列表，选择器不锁定）' };
  }
  return { text, titleKey: 'Agent 已锁定供应商，只走该供应商的请求' };
}

/**
 * 徽标（视觉沿用 `Agent.css` 既有 `.agent-channel-lock`）。
 *
 * 降级时（`no-list` / `no-models`）徽标加 `.degraded` 修饰并在 title 里说明原因：
 * 原实现拉列表失败是 `.catch(() => {})`，界面上与「一切正常」完全一致。
 */
export function ChannelLockBadge({ lock }: { lock: ChannelLockState }): JSX.Element | null {
  const { t } = useTranslation();
  const badge = channelLockBadge(lock);
  if (!badge) return null;
  // 字面量 t()：i18n 扫描器只认字面量 key，动态 t(badge.titleKey) 扫不出来
  const title = lock.degraded === 'no-list'
    ? t('已锁定该供应商（渠道列表加载失败，模型列表未知）')
    : lock.degraded === 'no-models'
      ? t('已锁定该供应商（该渠道未配置模型列表，选择器不锁定）')
      : `${t('Agent 已锁定供应商，只走该供应商的请求')}${t('（锁定期间不会自动切换模型）')}`;
  return (
    <span
      className={`agent-channel-lock${lock.degraded ? ' degraded' : ''}`}
      title={title}
    >
      <Lock size={11} />
      {badge.text}
    </span>
  );
}

/** 渠道列表项的最小形状（只用到这三个字段） */
interface ChannelListLike {
  id: string | number;
  name?: string;
  models?: string[];
  discovered_models?: string[];
}

/**
 * 取渠道列表并算出锁定状态（挂载方用这一个 hook）。
 *
 * 与旧实现的差别只有一处，但正是根因：**拉取失败不是「没锁定」，而是「锁定但
 * 未知模型」**。旧代码 `.catch(() => {})` 让两种情形在界面上无法区分（都只显示
 * 徽标、选择器解锁）。这里失败时显式给 `degraded: 'no-list'`，徽标据此提示。
 *
 * @param channel `[agent].channel` 配置值（空 = 不锁定，不发请求）
 */
export function useChannelLock(channel: string | undefined | null): ChannelLockState {
  const [lock, setLock] = useState<ChannelLockState>(() => resolveChannelLock(channel, []));
  const channelId = (channel ?? '').trim();

  useEffect(() => {
    if (!channelId) {
      setLock(resolveChannelLock('', []));
      return undefined;
    }
    let alive = true;
    // 先落「已锁定但列表未知」：请求在飞的这段时间也不该显示成「未锁定」
    setLock(resolveChannelLock(channelId, null));
    api.listChannels()
      .then((res) => {
        if (!alive) return;
        const list = (res as unknown as { data?: ChannelListLike[] }).data;
        setLock(resolveChannelLock(channelId, Array.isArray(list) ? list : []));
      })
      .catch(() => {
        if (!alive) return;
        // 失败不静默：保留 degraded 标记，界面据此提示「模型列表未知」
        setLock(resolveChannelLock(channelId, null));
      });
    return () => { alive = false; };
  }, [channelId]);

  return lock;
}
