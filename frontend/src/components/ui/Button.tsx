import React from 'react';
import { cn } from '../../lib/utils';

export interface ButtonProps {
  children: React.ReactNode;
  variant?: 'primary' | 'outline' | 'secondary' | 'danger';
  size?: 'sm';
  className?: string;
  type?: 'button' | 'submit' | 'reset';
  onClick?: React.MouseEventHandler<HTMLButtonElement>;
  disabled?: boolean;
}

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
  onClick,
  disabled,
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={cn(variants[variant] || variants.primary, size === 'sm' && 'btn-sm', className)}
      onClick={onClick}
      disabled={disabled}
      {...rest}
    >
      {children}
    </button>
  );
}
