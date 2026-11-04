/**
 * ZCode 验证码令牌池守卫（前端铸造器）。
 *
 * 来源：Agent2API `desktop-tauri/ui/zcode-captcha-pool.js`。
 *
 * ── 它解决的是什么 ────────────────────────────────────────────
 * ZCode 的「活动套餐」额度只从活动套餐端点花得出去，而那个端点**每条请求都要一个
 * 当次铸的阿里云验证码令牌**（少它一律 `400 {"code":3007}`）。Rust 侧铸不出来
 * （要跑阿里云 SDK），所以由这个跑在浏览器里的守卫铸：静默无痕验证 → 推给网关 →
 * 转发层按请求取一个。
 *
 * ── 节奏（不铸多、不空窗）─────────────────────────────────────
 * 每轮问网关 `GET /api/zcode/captcha`（间隔与单轮产量都是网关下发的设置）：
 * - `needsTokens: true`（有账号走这条路，且库存低于目标）→ 补到目标；
 * - `needsTokens: false` → 什么都不做（**没有账号用这条路时一个都不铸** ——
 *   阿里云对铸造频率有风控，白铸纯属烧配额）；
 * - 上游刚回过 3007（`rejected` 计数变大）→ 立刻补一轮，不等下一个周期。
 * 令牌寿命两分钟，每次只铸到目标（默认 3 个）：「够下一条请求立刻有得用」即可。
 *
 * ── 为什么不做成用户可见的按钮 ───────────────────────────────
 * 转发是后台发生的，没有人会在每条请求前点一次验证。这与「领套餐」的滑块不是
 * 一回事：那个是**用户动作**，必须让用户看见、确认；这个是**机器对机器的门禁
 * 令牌**，无痕模式本来就是为这种场景设计的。
 *
 * ── 失败时怎么办（如实，不装死）───────────────────────────────
 * 铸造失败只写控制台并退让到下一个周期：池子空了，转发层会如实回一句「活动套餐
 * 通道需要人机验证令牌…」，用户知道去哪儿看。**不弹 toast** —— 它是后台循环，
 * 弹一次就会有第二次、第三次。
 */

import api from '@/api';
import type { ZcodeCaptchaConfig, ZcodeCaptchaPoolStats } from '@/types';
import { invalidateMintInstance, isBusy, mintTraceless } from './aliyunCaptcha';

/** 轮询间隔的**回退默认**（毫秒）：网关没给 `pollMs` 时才用 */
const POLL_MS = 2_500;
/** 单轮铸造上限的**回退默认**：网关没给 `maxPerRound` 时才用 */
const MAX_PER_ROUND = 1;
/** 节奏参数的合法范围（与后端 `config::ZCODE_POOL_*` 常量同源） */
const POLL_MIN_MS = 1_000;
const POLL_MAX_MS = 30_000;
const PER_ROUND_MIN = 1;
const PER_ROUND_MAX = 5;
/** 失败后的退让（毫秒） */
const BACKOFF_MIN_MS = 3_000;
const BACKOFF_MAX_MS = 15_000;
/** 风控配置的缓存：上游那份一天也不会变几次，每轮都问纯属浪费 */
const CONFIG_TTL_MS = 5 * 60_000;
/**
 * 「上游此刻不要验证码」也要缓存，但用更短的 TTL。
 * 缓存负面结果是为了不每 2.5 秒去打一次上游配置端点；又不能太长 ——
 * 用户在设置页盼着池子填满时，等 5 分钟才回头问一次就太迟钝了。
 */
const CONFIG_DISABLED_TTL_MS = 60_000;

function pollMsOf(stats: ZcodeCaptchaPoolStats): number {
  const value = Number(stats.pollMs);
  if (!Number.isFinite(value) || value <= 0) return POLL_MS;
  return Math.min(POLL_MAX_MS, Math.max(POLL_MIN_MS, Math.round(value)));
}

function maxPerRoundOf(stats: ZcodeCaptchaPoolStats): number {
  const value = Number(stats.maxPerRound);
  if (!Number.isFinite(value) || value <= 0) return MAX_PER_ROUND;
  return Math.min(PER_ROUND_MAX, Math.max(PER_ROUND_MIN, Math.round(value)));
}

let timer = 0;
let running = false;
let stopped = true;
/** 上一轮看到的 3007 计数（变大 = 上游刚拒了令牌，立刻补货） */
let lastRejected = 0;
/** 连续失败次数（退让用） */
let failures = 0;
let configCache: { value: AliyunMintConfig | null; expiresAt: number } = {
  value: null,
  expiresAt: 0,
};

type AliyunMintConfig = Required<Pick<ZcodeCaptchaConfig, 'sceneId' | 'prefix' | 'region'>>;

const log = (message: string, ...args: unknown[]) =>
  console.log(`[ZCodeCaptcha] ${message}`, ...args);
const warn = (message: string, ...args: unknown[]) =>
  console.warn(`[ZCodeCaptcha] ${message}`, ...args);

/** 拿一次池子概况；失败返回 null，循环安静地退让。 */
async function fetchStats(): Promise<ZcodeCaptchaPoolStats | null> {
  try {
    const response = await api.getZcodeCaptcha();
    return response.success ? response.data ?? null : null;
  } catch (error) {
    log(`读取令牌池概况失败：${(error as Error)?.message || error}`);
    return null;
  }
}

async function pushTokens(tokens: { param: string; region: string }[]): Promise<boolean> {
  if (tokens.length === 0) return false;
  try {
    const response = await api.pushZcodeCaptchaTokens(tokens);
    return response.success;
  } catch (error) {
    log(`令牌入池失败：${(error as Error)?.message || error}`);
    return false;
  }
}

