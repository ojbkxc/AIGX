/**
 * 「添加账号」弹窗的各家表单配置（逐字移植自 Agent2API 的 add-account-configs.ts，
 * 按 AIGX 的 provider 目录与凭证形态收敛：同一家只保留一个 provider id）。
 *
 * 只放数据、不放 JSX：弹窗组件（components/AddAccountDialog.tsx）按这张表渲染
 * 手填字段、登录方式与说明；新增一家供应商时只改这里。
 */

export type AddMethodId = 'web' | 'sms' | 'manual';

export interface AddField {
  /** 表单草稿里的键（也是展示顺序） */
  key: string;
  label: string;
  /** 多行文本（凭据整串） */
  rows?: number;
  optional?: boolean;
  placeholder?: string;
  /**
   * 这一格落到账号请求体的哪个位置：
   * - credential：`api_key`（登录态型供应商的 Cookie / JWT / API Key）
   * - name：备注名
   * - base_url：上游地址
   * - header：额外请求头里的同名头（如 Trae 的 X-Uid）
   */
  target?: 'credential' | 'name' | 'base_url' | 'header';
  /** target=header 时的请求头名（缺省用 key） */
  header?: string;
  /** 整段是 JSON：提交时逐键并进请求体（CodeArts 的凭据就是这种形态） */
  json?: boolean;
}

export interface ProviderAddConfig {
  provider: string;
  label: string;
  /** 手填段标题 */
  manualTitle: string;
  /** 手填段说明（纯文本，避免 dangerouslySetInnerHTML） */
  manualNote: string;
  /** 手填段主按钮文案 */
  addButton: string;
  /** 露出的添加方式（按顺序渲染成分段） */
  methods: AddMethodId[];
  fields: AddField[];
  web?: {
    note: string;
    button: string;
    /** 官方登录页（远程网关收不到本机回调，登录完成后回填凭证） */
    url: string;
    hint?: string;
    /** 该家支持把浏览器地址栏里的回调地址粘回网关换取凭证 */
    callback?: boolean;
  };
  sms?: {
    note: string;
  };
}

