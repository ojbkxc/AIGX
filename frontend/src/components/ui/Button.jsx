import React from 'react';
import { cn } from '../../lib/utils';

const variants = {
  primary: 'btn btn-primary',
  outline: 'btn btn-outline',
  secondary: 'btn btn-secondary',
  danger: 'btn btn-danger',
};

/**
 * Button — 复用 App.css 按钮体系的轻量封装。
 * variant: primary | outline | secondary | danger；size: sm。
 */
export default function Button({
  children,
  variant = 'primary',
  size,
  className,
  type = 'button',
  ...rest
}) {
  return (
    <button
      type={type}
      className={cn(variants[variant] || variants.primary, size === 'sm' && 'btn-sm', className)}
      {...rest}
    >
      {children}
    </button>
  );
}
