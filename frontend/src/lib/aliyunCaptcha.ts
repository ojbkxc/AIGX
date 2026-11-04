/**
 * 阿里云验证码（滑块 / 无痕）求解器 —— ZCode「领套餐」与令牌铸造共用一份。
 *
 * 来源：Agent2API `desktop-tauri/ui/aliyun-captcha.js`（本身逐条移植自 AutoClaw
 * 桌面端渲染层）。两个调用方**只有一半相同**：
 *
 * - 领套餐：解验证码 → 用 `verifyParam` 直接去领取（用户可见的一次动作）；
 * - 令牌铸造（`mintTraceless`）：不弹滑块、不要用户操作，SDK 自己跑完风控把
 *   串交给回调，交给 `lib/zcodeCaptchaPool.ts` 推给网关。
 *
 * 两种集成模式拿到的值不一样：滑块走的 `captchaVerifyCallback` 收到的是裸
 * JSON（上游拒收，实测 3007），无痕走 `success` 才拿到上游认的 base64 串。
 * 因此两条路径各自一个 SDK 实例（元素 id 也不同），互不干扰。
 *
 * ── SDK 是浏览器端 JS，为什么能这么用 ─────────────────────────
 * SDK 从 `o.alicdn.com` 加载（`AliyunCaptcha.js`）。若给页面加了 CSP，必须在
 * `script-src` 里放行 `https://o.alicdn.com` 与 `https://*.alicdn.com`（SDK 自己
 * 还会再拉资源），否则验证码会静默加载失败。
 *
 * ── 与 Agent2API 的三处差异（都是为了 TS + 我们的界面约定）────
 * 1. 没有 `wbI18n`：文案直接写中文；
 * 2. 对外导出具名函数，而不是挂 `window.wbAliyunCaptcha`；
 * 3. `mintTraceless` 的超时 / 失败路径保持原样（一次一铸、失败即作废实例）。
 */

/** 上游风控配置（来自 `GET /api/v1/client/configs` 的 `configs.captcha`）。 */
export interface AliyunCaptchaConfig {
  sceneId: string;
  prefix: string;
  /** 阿里云站点（`cn` / `sgp` …） */
  region: string;
}

const SCRIPT_URL = 'https://o.alicdn.com/captcha-frontend/aliyunCaptcha/AliyunCaptcha.js';
const SCRIPT_ID = 'aliyun-captcha-sdk';
/** 滑块面板的容器（SDK 按 id 找） */
const ELEMENT_ID = 'aliyun-captcha-element';
/** 触发按钮：SDK 要求传一个 button 选择器，点它才弹滑块。做成 1×1 不可见，
 *  触发完全由代码控制（免得用户自己点出两个滑块）。 */
const BUTTON_ID = 'aliyun-captcha-trigger';

const SCRIPT_LOAD_TIMEOUT_MS = 40_000;
const INIT_TIMEOUT_MS = 40_000;
const VERIFY_TIMEOUT_MS = 120_000;
/** 无痕铸造的超时（卡住就重来，不留着等） */
const MINT_TIMEOUT_MS = 20_000;
/** 初始化后至少等 2.1 秒再点（实测：SDK 预热没完成时点击无效） */
const MINIMUM_WARMUP_MS = 2_100;
/** 初始化结果最多复用 19 分钟（超过则重建，避免实例内部状态过期） */
const INITIALIZATION_MAX_AGE_MS = 19 * 60_000;

/* ── 错误类型 ─────────────────────────────────────────────────── */

/** 验证码流程失败（`message` 是给人看的中文，界面可直接展示）。 */
export class CaptchaError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'CaptchaError';
  }
}

/** 用户主动取消（与「失败」分开，界面不报红）。 */
export class CaptchaCancelledError extends CaptchaError {
  constructor() {
    super('已取消验证码');
    this.name = 'CaptchaCancelledError';
  }
}

/** 调用方对一次验证串的处理结果（两个都为 true SDK 才收起滑块）。 */
export interface CaptchaRequestResult {
  captchaResult: boolean;
  bizResult: boolean;
}

/* ── SDK 类型（只声明用到的部分）─────────────────────────────── */

interface AliyunCaptchaInstance {
  destroy?: () => void;
  startTracelessVerification?: () => void;
  show?: () => void;
}

