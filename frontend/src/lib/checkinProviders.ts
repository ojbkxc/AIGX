/**
 * 支持「领套餐 / 每日签到」的 provider 白名单。
 *
 * 与后端 `src/checkin/claim.rs` 的 `CHECKIN_PROVIDERS` 同步维护：后端是权威，
 * 新增 / 移除 provider 时两边一起改 —— 否则账号池行内会出现「点了必然报不支持」
 * 的按钮，或该出现的按钮不出现。
 *
 * 顺序沿用后端常量（界面复选框与签到中心分组行的顺序也是它，插在中间会让存量
 * 用户的清单顺序变一下，没必要动）。
 */
export const CHECKIN_PROVIDERS: ReadonlySet<string> = new Set([
  'loomy',
  'kuku',
  'qoder',
  'qoder-intl',
  'xiaohuanxiong',
  'autoclaw',
  'autoclaw-intl',
  'workbuddy',
  'workbuddy-intl',
  'trae',
  'codearts',
  'officeace',
  'chatglm',
  'modelscope',
  'modelscope-intl',
  'atomcode',
  'lobsterai',
  'zcode',
  'zcode-intl',
]);