/** 通用 API Key 型供应商（非登录态）没有独立配置，按 provider 元数据兜底 */
export const SESSION_ADD_CONFIGS: Record<string, ProviderAddConfig> = {
  workbuddy: {
    provider: 'workbuddy',
    label: 'WorkBuddy',
    manualTitle: '填写凭证',
    manualNote:
      'accessToken 是官方客户端的登录令牌（形如一长串 JWT）。两版账号（国内版 WorkBuddy / 国际版 WorkBuddy AI）可同时保存，各自一份模型清单。',
    addButton: '添加 WorkBuddy 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开官方登录页并完成登录。远程网关收不到浏览器回调，登录成功后请把客户端登录态里的 accessToken 复制到「填写凭证」；本机部署也可以用客户端凭证文件导入。',
      button: '打开 WorkBuddy 登录页',
      url: 'https://www.workbuddy.cn/',
      hint: '登录完成后回到本弹窗，切到「填写凭证」粘贴 accessToken',
    },
    fields: [
      {
        key: 'accessToken',
        label: 'accessToken',
        rows: 3,
        target: 'credential',
        placeholder: '粘贴 WorkBuddy 的 accessToken（auth.accessToken）',
      },
      {
        key: 'name',
        label: '备注名',
        optional: true,
        target: 'name',
        placeholder: '可选，留空则用凭证里的账号名',
      },
    ],
  },
  xiaohuanxiong: {
    provider: 'xiaohuanxiong',
    label: '小浣熊',
    manualTitle: '粘贴 token / refreshToken',
    manualNote:
      'refreshToken 可选，填了到期可自动续期；两者都能从小浣熊客户端登录态文件（~/.box-agent/config/auth.json）里取到。',
    addButton: '添加小浣熊账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开小浣熊官方登录页完成登录。远程网关收不到浏览器回调，登录成功后请把客户端登录态里的 token 复制到「填写凭证」。',
      button: '打开小浣熊登录页',
      url: 'https://xiaohuanxiong.com/',
      hint: '登录完成后回到本弹窗，切到「填写凭证」粘贴 token',
      callback: true,
    },
    fields: [
      {
        key: 'token',
        label: 'token',
        rows: 3,
        target: 'credential',
        placeholder: '粘贴 access_token（一长串 JWT）',
      },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用凭证里的账号名' },
    ],
  },
  catpaw: {
    provider: 'catpaw',
    label: 'CatPaw',
    manualTitle: '填写凭证',
    manualNote: 'token 是登录态 Cookie（X-Passport-Token）；CatPaw 没有刷新机制，过期后需在客户端重新登录。',
    addButton: '添加 CatPaw 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开 CatPaw 官方登录页（美团 passport）完成登录。远程网关收不到浏览器回调，登录成功后请把 X-Passport-Token 复制到「填写凭证」。',
      button: '打开 CatPaw 登录页',
      url: 'https://catpaw.meituan.com/',
      hint: '登录完成后回到本弹窗，切到「填写凭证」粘贴 token',
    },
    fields: [
      {
        key: 'token',
        label: 'token',
        rows: 3,
        target: 'credential',
        placeholder: 'CatPaw 的 X-Passport-Token（登录态 Cookie）',
      },
      { key: 'uid', label: 'uid', optional: true, target: 'header', header: 'x-uid', placeholder: '可选，CatPaw 账号标识' },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用登录名或 uid' },
    ],
  },
  cline: {
    provider: 'cline',
    label: 'Cline',
    manualTitle: '填写凭证',
    manualNote:
      'accessToken 形如 workos:…（不带前缀也会自动补上）；refreshToken 可选，填了可自动续期。两者都能从 Cline 客户端登录态文件（~/.cline/data/settings/providers.json）取到。',
    addButton: '添加 Cline 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开 Cline 官方授权页完成设备授权（授权页给出的一次性代码在浏览器里确认即可）。远程网关无法代替浏览器确认，完成后请把 accessToken 复制到「填写凭证」。',
      button: '打开 Cline 授权页',
      url: 'https://app.cline.bot/',
      hint: '授权完成后回到本弹窗，切到「填写凭证」粘贴 accessToken',
    },
    fields: [
      {
        key: 'accessToken',
        label: 'accessToken',
        rows: 3,
        target: 'credential',
        placeholder: '粘贴 workos:… 开头的令牌（不带前缀也会自动补上）',
      },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空使用账号姓名或令牌指纹' },
    ],
  },
  qoder: {
    provider: 'qoder',
    label: 'Qoder',
    manualTitle: '使用个人访问令牌（PAT）',
    manualNote:
      '在 Qoder 账号设置 → Integrations 生成 PAT（别填 Google / GitHub 的令牌）。中国版与国际版的账号与凭证不通用。COSY 签名由网关自动完成。',
    addButton: '添加 Qoder 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开 Qoder 官方授权页完成设备码授权。远程网关无法自动接收授权结果，完成后请把 accessToken 复制到「填写凭证」。',
      button: '打开 Qoder 授权页',
      url: 'https://qoder.com/',
      hint: '授权完成后回到本弹窗，切到「填写凭证」粘贴访问令牌',
    },
    fields: [
      { key: 'pat', label: 'Qoder PAT', rows: 3, target: 'credential', placeholder: '粘贴 Qoder 个人访问令牌（pt-…）' },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空使用账号昵称或邮箱' },
    ],
  },
  accio: {
    provider: 'accio',
    label: 'Accio',
    manualTitle: '填写凭证',
    manualNote:
      'accessToken 是 Accio 的登录凭证（一长串不透明 token，不是 JWT）；refreshToken 可选，填了之后到期能自动续期。国际版与国内版是两套独立账号，凭证不通用。',
    addButton: '添加 Accio 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开 Accio 官方登录页完成登录。远程网关收不到浏览器回调，登录成功后请把 accessToken 复制到「填写凭证」。',
      button: '打开 Accio 登录页',
      url: 'https://www.accio.com/',
      hint: '登录完成后回到本弹窗，切到「填写凭证」粘贴 accessToken',
      callback: true,
    },
    fields: [
      { key: 'accessToken', label: 'accessToken', rows: 3, target: 'credential', placeholder: '粘贴 Accio 的 accessToken' },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空使用账号昵称或邮箱' },
    ],
  },
  zcode: {
    provider: 'zcode',
    label: 'ZCode',
    manualTitle: '填写凭证',
    manualNote:
      '两个互不替代的凭证，按你要用的功能填，至少填一个：编码套餐 API Key 用于转发推理（打 open.bigmodel.cn）；jwt 用于领取套餐与查询余额。国内版与国际版是两套独立账号与套餐。',
    addButton: '添加 ZCode 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开 ZCode 官方授权页完成授权。远程网关无法自动取回授权结果，完成后请把编码套餐 API Key 复制到「填写凭证」。',
      button: '打开 ZCode 授权页',
      url: 'https://zcode.z.ai/',
      hint: '授权完成后回到本弹窗，切到「填写凭证」粘贴 API Key',
    },
    fields: [
      {
        key: 'accessToken',
        label: '编码套餐 API Key',
        rows: 3,
        target: 'credential',
        placeholder: '用于转发；形如 apiKey.secret 两段点分（不填则这个账号不能转发）',
      },
      { key: 'jwt', label: 'Coding Plan JWT', rows: 3, optional: true, placeholder: '用于领取套餐与查询余额；不填则这个账号不能领取与查余额' },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空自动生成' },
    ],
  },
  codearts: {
    provider: 'codearts',
    label: '华为 CodeArts',
    manualTitle: '粘贴登录凭据',
    manualNote:
      '整份粘贴官方插件 / CLIProxyAPI 落盘的凭据 JSON（形如 {"codearts_provider_credential":{…}}，铺平的也行）。必填：access_key_id、secret_access_key、security_token；要能自动续期，必须连 refresh_token 与 oauth_context 一起粘。临时凭据约一小时到期，缺这半块就续不回来，只能重新登录。',
    addButton: '添加 CodeArts 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开华为云 CodeArts 官方授权页登录。登录完成后浏览器会跳回网关自己的 /oauth/callback；网关跑在另一台机器上时收不到回调，请把地址栏里的最终回调地址（含 code）复制到「填写凭证」旁的说明里按提示换取，或改用整份凭据 JSON 粘贴。',
      button: '打开 CodeArts 授权页',
      url: 'https://devcloud.huaweicloud.com/',
      hint: '远程部署时把地址栏回调地址复制回来，按提示处理',
      callback: true,
    },
    fields: [
      {
        key: 'credentialJson',
        label: '凭据 JSON',
        rows: 8,
        json: true,
        target: 'credential',
        placeholder:
          '{"codearts_provider_credential":{"access_key_id":"HSTA…","secret_access_key":"…","security_token":"…","expires_at":"…","refresh_token":"eyJ…","oauth_context":{…}}}',
      },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空使用凭据里的 user_name' },
    ],
  },
  trae: {
    provider: 'trae',
    label: 'Trae',
    manualTitle: '填写凭证',
    manualNote:
      'accessToken 是 Trae 的 Cloud-IDE-JWT（三段点分）。手工粘贴时请连 machineId / deviceId 一起填：上游把它们与登录时上传的设备公钥绑在一起判设备，凭空换一对会撞 2xxxx 那族设备绑定拒绝。',
    addButton: '添加 Trae 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开 Trae SOLO 官方授权页并用你的 Trae 账号登录。授权完成后官方页面会跳回本机的一个临时端口；远程网关收不到回调，请把地址栏里的回调地址或登录态令牌复制到「填写凭证」。',
      button: '打开 Trae 授权页',
      url: 'https://www.trae.cn/',
      hint: '远程部署时把地址栏回调地址复制回来，或粘贴 accessToken',
      callback: true,
    },
    fields: [
      { key: 'accessToken', label: 'accessToken', rows: 3, target: 'credential', placeholder: 'Cloud-IDE-JWT（三段点分）' },
      { key: 'uid', label: '用户 ID', optional: true, target: 'header', header: 'x-uid', placeholder: '可选；用于去重与展示名' },
      { key: 'machineId', label: 'machineId', optional: true, target: 'header', header: 'x-machine-id', placeholder: '可选；UUID 形态，与凭据同生共死' },
      {
        key: 'deviceId',
        label: 'deviceId',
        optional: true,
        target: 'header',
        header: 'x-device-id',
        placeholder: '可选；纯数字串（上游只收 8–24 位数字）',
      },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空自动用昵称' },
    ],
  },
  loomy: {
    provider: 'loomy',
    label: 'Loomy',
    manualTitle: '填写凭证',
    manualNote:
      '粘贴的是登录 session（同时是模型网关的 token）。Loomy 没有续期接口，session 有效期约 14 天，过期后重新登录即可。也可以用上面的「手机验证码登录」直接发码登录。',
    addButton: '添加 Loomy 账号',
    methods: ['sms', 'manual'],
    sms: {
      note: '用 Loomy 账号绑定的手机号登录：点「获取验证码」后填入即可。这是本家最省事的入口。',
    },
    fields: [
      { key: 'session', label: 'session', rows: 3, target: 'credential', placeholder: '登录 session（一长串）' },
      { key: 'userId', label: 'userId', optional: true, placeholder: '可选，讯飞侧 userid' },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用脱敏手机号或 userId' },
    ],
  },
  kuku: {
    provider: 'kuku',
    label: 'KukuAI',
    manualTitle: '粘贴 Cookie',
    manualNote:
      'BDUSS 必填；请粘贴完整 Cookie（其中 PTOKEN 必须保留，网关要用它向百度换发 KukuAI 业务会话令牌）。支持整段 Cookie 头、Cookie 编辑器导出的 JSON 数组，或 {BDUSS, STOKEN} 对象。KukuAI 没有刷新机制，登录态过期后重新粘贴即可。',
    addButton: '添加 KukuAI 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开 KukuAI 主站用百度账号登录（手机验证码 / 扫码均可）。远程网关读不到浏览器 Cookie，登录成功后请从浏览器开发者工具复制完整 Cookie，粘贴到「填写凭证」。',
      button: '打开 KukuAI 登录页',
      url: 'https://kuku.baidu.com/',
      hint: '登录完成后把完整 Cookie 粘贴到「填写凭证」',
    },
    fields: [
      {
        key: 'cookie',
        label: 'Cookie',
        rows: 3,
        target: 'credential',
        placeholder: 'BDUSS=…; STOKEN=…（或 Cookie 编辑器导出的 JSON）',
      },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用账号 uk' },
    ],
  },
  monkeycode: {
    provider: 'monkeycode',
    label: 'MonkeyCode',
    manualTitle: '粘贴 session cookie',
    manualNote:
      '粘贴网页登录态 cookie monkeycode_ai_session 的值（在浏览器里复制整段 Cookie 直接粘贴也行，网关会自己剥出值）。MonkeyCode 没有续期接口：session 有效期约 30 天（硬限制），过期后重新登录网页、再粘贴一次即可。',
    addButton: '添加 MonkeyCode 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开 MonkeyCode 主站登录。远程网关读不到浏览器 Cookie，登录成功后请复制 monkeycode_ai_session 的值粘贴到「填写凭证」。',
      button: '打开 MonkeyCode 登录页',
      url: 'https://monkeycode-ai.com/',
      hint: '登录完成后把 session cookie 粘贴到「填写凭证」',
    },
    fields: [
      {
        key: 'session',
        label: 'session',
        rows: 3,
        target: 'credential',
        placeholder: 'monkeycode_ai_session 的值（也可直接粘贴整段 Cookie）',
      },
      {
        key: 'imageId',
        label: '任务镜像 id',
        optional: true,
        placeholder: '可选：发起对话必需，留空则由网关从该账号已有任务里自动发现',
      },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用上游返回的账号名或账号 id' },
    ],
  },
  commandcode: {
    provider: 'commandcode',
    label: 'Command Code',
    manualTitle: '粘贴 API Key',
    manualNote:
      '粘贴 Command Code 的 API Key（user_ 开头；从官方 CLI 的 ~/.commandcode/auth.json 或网页 studio 获取）。整行 / 带引号 / 带 Bearer 前缀地粘都行，网关会自己剥成裸 key。key 不过期、也没有续期接口，失效后重新获取即可。',
    addButton: '添加 Command Code 账号',
    methods: ['manual'],
    fields: [
      {
        key: 'apiKey',
        label: 'apiKey',
        rows: 3,
        target: 'credential',
        placeholder: 'user_ 开头的 API Key（也可直接粘贴 Authorization: Bearer … 整行）',
      },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用 key 尾号' },
    ],
  },
  antigravity: {
    provider: 'antigravity',
    label: 'Antigravity',
    manualTitle: '粘贴 Google refresh token',
    manualNote:
      '粘贴 Antigravity 的 Google refresh token（以 1// 开头；直接粘贴含 refresh_token= 的整行也行）。refresh token 不会过期，access token 到期由网关自动续期。',
    addButton: '添加 Antigravity 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '在打开的窗口里登录 Google 账号（Antigravity 的授权页）。远程网关收不到 loopback 回调，授权完成后请把 refresh token 粘贴到「填写凭证」。',
      button: '打开 Antigravity 网页登录',
      url: 'https://antigravity.google/',
      hint: '授权完成后回到本弹窗，切到「填写凭证」粘贴 refresh token',
      callback: true,
    },
    fields: [
      {
        key: 'refreshToken',
        label: 'refreshToken',
        rows: 2,
        target: 'credential',
        placeholder: '1// 开头的 Google refresh token（也可直接粘贴 refresh_token=… 整行）',
      },
      { key: 'projectId', label: 'projectId', optional: true, placeholder: '可选（cloudaicompanionProject）：留空由网关自动发现' },
      { key: 'email', label: 'email', optional: true, placeholder: '可选：账号邮箱，用于展示与身份识别' },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用邮箱或令牌尾号' },
    ],
  },
  officeace: {
    provider: 'officeace',
    label: 'OfficeAce 果办',
    manualTitle: '粘贴网关凭据',
    manualNote:
      '从别的机器（或 OfficeAce 桌面端 models.json）取三项贴进来：模型网关基址（baseUrl，形如 https://modelgw-…/v2）与 modelAppKey / modelAppSecret。这三项就能转发（网关凭据不过期）。',
    addButton: '添加 OfficeAce 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '打开华为云授权页登录。登录完成后网关会自动轮询云端取回授权码（不需要装 OfficeAce 桌面端，也无需手工回填回调地址）。远程网关无法自动登录时，请把网关凭据粘贴到「填写凭证」。',
      button: '打开授权页',
      url: 'https://www.huaweicloud.com/',
      hint: '登录完成后回到本弹窗等待自动完成（无需粘贴回调地址）',
    },
    fields: [
      { key: 'baseUrl', label: '模型网关基址', target: 'base_url', placeholder: 'https://modelgw-…huaweicloud-agentarts.com/v2' },
      { key: 'modelAppKey', label: '网关 Key', placeholder: 'model_app_key' },
      { key: 'modelAppSecret', label: '网关 Secret', placeholder: 'model_app_secret' },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空用「OfficeAce 果办」' },
    ],
  },
  orcaterm: {
    provider: 'orcaterm',
    label: 'OrcaTerm',
    manualTitle: '粘贴 accessToken',
    manualNote:
      '粘贴 OrcaTerm 的 OAuth accessToken（JWT，三段点分；从桌面客户端 data.bin 或 Web 版 Cookie ot_session 获取）。直接粘贴含 Bearer / ot_session= 的整行也行，网关会自己剥成裸 token。添加后由网关自动续期（上游有续期接口）。',
    addButton: '添加 OrcaTerm 账号',
    methods: ['web', 'manual'],
    web: {
      note:
        '在打开的官方 Web 控制台里用微信扫码登录（窗口会自动弹出登录面板；若没有，点左下角头像 → 立即登录）。远程网关读不到浏览器 Cookie，登录成功后请把 ot_session / accessToken 粘贴到「填写凭证」。',
      button: '打开 OrcaTerm 控制台',
      url: 'https://orcaterm.cloud.tencent.com/',
      hint: '扫码完成后把 ot_session / accessToken 粘贴到「填写凭证」',
    },
    fields: [
      {
        key: 'accessToken',
        label: 'accessToken',
        rows: 3,
        target: 'credential',
        placeholder: 'JWT（三段点分；也可直接粘贴 Bearer 整行或 data.bin 内容）',
      },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用账号昵称或 userId' },
    ],
  },
  autoclaw: {
    provider: 'autoclaw',
    label: 'AutoClaw',
    manualTitle: '填写凭证',
    manualNote:
      'token 支持 enc: 前缀（Windows 上自动解密）。未填 refreshToken 无法自动续期。国内版官方唯一登录方式是手机验证码；远程网关没有短信通道，请在客户端登录后粘贴 token。',
    addButton: '添加 AutoClaw 账号',
    methods: ['manual'],
    fields: [
      {
        key: 'token',
        label: 'token',
        rows: 3,
        target: 'credential',
        placeholder: '明文 JWT 或 auth.json 里的 enc: 加密值（自动解密）',
      },
      { key: 'refreshToken', label: 'refreshToken', rows: 2, optional: true, placeholder: '没有则无法自动续期' },
      { key: 'deviceId', label: 'deviceId', optional: true, placeholder: '可选，续期时带上' },
      { key: 'name', label: '备注名', optional: true, target: 'name', placeholder: '可选，留空则用 userId' },
    ],
  },
};