interface AliyunCaptchaInitOptions {
  SceneId: string;
  mode: string;
  element: string;
  button: string;
  language: string;
  region?: string;
  prefix?: string;
  captchaLogoImg?: string;
  showErrorTip?: boolean;
  slideStyle?: { width: number; height: number };
  captchaVerifyCallback?: (param: string) => Promise<CaptchaRequestResult> | CaptchaRequestResult;
  onBizResultCallback?: () => void;
  getInstance: (instance: AliyunCaptchaInstance) => void;
  success?: (result: unknown) => void;
  fail?: (error: { message?: string } | undefined) => void;
  onError?: (error: { message?: string } | undefined) => void;
}

type InitAliyunCaptcha = (options: AliyunCaptchaInitOptions) => void;

declare global {
  interface Window {
    AliyunCaptchaConfig?: { region: string; prefix: string };
    initAliyunCaptcha?: InitAliyunCaptcha;
  }
}

/* ── 模块级状态（一次只允许一个滑块流程）─────────────────────── */

let scriptLoadPromise: Promise<void> | null = null;
let initializationPromise: Promise<void> | null = null;
/** 初始化键（region:prefix:sceneId:language）—— 配置变了就重建实例 */
let initializationKey = '';
/** 代际号：异步流程回来时用它判断「这一轮是否已被作废」 */
let initializationGeneration = 0;
let initializedAt = 0;
let captchaInstance: AliyunCaptchaInstance | null = null;

interface PendingVerification {
  generation: number;
  instance: AliyunCaptchaInstance;
  request: (verifyParam: string) => Promise<CaptchaRequestResult>;
  resolve: (value: CaptchaRequestResult) => void;
  reject: (error: Error) => void;
  timer: number;
  settled: boolean;
}

let pendingVerification: PendingVerification | null = null;

const sleep = (ms: number) => new Promise<void>((resolve) => window.setTimeout(resolve, ms));

/**
 * 语言映射：阿里云只认这几个短码，直接把 `zh-CN` 递过去会被当成不认识而回落英文。
 */
function resolveLanguage(language: string): string {
  const normalized = String(language || '').trim().replace(/_/g, '-').toLowerCase();
  if (normalized === 'zh-tw' || normalized.startsWith('zh-hant')) return 'tw';
  if (normalized.startsWith('zh')) return 'cn';
  if (normalized.startsWith('ar')) return 'ar';
  if (normalized.startsWith('de')) return 'de';
  if (normalized.startsWith('es')) return 'es';
  if (normalized.startsWith('fr')) return 'fr';
  if (normalized.startsWith('id') || normalized.startsWith('in')) return 'in';
  if (normalized.startsWith('it')) return 'it';
  if (normalized.startsWith('ja')) return 'ja';
  if (normalized.startsWith('ko')) return 'ko';
  if (normalized.startsWith('pt')) return 'pt';
  if (normalized.startsWith('ru')) return 'ru';
  if (normalized.startsWith('th')) return 'th';
  if (normalized.startsWith('tr')) return 'tr';
  if (normalized.startsWith('vi')) return 'vi';
  return 'en';
}

/** 界面当前语言（取不到按英文，阿里云能兜住）。 */
function currentLanguage(): string {
  return document.documentElement.lang || 'zh-CN';
}

function getInitAliyunCaptcha(): InitAliyunCaptcha | null {
  const value = window.initAliyunCaptcha;
  return typeof value === 'function' ? value : null;
}

function removeCaptchaElements() {
  document.getElementById(ELEMENT_ID)?.remove();
  document.getElementById(BUTTON_ID)?.remove();
}

/** 作废当前滑块实例（配置变了 / 用完了要重建时调）。 */
function invalidateInitialization() {
  const instance = captchaInstance;
  initializationGeneration += 1;
  captchaInstance = null;
  initializationPromise = null;
  initializedAt = 0;
  removeCaptchaElements();
  try {
    instance?.destroy?.();
  } catch {
    /* 忽略：实例已不可用 */
  }
}

