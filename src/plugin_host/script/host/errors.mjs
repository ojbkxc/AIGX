// 错误分类 —— AIGX 自研脚本宿主（上游 AGPL，仅按协议契约独立书写）。
//
// 错误码口径（宿主 → 网关的 error.code 取值，网关据此分流处置）：
// - unsupported                       —— 方法/能力脚本未实现
// - account_model_unavailable         —— 请求的模型在该账号上不可用
// - account_model_permissions_unknown —— 账号对该模型的权限未知
// - local_precondition                —— 本地前置条件不满足（缺配置/缺账号/参数缺失）
// - internal                          —— 兜底（未识别的错误一律归 internal）
// - provider_http                     —— 上游 HTTP 失败；错误对象附带数字 status 字段
//
// 线上形状：{"id":X,"error":{"message","code"[,"status"]}}。网关侧
// RpcError 只声明 message/code（未知字段忽略不炸），status 是给新解析器
// 的前向扩展——旧网关从 message 文本里也能看到状态码。
//
// 特殊映射：脚本以 `signIn: "expired"`（或 code:"expired"）报登录态失效
// 时，宿主归一成 provider_http + status 401 —— 网关按「上游 401」处置
// （标记账号 unhealthy / 触发重登）。

/** wire 错误码常量。 */
export const ErrorCodes = Object.freeze({
  Unsupported: "unsupported",
  AccountModelUnavailable: "account_model_unavailable",
  AccountModelPermissionsUnknown: "account_model_permissions_unknown",
  LocalPrecondition: "local_precondition",
  Internal: "internal",
  ProviderHttp: "provider_http",
});

const KNOWN_CODES = new Set(Object.values(ErrorCodes));

/** 网关回调报错码 → 宿主错误码的映射（回调失败多半是本地条件问题）。 */
const GATEWAY_CODE_MAP = Object.freeze({
  not_found: "local_precondition",
  bad_request: "local_precondition",
  no_handler: "unsupported",
});

/** 带错误码的宿主错误。extra 里的字段（如 status）原样保留，随线上帧带出。 */
export class ProviderError extends Error {
  constructor(code, message, extra = undefined) {
    super(message);
    this.name = "ProviderError";
    this.code = code;
    if (extra && typeof extra === "object") Object.assign(this, extra);
  }
}

/** 快捷构造：local_precondition。 */
export function precondition(message) {
  return new ProviderError(ErrorCodes.LocalPrecondition, message);
}

/** 快捷构造：unsupported。 */
export function unsupported(message) {
  return new ProviderError(ErrorCodes.Unsupported, message);
}

/**
 * 把任意抛出物归一成 wire 错误形状 {message, code, status?}。
 *
 * 判定顺序（先专后泛）：
 * 1. ProviderError —— 宿主自己构造的，直接采信；
 * 2. signIn:"expired" / code:"expired" —— 登录态失效 → provider_http 401；
 * 3. code 在已知错误码集合 —— 原样采信（附 status 若有）；
 * 4. 只有数字 status —— 脚本自己 fetch 到的 HTTP 失败 → provider_http；
 * 5. 网关回调的错误码按 GATEWAY_CODE_MAP 归一；
 * 6. 其余一律 internal。
 */
export function classifyError(err) {
  if (err instanceof ProviderError) {
    const out = { message: err.message || err.code, code: err.code };
    if (typeof err.status === "number") out.status = err.status;
    return out;
  }
  if (err && typeof err === "object") {
    const code = typeof err.code === "string" ? err.code : undefined;
    // 登录态失效的三种报法 → provider_http 401
    if (err.signIn === "expired" || code === "expired" || code === "signin_expired") {
      return {
        message: err.message || "sign-in expired",
        code: ErrorCodes.ProviderHttp,
        status: 401,
      };
    }
    if (code && KNOWN_CODES.has(code)) {
      const out = { message: err.message || code, code };
      if (typeof err.status === "number") out.status = err.status;
      return out;
    }
    // 脚本抛了带数字 status 的错误（比如自己 fetch 拿到的 HTTP 失败）
    if (typeof err.status === "number" && err.status > 0) {
      return {
        message: err.message || `upstream http ${err.status}`,
        code: ErrorCodes.ProviderHttp,
        status: err.status,
      };
    }
    // 网关回调失败（store.get 没账号 / no_handler 等）——按映射归一
    if (code && GATEWAY_CODE_MAP[code]) {
      return { message: err.message || code, code: GATEWAY_CODE_MAP[code] };
    }
    if (err.message) return { message: String(err.message), code: ErrorCodes.Internal };
  }
  return { message: String(err ?? "unknown error"), code: ErrorCodes.Internal };
}

/** 任何抛出物的一句话描述（日志用，不抛）。 */
export function describeError(err) {
  if (err instanceof Error) return err.message;
  if (err && typeof err === "object") {
    try {
      return JSON.stringify(err);
    } catch {
      return String(err);
    }
  }
  return String(err);
}