/** 添加方式分段的中文标题 */
export const ADD_METHOD_LABELS: Record<AddMethodId, string> = {
  web: '网页登录',
  sms: '手机验证码登录',
  manual: '填写凭证',
};

export function addConfigOf(provider: string): ProviderAddConfig | undefined {
  return SESSION_ADD_CONFIGS[provider];
}

/* ─── 预置 API / 自定义协议（与账号池页共用同一份目录）────────────── */

export type ProviderProtocol = 'openai' | 'anthropic' | 'gemini';

export interface PresetProviderItem {
  key: string;
  name: string;
  protocol: ProviderProtocol;
  base_url: string;
  hint?: string;
}

/** 预置端点模板（取自 Agent2API preset 目录；与内置目录同地址的会在渲染时去重） */
export const PRESET_PROVIDERS: PresetProviderItem[] = [
  { key: 'openai', name: 'OpenAI', protocol: 'openai', base_url: 'https://api.openai.com/v1' },
  { key: 'anthropic', name: 'Anthropic', protocol: 'anthropic', base_url: 'https://api.anthropic.com' },
  { key: 'openrouter', name: 'OpenRouter', protocol: 'openai', base_url: 'https://openrouter.ai/api/v1' },
  { key: 'huggingface', name: 'HuggingFace', protocol: 'openai', base_url: 'https://router.huggingface.co/v1' },
  { key: 'nvidia', name: 'NVIDIA', protocol: 'openai', base_url: 'https://integrate.api.nvidia.com/v1' },
  {
    key: 'deepseek-anthropic',
    name: 'DeepSeek（Anthropic 端点）',
    protocol: 'anthropic',
    base_url: 'https://api.deepseek.com/anthropic',
  },
  { key: 'glm-cn', name: 'GLM 中国', protocol: 'openai', base_url: 'https://open.bigmodel.cn/api/coding/paas/v4' },
  { key: 'zai-anthropic', name: 'Z.ai（Anthropic 端点）', protocol: 'anthropic', base_url: 'https://api.z.ai/api/anthropic' },
  {
    key: 'siliconflow-intl',
    name: 'SiliconFlow 国际',
    protocol: 'openai',
    base_url: 'https://api.siliconflow.com/v1',
  },
  {
    key: 'volcengine-ark',
    name: '火山方舟 Coding',
    protocol: 'openai',
    base_url: 'https://ark.cn-beijing.volces.com/api/coding/v3',
  },
  {
    key: 'alicode-cn',
    name: '阿里云百炼 Coding（国内）',
    protocol: 'openai',
    base_url: 'https://coding.dashscope.aliyuncs.com/v1',
  },
  {
    key: 'alicode-intl',
    name: '阿里云百炼 Coding（国际）',
    protocol: 'openai',
    base_url: 'https://coding-intl.dashscope.aliyuncs.com/v1',
  },
  {
    key: 'alims-intl',
    name: '阿里云百炼模型服务（国际）',
    protocol: 'openai',
    base_url: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1',
  },
  {
    key: 'alitp-intl',
    name: '阿里云 Token Plan（国际）',
    protocol: 'openai',
    base_url: 'https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1',
  },
  {
    key: 'xiaomi-mimo',
    name: '小米 MiMo Token Plan',
    protocol: 'openai',
    base_url: 'https://token-plan-sgp.xiaomimimo.com/v1',
  },
  { key: 'minimax-cn', name: 'MiniMax 中国', protocol: 'openai', base_url: 'https://api.minimaxi.com/v1' },
  { key: 'sensenova', name: '商汤日日新', protocol: 'openai', base_url: 'https://api.sensenova.cn/compatible-mode/v1' },
];

/** 自定义分段：三种协议，地址由用户自己填。 */
export const CUSTOM_PROTOCOLS: Array<{ kind: string; labelKey: string; hintKey: string }> = [
  { kind: 'openai_compatible', labelKey: 'OpenAI 兼容（自定义地址）', hintKey: '按 {base}/chat/completions 转发' },
  { kind: 'anthropic', labelKey: 'Anthropic 兼容（自定义地址）', hintKey: '按 {base}/v1/messages 转发' },
  { kind: 'gemini', labelKey: 'Gemini 兼容（自定义地址）', hintKey: '按 {base}/models 转发' },
];

/** 协议 → 落库 provider kind（预置模板按协议落到对应兼容 kind）。 */
export function presetKind(protocol: ProviderProtocol): string {
  if (protocol === 'anthropic') return 'anthropic';
  if (protocol === 'gemini') return 'gemini';
  return 'openai_compatible';
}

/** URL 归一（去尾斜杠 / 小写），用于内置与预置同地址去重。 */
export function normalizeUrl(url: string): string {
  return url.trim().replace(/\/+$/, '').toLowerCase();
}