/** 备好滑块容器与触发按钮（两个都由 SDK 按 id 找）。 */
function ensureCaptchaElements(): HTMLButtonElement {
  let element = document.getElementById(ELEMENT_ID);
  if (!element) {
    element = document.createElement('div');
    element.id = ELEMENT_ID;
    element.style.position = 'relative';
    element.style.zIndex = '2147483000';
    document.body.appendChild(element);
  }
  let button = document.getElementById(BUTTON_ID) as HTMLButtonElement | null;
  if (!button) {
    button = document.createElement('button');
    button.id = BUTTON_ID;
    button.type = 'button';
    button.tabIndex = -1;
    button.setAttribute('aria-hidden', 'true');
    button.style.position = 'fixed';
    button.style.width = '1px';
    button.style.height = '1px';
    button.style.opacity = '0';
    button.style.pointerEvents = 'none';
    button.style.overflow = 'hidden';
    document.body.appendChild(button);
  }
  return button;
}

/**
 * 加载 SDK 脚本。
 *
 * `window.AliyunCaptchaConfig` 必须在脚本加载**之前**设好 —— SDK 读它决定打哪个
 * 阿里云站点。设晚了 SDK 会用默认站点，表现是「验证码弹出来但一直转圈」。
 */
function loadScript(config: AliyunCaptchaConfig): Promise<void> {
  window.AliyunCaptchaConfig = { region: config.region, prefix: config.prefix };
  if (getInitAliyunCaptcha()) return Promise.resolve();
  if (scriptLoadPromise) return scriptLoadPromise;
  scriptLoadPromise = new Promise<void>((resolve, reject) => {
    const existing = document.getElementById(SCRIPT_ID) as HTMLScriptElement | null;
    const script = existing || document.createElement('script');
    let timer = 0;
    const cleanup = () => {
      window.clearTimeout(timer);
      script.removeEventListener('load', onLoad);
      script.removeEventListener('error', onError);
    };
    const fail = (error: Error) => {
      cleanup();
      scriptLoadPromise = null;
      script.remove();
      reject(error);
    };
    const onLoad = () => {
      cleanup();
      if (getInitAliyunCaptcha()) resolve();
      else fail(new CaptchaError('验证码组件加载异常，请重试'));
    };
    const onError = () => fail(new CaptchaError('验证码组件加载失败，请检查网络后重试'));
    timer = window.setTimeout(
      () => fail(new CaptchaError('验证码组件加载超时，请检查网络后重试')),
      SCRIPT_LOAD_TIMEOUT_MS,
    );
    script.addEventListener('load', onLoad);
    script.addEventListener('error', onError);
    if (!existing) {
      script.id = SCRIPT_ID;
      script.async = true;
      script.src = SCRIPT_URL;
      document.head.appendChild(script);
    }
  });
  return scriptLoadPromise;
}

function settlePending(
  pending: PendingVerification,
  outcome: { value: CaptchaRequestResult } | { error: Error },
): boolean {
  if (pendingVerification !== pending || pending.settled) return false;
  pending.settled = true;
  window.clearTimeout(pending.timer);
  if ('error' in outcome) pending.reject(outcome.error);
  else pending.resolve(outcome.value);
  return true;
}

/**
 * SDK 回调：拿到不透明验证串 → 交给调用方去干它那件事。
 *
 * 返回值必须是 `{captchaResult, bizResult}`：`captchaResult` 是验证码本身是否
 * 通过，`bizResult` 是调用方的业务是否接受它。两个都为 true SDK 才收起滑块。
 */
async function captchaVerifyCallback(
  generation: number,
  captchaVerifyParam: string,
): Promise<CaptchaRequestResult> {
  const pending = pendingVerification;
  if (!pending || pending.generation !== generation) {
    return { captchaResult: false, bizResult: false };
  }
  if (typeof captchaVerifyParam !== 'string' || captchaVerifyParam.length === 0) {
    settlePending(pending, { error: new CaptchaError('验证码校验失败，请重试') });
    return { captchaResult: false, bizResult: false };
  }
  try {
    const result = await pending.request(captchaVerifyParam);
    if (pendingVerification !== pending || pending.settled) {
      return {
        captchaResult: Boolean(result?.captchaResult),
        bizResult: Boolean(result?.bizResult),
      };
    }
    settlePending(pending, { value: result });
    return {
      captchaResult: Boolean(result?.captchaResult),
      bizResult: Boolean(result?.bizResult),
    };
  } catch (error) {
    settlePending(pending, { error: error as Error });
    return { captchaResult: false, bizResult: false };
  }
}

