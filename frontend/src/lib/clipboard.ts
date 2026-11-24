/**
 * 复制文本到剪贴板，带降级通道。
 *
 * 为什么需要降级：`navigator.clipboard` 只在**安全上下文**可用（HTTPS 或
 * localhost）。AIGX 控制台常被内网以 `http://192.168.x.x:端口` 打开——那种
 * 环境下 `navigator.clipboard` 是 `undefined`，只调 API 的写法必然失败且
 * 用户看到的是「浏览器禁用了剪贴板权限」这种误导性提示。
 *
 * 降级用临时 `textarea` + `document.execCommand('copy')`（已废弃但仍是唯一
 * 的非安全上下文方案）。返回值让调用方决定提示文案，不在这里吞错误。
 */
export async function copyText(text: string): Promise<boolean> {
  if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // 权限被拒 / 非用户手势：继续走降级，别直接判失败
    }
  }
  if (typeof document === 'undefined') return false;
  const holder = document.createElement('textarea');
  holder.value = text;
  // 必须在视口内且可选中，否则 iOS Safari 选不中；用 fixed + 透明而非 display:none
  holder.style.position = 'fixed';
  holder.style.top = '0';
  holder.style.left = '0';
  holder.style.opacity = '0';
  holder.setAttribute('readonly', '');
  document.body.appendChild(holder);
  try {
    holder.select();
    holder.setSelectionRange(0, text.length);
    return document.execCommand('copy');
  } catch {
    return false;
  } finally {
    document.body.removeChild(holder);
  }
}
