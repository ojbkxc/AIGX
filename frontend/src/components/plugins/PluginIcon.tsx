/**
 * PluginIcon — 插件图标渲染 + 安全回落。
 *
 * 图标来源（后端契约）：
 * - 已装原生包 `pack.icon`：`data:<mime>;base64,…`（后端 `pack_icon` 直接把
 *   磁盘上的图转成 data URI）；
 * - 商店条目 `item.icon`：目录里的同名字段（可能是 data URI / 绝对 URL）；
 * - 脚本插件 `ScriptPackageView.icon` 或 `ScriptCatalogEntryView.iconSources`：
 *   缓存的 `/plugins/script-icons/<hash>.<ext>` 相对路径、`data:` 或外链。
 *
 * 渲染约定：
 * - `data:` / `blob:` / `http(s):` / 协议相对 `//` 原样使用；
 * - 以 `/` 开头的根相对路径原样使用（生产由后端同源静态服务）；
 * - 其它相对路径补一个前导 `/`，避免相对当前页面路径拼接出错；
 * - 任何候选图 404/解码失败 → 依次尝试下一个候选，全部失败回落首字母（空名回落 🧩）；
 * - 图标始终被固定在 `.pl-card-icon` 尺寸盒内（`overflow:hidden`），不撑破卡片。
 */
import { memo, useState } from 'react';

/** 把后端给的图标值解析成可放进 <img src> 的地址；空/非法返回 ''。 */
export function resolveIconSrc(value: unknown): string {
  if (typeof value !== 'string') return '';
  const raw = value.trim();
  if (!raw) return '';
  // 已经是绝对地址或 data/blob URI：原样使用
  if (/^(?:data:|blob:|https?:|\/\/)/i.test(raw)) return raw;
  // 根相对路径：原样使用
  if (raw.startsWith('/')) return raw;
  // 其余（`icons/a.png`、`./icons/a.png`）按站点根拼接，避免相对页面路径解析
  return `/${raw.replace(/^\.?\//, '')}`;
}

/**
 * 图标盒（性能约束）。
 *
 * 插件列表轮询（下载/批量操作时每 2s 回读）会让卡片整体重渲染；图标常是
 * `data:` URI，若每次重建 `<img>` 触发解码，长列表会明显掉帧。这里用
 * `memo` + 按内容比较 `sources` 数组，父级重渲染时图标节点原样复用。
 */
function PluginIconInner({
  src,
  sources,
  label,
  className,
}: {
  /** 单个图标地址（已装包 / 商店条目） */
  src?: string | null;
  /** 候选地址列表（脚本目录 iconSources；按序回退） */
  sources?: string[];
  /** 回落显示名（取首字母）；缺省显示 🧩 */
  label?: string;
  className?: string;
}): JSX.Element {
  const candidates = (sources && sources.length > 0 ? sources : [src])
    .map(resolveIconSrc)
    .filter((value) => value.length > 0);
  const [index, setIndex] = useState(0);
  const [failed, setFailed] = useState(false);

  const current = candidates[index];
  if (!current || failed) {
    const initial = (label || '').trim().slice(0, 1).toUpperCase();
    return (
      <div className={`pl-card-icon${className ? ` ${className}` : ''}`} aria-hidden="true">
        <span className="pl-card-icon-fallback">{initial || '🧩'}</span>
      </div>
    );
  }

  return (
    <div className={`pl-card-icon${className ? ` ${className}` : ''}`}>
      <img
        src={current}
        alt=""
        loading="lazy"
        decoding="async"
        draggable={false}
        referrerPolicy="no-referrer"
        onError={() => {
          if (index + 1 < candidates.length) setIndex(index + 1);
          else setFailed(true);
        }}
      />
    </div>
  );
}

const sourceKey = (value?: string[]): string => (value && value.length > 0 ? value.join('\u0001') : '');

export const PluginIcon = memo(
  PluginIconInner,
  (prev, next) =>
    prev.src === next.src &&
    prev.label === next.label &&
    prev.className === next.className &&
    sourceKey(prev.sources) === sourceKey(next.sources),
);

export default PluginIcon;