/**
 * 初始化滑块 SDK 实例（同配置 19 分钟内复用，配置变了或过期就重建）。
 *
 * `getInstance` 回调是「SDK 准备好了」的唯一信号 —— 没有它我们不知道实例何时可用。
 */
function initializeAliyunCaptcha(config: AliyunCaptchaConfig): Promise<void> {
  const language = resolveLanguage(currentLanguage());
  const key = `${config.region}:${config.prefix}:${config.sceneId}:${language}`;
  if (initializationPromise && initializationKey === key) {
    const inFlight = initializedAt === 0;
    const fresh = Date.now() - initializedAt < INITIALIZATION_MAX_AGE_MS;
    if (inFlight || fresh) return initializationPromise;
    invalidateInitialization();
  }
  if (initializationKey && initializationKey !== key) invalidateInitialization();
  initializationKey = key;
  const generation = ++initializationGeneration;
  initializationPromise = (async () => {
    await loadScript(config);
    ensureCaptchaElements();
    const initAliyunCaptcha = getInitAliyunCaptcha();
    if (!initAliyunCaptcha) throw new CaptchaError('验证码组件不可用，请重试');
    await new Promise<void>((resolve, reject) => {
      let settled = false;
      let boundInstance: AliyunCaptchaInstance | null = null;
      const timer = window.setTimeout(() => {
        if (settled) return;
        settled = true;
        reject(new CaptchaError('验证码组件初始化超时，请重试'));
      }, INIT_TIMEOUT_MS);
      const settle = (callback: () => void): boolean => {
        if (settled) return false;
        settled = true;
        window.clearTimeout(timer);
        callback();
        return true;
      };
      try {
        initAliyunCaptcha({
          SceneId: config.sceneId,
          mode: 'popup',
          element: `#${ELEMENT_ID}`,
          button: `#${BUTTON_ID}`,
          captchaVerifyCallback: (param) => captchaVerifyCallback(generation, param),
          // 业务结果回调：调用方对「验证码过了但业务没通过」的反馈走这里。
          onBizResultCallback: () => {},
          getInstance: (instance) => {
            boundInstance = instance;
            if (generation !== initializationGeneration) {
              try {
                instance.destroy?.();
              } catch {
                /* 忽略 */
              }
              return;
            }
            const ok = settle(() => {
              captchaInstance = instance;
              initializedAt = Date.now();
              resolve();
            });
            if (!ok) {
              try {
                instance.destroy?.();
              } catch {
                /* 忽略 */
              }
            }
          },
          slideStyle: { width: 360, height: 40 },
          language,
          onError: (error) => {
            const pending = pendingVerification;
            if (
              pending
              && pending.generation === generation
              && pending.instance === boundInstance
            ) {
              settlePending(pending, {
                error: new CaptchaError(error?.message || '验证码校验失败，请重试'),
              });
              return;
            }
            if (settled) {
              if (
                generation === initializationGeneration
                && (!boundInstance || captchaInstance === boundInstance)
              ) {
                invalidateInitialization();
              }
              return;
            }
            settle(() => reject(new CaptchaError(error?.message || '验证码组件不可用，请重试')));
          },
        });
      } catch (error) {
        settle(() =>
          reject(new CaptchaError((error as Error)?.message || '验证码组件不可用，请重试')));
      }
    });
    if (!captchaInstance) throw new CaptchaError('验证码组件不可用，请重试');
  })();
  return initializationPromise;
}

/** 等 SDK 预热完成（实测：太快点击弹不出滑块）。 */
async function waitForWarmup(): Promise<void> {
  const remaining = MINIMUM_WARMUP_MS - (Date.now() - initializedAt);
  if (remaining > 0) await sleep(remaining);
}

/**
 * 走一次完整滑块验证：初始化 → 预热 → 弹滑块 → 用户拖 → 拿串交给调用方。
 *
 * `request(verifyParam)` 由调用方注入，返回 `{captchaResult, bizResult}`；
 * 它 resolve 出来的值会原样成为 `solve()` 的返回值。
 *
 * ── 超时只包着「拖滑块 + 那一次请求」────────────────────────
 * 120 秒覆盖到这里为止。调用方的 `request` 若做长流程（等登录回调之类）会被这个
 * 超时误杀。长流程要放在 `solve()` **返回之后**。
 */
