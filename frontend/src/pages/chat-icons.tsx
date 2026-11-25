/**
 * 助手/对话页用到的图标（内联 SVG，不依赖 lucide 的包体积）。
 *
 * 与 A2A v3.5.1 `islands/assistant-icons.tsx` 的图形同形同义：24 画布、
 * currentColor 描边、圆头线帽。AIGX 无 @ui 的图标资产，这里内联，供
 * composer / messages / settings-drawer / shell 共用。
 */

export type ChatIconProps = { size?: number; className?: string }

export function ChatPlusIcon({ size = 17, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <path d='M12 5.5v13M5.5 12h13' fill='none' stroke='currentColor' strokeWidth='1.9' strokeLinecap='round' />
    </svg>
  )
}

export function ChatImageIcon({ size = 15, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <g fill='none' stroke='currentColor' strokeWidth='1.7' strokeLinecap='round' strokeLinejoin='round'>
        <rect x='3.5' y='4.5' width='17' height='15' rx='2.6' />
        <circle cx='8.7' cy='9.4' r='1.5' fill='currentColor' stroke='none' />
        <path d='M5.2 17.2l4.7-4.7a1.7 1.7 0 0 1 2.4 0l1.5 1.5 2.1-2.1a1.7 1.7 0 0 1 2.4 0l1.8 1.8' />
      </g>
    </svg>
  )
}

export function ChatFileIcon({ size = 15, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <g fill='none' stroke='currentColor' strokeWidth='1.7' strokeLinecap='round' strokeLinejoin='round'>
        <path d='M6.2 3.5h7.1l4.5 4.5v12.5H6.2z' />
        <path d='M13.3 3.7v4.4h4.4M9 12h6M9 15.3h6' />
      </g>
    </svg>
  )
}

export function ChatShieldIcon({ size = 15, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <path d='M12 3.2l7.2 2.7v5.4c0 4.4-2.9 7.6-7.2 9.2-4.3-1.6-7.2-4.8-7.2-9.2V5.9L12 3.2z'
        fill='none' stroke='currentColor' strokeWidth='1.7' strokeLinejoin='round' />
      <path d='M8.9 11.9l2.1 2.1 4.2-4.2' fill='none' stroke='currentColor' strokeWidth='1.8'
        strokeLinecap='round' strokeLinejoin='round' />
    </svg>
  )
}

export function ChatBoltIcon({ size = 15, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <path d='M13.2 2.5L5.4 13.4h5.2l-1.1 8.1 8.3-11.3h-5.2l.6-7.7z' fill='currentColor' />
    </svg>
  )
}

export function ChatSendIcon({ size = 16, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <path d='M12 19.5V5M6 11l6-6 6 6' fill='none' stroke='currentColor' strokeWidth='2'
        strokeLinecap='round' strokeLinejoin='round' />
    </svg>
  )
}

export function ChatChevronIcon({ size = 11, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <path d='M6.5 9.2l5.5 5.4 5.5-5.4' fill='none' stroke='currentColor' strokeWidth='2'
        strokeLinecap='round' strokeLinejoin='round' />
    </svg>
  )
}

export function ChatMiniChevronIcon({ size = 11, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <path d='M9.2 6.5l5.4 5.5-5.4 5.5' fill='none' stroke='currentColor' strokeWidth='2'
        strokeLinecap='round' strokeLinejoin='round' />
    </svg>
  )
}

export function ChatCheckIcon({ size = 12, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <path d='M5 12.7l4.6 4.6L19 7.2' fill='none' stroke='currentColor' strokeWidth='2.2'
        strokeLinecap='round' strokeLinejoin='round' />
    </svg>
  )
}

export function ChatLayersIcon({ size = 13, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <g fill='none' stroke='currentColor' strokeWidth='1.7' strokeLinecap='round' strokeLinejoin='round'>
        <path d='M12 3.6l8.4 4.3L12 12.2 3.6 7.9 12 3.6z' />
        <path d='M4.4 11.7L12 15.6l7.6-3.9M4.4 15.5L12 19.4l7.6-3.9' />
      </g>
    </svg>
  )
}

export function ChatSlidersIcon({ size = 13, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <path d='M4.5 7.5h15M4.5 16.5h15' fill='none' stroke='currentColor' strokeWidth='1.8' strokeLinecap='round' />
      <circle cx='9' cy='7.5' r='2.1' fill='none' stroke='currentColor' strokeWidth='1.8' />
      <circle cx='15.5' cy='16.5' r='2.1' fill='none' stroke='currentColor' strokeWidth='1.8' />
    </svg>
  )
}

export function ChatSearchIcon({ size = 13, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <circle cx='10.6' cy='10.6' r='6' fill='none' stroke='currentColor' strokeWidth='1.8' />
      <path d='M15.2 15.2l4.4 4.4' fill='none' stroke='currentColor' strokeWidth='1.8' strokeLinecap='round' />
    </svg>
  )
}

export function ChatStopIcon({ size = 13, className }: ChatIconProps) {
  return (
    <svg viewBox='0 0 24 24' width={size} height={size} className={className} aria-hidden='true'>
      <rect x='6.8' y='6.8' width='10.4' height='10.4' rx='2.4' fill='currentColor' />
    </svg>
  )
}

/** 助手头像：主色圆底 + spark（与 A2A 的助手头像同形） */
export function ChatAvatar({ className }: { className?: string }) {
  return (
    <span className={className} aria-hidden='true'>
      <svg viewBox='0 0 24 24' width='14' height='14'>
        <path fill='currentColor' d='M12 3.6l1.7 4.9 4.9 1.7-4.9 1.7L12 16.8l-1.7-4.9-4.9-1.7 4.9-1.7L12 3.6z' />
        <path fill='currentColor' d='M18.3 15.2l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8.8-2.2z' />
      </svg>
    </span>
  )
}