/** 铸一个（走无痕验证；SDK 未就绪或用户正在拖滑块时返回 null）。 */
async function mintOne(config: AliyunMintConfig): Promise<string | null> {
  // 用户正在拖滑块（领套餐）时让路：两条流程都在抢 SDK 的资源
  if (isBusy()) return null;
  try {
    return await mintTraceless(config);
  } catch (error) {
    log(`铸造失败：${(error as Error)?.message || error}`);
    return null;
  }
}

/**
 * 风控配置探测结果。
 *
 * `transient` 区分「这两类该歇一会儿（上游不要验证码 / 没账号）」与
 * 「这次是取失败（要退让重试）」—— 两者都返回 `config: null`，但下一轮的节奏
 * 不同：前者按正常轮询间隔，后者按退让间隔。不退让就会在配置读不到时
 * 以 2.5 秒的节奏敲上游。
 */
interface ConfigProbe {
  config: AliyunMintConfig | null;
  transient: boolean;
}

/**
 * 取阿里云风控配置（`{enabled, prefix, sceneId, region}`）。
 *
 * 与「领套餐」走的是**同一份**上游配置（`GET /api/v1/client/configs` 的
 * `configs.captcha`）。账号 id 由网关给（`captchaAccountId`，它挑的是走这条路
 * 的那个）—— 界面不必自己读账号表。
 */
async function fetchCaptchaConfig(stats: ZcodeCaptchaPoolStats): Promise<ConfigProbe> {
  if (configCache.expiresAt > Date.now()) {
    return { config: configCache.value, transient: false };
  }
  const accountId = String(stats.captchaAccountId || '');
  if (!accountId) return { config: null, transient: false };
  try {
    const response = await api.getZcodeCaptchaConfig(accountId);
    const captcha = response.success ? response.data?.captcha ?? null : null;
    if (!captcha?.enabled || !captcha.sceneId) {
      // 上游此刻不要验证码（或配置不完整）：按「不铸造」处理，过一阵再问
      configCache = { value: null, expiresAt: Date.now() + CONFIG_DISABLED_TTL_MS };
      log('上游风控配置未启用（enabled:false）——不铸造');
      return { config: null, transient: false };
    }
    const value: AliyunMintConfig = {
      sceneId: captcha.sceneId,
      prefix: captcha.prefix || '',
      region: captcha.region || '',
    };
    configCache = { value, expiresAt: Date.now() + CONFIG_TTL_MS };
    return { config: value, transient: false };
  } catch (error) {
    log(`取风控配置失败：${(error as Error)?.message || error}`);
    return { config: null, transient: true };
  }
}

/** 一轮：读概况 → 需要就补 → 排下一次 */
async function round(): Promise<void> {
  if (running || stopped) return;
  running = true;
  try {
    const stats = await fetchStats();
    if (!stats) {
      schedule(BACKOFF_MIN_MS);
      return;
    }
    // 上游刚拒过令牌（3007）：库存即使「够」也不可信了，也补一轮
    const rejectedNow = Number(stats.rejected) || 0;
    const challenged = rejectedNow > lastRejected;
    lastRejected = rejectedNow;
    const want = stats.needsTokens === true || challenged;
    if (!want) {
      schedule(pollMsOf(stats));
      return;
    }
    const probe = await fetchCaptchaConfig(stats);
    if (!probe.config) {
      // 上游不要验证码 / 没账号：按正常节奏再问；取失败才退让
      schedule(probe.transient ? BACKOFF_MIN_MS : pollMsOf(stats));
      return;
    }
    const config = probe.config;
    const target = Math.max(1, Number(stats.target) || 1);
    const ready = Number(stats.ready) || 0;
    const need = Math.min(maxPerRoundOf(stats), Math.max(0, target - ready));
    const tokens: { param: string; region: string }[] = [];
    for (let index = 0; index < need; index += 1) {
      const param = await mintOne(config);
      if (!param) break;
      tokens.push({ param, region: config.region || '' });
      // 连续铸造之间留一点间隔：SDK 的实例重建需要时间
      await new Promise<void>((resolve) => window.setTimeout(resolve, 500));
    }
    if (tokens.length === 0) {
      backoff('本轮未能铸出令牌');
      return;
    }
    await pushTokens(tokens);
    failures = 0;
    schedule(pollMsOf(stats));
  } catch (error) {
    backoff(`本轮异常：${(error as Error)?.message || error}`);
  } finally {
    running = false;
  }
}

/** 退让：连续失败两次就重置铸造实例（实例可能已损坏）。 */
function backoff(reason: string) {
  failures += 1;
  if (failures >= 2) {
    try {
      invalidateMintInstance();
    } catch {
      /* 忽略 */
    }
  }
  const delay = Math.min(BACKOFF_MIN_MS * failures, BACKOFF_MAX_MS);
  warn(`${reason}（连续失败 ${failures} 次），将在 ${delay}ms 后重试`);
  schedule(delay);
}

function schedule(delay: number) {
  window.clearTimeout(timer);
  if (stopped) return;
  timer = window.setTimeout(() => void round(), delay);
}

/** 起来（管理页登录后调一次；重复调只是重置计时器）。 */
export function start(): void {
  stopped = false;
  schedule(1_500);
}

/** 停掉（登出管理页时调；不清池子，网关那侧照常服务）。 */
export function stop(): void {
  stopped = true;
  window.clearTimeout(timer);
}

/** 当前是否在跑（面板显示「后台铸造中」用）。 */
export function isRunning(): boolean {
  return !stopped;
}