export async function solve(
  config: AliyunCaptchaConfig,
  request: (verifyParam: string) => Promise<CaptchaRequestResult>,
): Promise<CaptchaRequestResult> {
  await initializeAliyunCaptcha(config);
  await waitForWarmup();
  const button = ensureCaptchaElements();
  const instance = captchaInstance;
  if (!instance) throw new CaptchaError('验证码组件不可用，请重试');
  const generation = initializationGeneration;
  return new Promise<CaptchaRequestResult>((resolve, reject) => {
    const pending: PendingVerification = {
      generation,
      instance,
      request,
      resolve,
      reject,
      timer: 0,
      settled: false,
    };
    pending.timer = window.setTimeout(() => {
      settlePending(pending, { error: new CaptchaError('验证码校验超时，请重试') });
    }, VERIFY_TIMEOUT_MS);
    pendingVerification = pending;
    button.click();
  });
}

/* ── 静默铸造（令牌池的令牌来源）─────────────────────────────── */

/**
 * 铸造容器 id **每次铸造都换一套**（与滑块流程的也分开）。
 *
 * 为什么不复用：实测「destroy 之后在同一个容器上重新 init」会卡在初始化（20 秒
 * 超时），换一套新容器就正常 —— SDK 在容器上留了内部状态。
 */
let mintSeq = 0;
function mintIds() {
  mintSeq += 1;
  return {
    elementId: `zcode-mint-captcha-element-${mintSeq}`,
    buttonId: `zcode-mint-captcha-trigger-${mintSeq}`,
  };
}

let mintInstancePromise: Promise<AliyunCaptchaInstance> | null = null;
let mintInstanceKey = '';
let mintInstanceRef: AliyunCaptchaInstance | null = null;
let mintElementId = '';
let mintButtonId = '';
let mintWaiter: {
  generation: number;
  resolve: (value: string) => void;
  reject: (error: Error) => void;
} | null = null;
let mintGeneration = 0;
let mintInitializedAt = 0;

/**
 * 模拟真实用户行为（页面可见性 + 鼠标移动），提升无痕风控通过率，并避免后台窗口
 * 被判定为完全无交互的 headless 环境。
 */
function simulateHumanActivity() {
  try {
    if (document.hidden) {
      Object.defineProperty(document, 'hidden', { value: false, configurable: true });
    }
    if (document.visibilityState !== 'visible') {
      Object.defineProperty(document, 'visibilityState', {
        value: 'visible',
        configurable: true,
      });
    }
  } catch {
    /* 忽略只读拦截 */
  }
  try {
    for (let i = 0; i < 3; i++) {
      const x = Math.floor(100 + Math.random() * 300);
      const y = Math.floor(100 + Math.random() * 200);
      const event = new MouseEvent('mousemove', {
        bubbles: true,
        cancelable: true,
        view: window,
        clientX: x,
        clientY: y,
        screenX: x + 50,
        screenY: y + 50,
      });
      window.dispatchEvent(event);
    }
  } catch {
    /* 忽略事件派发失败 */
  }
}

/**
 * 备好铸造用的容器与触发按钮。
 *
 * 放在**屏幕外**而不是 `display:none`：SDK 会读容器尺寸做布局，隐藏容器在部分
 * 版本里会让实例初始化不出来（无痕模式虽不显示 UI，SDK 仍按容器初始化）。
 */
function ensureMintElements(elementId: string, buttonId: string) {
  let element = document.getElementById(elementId);
  if (!element) {
    element = document.createElement('div');
    element.id = elementId;
    element.style.position = 'fixed';
    element.style.left = '-9999px';
    element.style.top = '0';
    element.style.width = '360px';
    element.style.height = '40px';
    document.body.appendChild(element);
  }
  let button = document.getElementById(buttonId);
  if (!button) {
    button = document.createElement('button');
    button.id = buttonId;
    button.type = 'button';
    button.tabIndex = -1;
    button.setAttribute('aria-hidden', 'true');
    button.style.position = 'fixed';
    button.style.left = '-9999px';
    button.style.width = '1px';
    button.style.height = '1px';
    button.style.opacity = '0';
    button.style.pointerEvents = 'none';
    document.body.appendChild(button);
  }
}

/** 铸造实例作废（失败/超时/铸完都调；下一次用新实例重来）。 */
export function invalidateMintInstance() {
  const instance = mintInstanceRef;
  mintInstancePromise = null;
  mintInstanceKey = '';
  mintInstanceRef = null;
  mintInitializedAt = 0;
  try {
    instance?.destroy?.();
  } catch {
    /* 忽略：实例已不可用 */
  }
  try {
    document.getElementById(mintElementId)?.remove();
  } catch {
    /* 忽略 */
  }
  try {
    document.getElementById(mintButtonId)?.remove();
  } catch {
    /* 忽略 */
  }
  mintElementId = '';
  mintButtonId = '';
}

/** 铸造失败：落定正在等的那次（并作废实例）。 */
function failMint(error: Error) {
  const waiter = mintWaiter;
  mintWaiter = null;
  invalidateMintInstance();
  if (waiter) waiter.reject(error);
}

/**
 * 校验并归一一个验证串。
 *
 * 上游只认「约 280 字符的 base64 JSON，且内含一个长 securityToken」。SDK 的降级
 * 路径会给出短串（拿去请求必回 3007），所以宁可在这里拒绝、让上层重铸，也不要把
 * 一个注定 3007 的串推进池子 —— 那会烧掉一次往返还把「库存有货」变成假的。
 */
function validateVerifyParam(value: unknown): string {
  if (typeof value !== 'string' || value.trim().length < 200) {
    throw new CaptchaError('验证码组件返回的验证串不完整（请重试）');
  }
  const text = value.trim();
  try {
    const json = JSON.parse(atob(text)) as { securityToken?: unknown; SecurityToken?: unknown };
    const token = json && (json.securityToken || json.SecurityToken);
    if (!token || String(token).length < 50) throw new Error('no securityToken');
  } catch {
    throw new CaptchaError('验证码组件返回的验证串不是上游认的形态（请重试）');
  }
  return text;
}

/**
 * SDK 的 `success` 回调：把结果交给正在等的那次铸造。
 *
 * 不同模式下 SDK 给的值不一样（字符串 / 带 `verifyParam`、`captchaVerifyParam`、
 * `data`、`param` 的对象），四种都取一遍再交给 {@link validateVerifyParam} 把关。
 */
function deliverMintResult(result: unknown) {
  const waiter = mintWaiter;
  if (!waiter) return;
  let value: unknown = result;
  if (result && typeof result === 'object') {
    const object = result as Record<string, unknown>;
    value = object.verifyParam || object.captchaVerifyParam || object.data || object.param;
  }
  let param: string | null = null;
  let failure: Error | null = null;
  try {
    param = validateVerifyParam(value);
  } catch (error) {
    failure = error as Error;
  }
  mintWaiter = null;
  if (failure || !param) {
    // 先把等待位放回去再失败：`failMint` 靠 `mintWaiter` 找等待者，
    // 这里已经清空了它，直接调会让调用方那条 promise 永远悬着（铸造器卡死）。
    mintWaiter = waiter;
    failMint(failure ?? new CaptchaError('验证码组件返回的验证串不可用'));
    return;
  }
  // ── 铸完就作废实例（**一次一铸**，实测结论）──────────────────
  // 同一个实例第二次调 `startTracelessVerification()` 必失败（2026-09-28 实测：
  // 第一次 958ms 拿到串，第二次直接走 fail 回调）。所以每次铸造都重建实例 ——
  // 代价约 1 秒，换「第二次必然成功」。
  invalidateMintInstance();
  waiter.resolve(param);
}

/** 备好铸造实例（同配置复用；失败/超时后由 invalidateMintInstance 作废）。 */
function ensureMintInstance(config: AliyunCaptchaConfig): Promise<AliyunCaptchaInstance> {
  const language = resolveLanguage(currentLanguage());
  const key = `${config.region}:${config.prefix}:${config.sceneId}:${language}`;
  if (mintInstancePromise && mintInstanceKey === key) return mintInstancePromise;
  if (mintInstanceRef) invalidateMintInstance();
  mintInstanceKey = key;
  const pending = (async () => {
    await loadScript(config);
    const ids = mintIds();
    mintElementId = ids.elementId;
    mintButtonId = ids.buttonId;
    ensureMintElements(ids.elementId, ids.buttonId);
    const initAliyunCaptcha = getInitAliyunCaptcha();
    if (!initAliyunCaptcha) throw new CaptchaError('验证码组件不可用，请重试');
    return new Promise<AliyunCaptchaInstance>((resolve, reject) => {
      let settled = false;
      const timer = window.setTimeout(() => {
        if (settled) return;
        settled = true;
        mintInstancePromise = null;
        reject(new CaptchaError('验证码组件初始化超时，请重试'));
      }, INIT_TIMEOUT_MS);
      try {
        initAliyunCaptcha({
          SceneId: config.sceneId,
          mode: 'popup',
          region: config.region,
          prefix: config.prefix,
          element: `#${ids.elementId}`,
          button: `#${ids.buttonId}`,
          captchaLogoImg: '',
          showErrorTip: false,
          language,
          getInstance: (instance) => {
            if (settled) return;
            settled = true;
            window.clearTimeout(timer);
            mintInstanceRef = instance;
            mintInitializedAt = Date.now();
            resolve(instance);
          },
          success: (result) => deliverMintResult(result),
          fail: (error) => failMint(new CaptchaError(error?.message || '验证码校验失败，请重试')),
          onError: (error) =>
            failMint(new CaptchaError(error?.message || '验证码组件出错，请重试')),
        });
      } catch (error) {
        if (settled) return;
        settled = true;
        window.clearTimeout(timer);
        mintInstancePromise = null;
        reject(new CaptchaError((error as Error)?.message || '验证码组件初始化失败'));
      }
    });
  })();
  // 初始化失败不要把失败的 promise 缓存住（下一次要能重来）
  pending.catch(() => {
    mintInstancePromise = null;
    mintInstanceKey = '';
  });
  mintInstancePromise = pending;
  return pending;
}

/**
 * 静默铸一个验证串（无痕验证；不弹滑块、不需要用户操作）。
 *
 * 转发链路**每条请求**都要一个当次铸的令牌，而转发发生在后台，没有让用户拖滑块
 * 的机会 —— 无痕模式本来就是为这种场景设计的。
 *
 * 返回值就是这个串。**一次一用**：铸好不用，两分钟后自己过期（上游拒收）。
 */
export async function mintTraceless(config: AliyunCaptchaConfig): Promise<string> {
  const instance = await ensureMintInstance(config);
  // SDK 预热等待：初始化后必须收集足够的设备环境与指纹（至少 2.1 秒），
  // 否则直接调 startTracelessVerification 会触发 fail / onError。
  const elapsed = Date.now() - mintInitializedAt;
  if (elapsed < MINIMUM_WARMUP_MS) await sleep(MINIMUM_WARMUP_MS - elapsed);
  simulateHumanActivity();

  const generation = ++mintGeneration;
  return new Promise<string>((resolve, reject) => {
    const timer = window.setTimeout(() => {
      if (!mintWaiter || mintWaiter.generation !== generation) return;
      mintWaiter = null;
      invalidateMintInstance();
      reject(new CaptchaError('静默铸造超时，请重试'));
    }, MINT_TIMEOUT_MS);
    mintWaiter = {
      generation,
      resolve: (value) => {
        window.clearTimeout(timer);
        resolve(value);
      },
      reject: (error) => {
        window.clearTimeout(timer);
        reject(error);
      },
    };
    const start = typeof instance.startTracelessVerification === 'function'
      ? instance.startTracelessVerification
      : instance.show;
    if (typeof start !== 'function') {
      failMint(new CaptchaError('验证码组件不支持静默铸造'));
      return;
    }
    try {
      start.call(instance);
    } catch (error) {
      failMint(new CaptchaError((error as Error)?.message || '验证码组件启动失败'));
    }
  });
}

/** 用户取消：把等待中的那次落定成「已取消」并作废实例。 */
export function cancel(): boolean {
  const pending = pendingVerification;
  if (!pending) return false;
  const cancelled = settlePending(pending, { error: new CaptchaCancelledError() });
  if (cancelled) invalidateInitialization();
  return cancelled;
}

/** 有没有正在等的滑块流程（调用方用它决定关弹窗时要不要提示）。 */
export function isBusy(): boolean {
  return pendingVerification !== null;
}