/**
 * 组的增删改弹窗（对齐 Agent2API v3.5.1 `routing-group-dialogs.tsx`）。
 *
 * 三个弹窗：
 * 1. `RoutingGroupDialog` —— 编辑路由组字段（组名 / 对外模型 ID / 描述 / 组 id）。
 *    底部按钮 A2A 是「取消 / 应用到草稿」：本仓同样只改草稿，真正落盘由
 *    页面底部的「保存档案」负责（A2A 也是这一步才 save）。
 * 2. `RoutingMemberDialog` —— 添加参与模型：模型选择器 + 「添加 · N」。
 * 3. `RoutingMemberAccounts` —— 参与账号：列出该提供商账号目录逐个加入/移出 +
 *    上下移 + 单账号设置入口 + 手输 id 兜底（目录不可用时）+ 全部移出。
 */
import type React from 'react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowDown, ArrowUp, Settings2, Trash2 } from 'lucide-react';
import type { RoutingMember } from '../../types/routing-profile';
import type { RoutingAccountRef } from './routing-account-row';
import { RoutingModelPicker } from './routing-model-picker';
import type { RouteModelChoice } from './routing-model-picker';
import {
  createRouteMembers,
  routeGroupFieldsError,
  RoutingGroupForm,
  ROUTING_MEMBER_LIMIT,
  type RoutingGroupFields,
} from './routing-group-form';

/** 弹窗外壳（AIGX 的 modal-* 体系；A2A 用 Dialog） */
function RoutingDialog({ title, width, onClose, footer, children }: {
  title: string;
  width?: number;
  onClose(): void;
  footer: React.ReactNode;
  children: React.ReactNode;
}): JSX.Element {
  const { t } = useTranslation();
  return (
    <div
      className="modal-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={title}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="modal" style={{ maxWidth: width ?? 600, width: '92%' }} onClick={(event) => event.stopPropagation()}>
        <div className="modal-header">
          <h3>{title}</h3>
          <button type="button" className="modal-close" aria-label={t('关闭')} onClick={onClose}>
            ×
          </button>
        </div>
        <div className="modal-body">{children}</div>
        <div className="modal-footer">{footer}</div>
      </div>
    </div>
  );
}

export function RoutingGroupDialog({ fields, otherIds, onSave, onClose }: {
  fields: RoutingGroupFields;
  /** 其它组的 id（重名检测） */
  otherIds: string[];
  onSave(fields: RoutingGroupFields): void;
  onClose(): void;
}): JSX.Element {
  const { t } = useTranslation();
  const [draft, setDraft] = useState<RoutingGroupFields>(fields);
  const [error, setError] = useState('');
  const save = (): void => {
    const message = routeGroupFieldsError(draft, otherIds, fields.id);
    if (message) {
      setError(t(message));
      return;
    }
    onSave({ id: draft.id.trim(), name: draft.name.trim(), model: draft.model.trim().toLowerCase(), description: draft.description?.trim() ?? '' });
    onClose();
  };
  return (
    <RoutingDialog
      title={t('编辑路由组')}
      onClose={onClose}
      footer={<>
        <button type="button" className="btn btn-outline" onClick={onClose}>{t('取消')}</button>
        <button type="button" className="btn btn-primary" onClick={save}>{t('应用到草稿')}</button>
      </>}
    >
      <p className="rt-config-note">
        {t('把不同提供商、不同模型组合成一个入口。客户端使用对外模型 ID 发起请求。')}
      </p>
      <RoutingGroupForm value={draft} onChange={setDraft} />
      {error ? <p className="rt-config-error" role="alert">{error}</p> : null}
    </RoutingDialog>
  );
}

export function RoutingMemberDialog({ choices, members, onAdd, onClose }: {
  choices: RouteModelChoice[];
  members: RoutingMember[];
  onAdd(members: RoutingMember[]): void;
  onClose(): void;
}): JSX.Element {
  const { t } = useTranslation();
  const [selected, setSelected] = useState<string[]>([]);
  const options = choices.filter(
    (choice) => !members.some(
      (member) => member.provider === choice.provider && member.model.toLowerCase() === choice.model.toLowerCase(),
    ),
  );
  const byKey = new Map(options.map((choice) => [`${choice.provider}\u0000${choice.model.toLowerCase()}`, choice]));
  const picked = selected.flatMap((key) => {
    const choice = byKey.get(key);
    return choice ? [choice] : [];
  });
  const limit = Math.max(0, ROUTING_MEMBER_LIMIT - members.length);
  const add = (): void => {
    if (!picked.length || picked.length > limit) return;
    onAdd(createRouteMembers(picked));
    onClose();
  };
  return (
    <RoutingDialog
      title={t('添加参与模型')}
      onClose={onClose}
      footer={<>
        <button type="button" className="btn btn-outline" onClick={onClose}>{t('取消')}</button>
        <button
          type="button"
          className="btn btn-primary"
          disabled={!picked.length || picked.length > limit}
          onClick={add}
        >
          {t('添加')} · {picked.length}
        </button>
      </>}
    >
      <p className="rt-config-note">{t('可以跨提供商、跨模型多选。同一提供商也可以加入不同模型。')}</p>
      <RoutingModelPicker choices={options} selected={selected} onChange={setSelected} limit={limit} />
      <p className="rt-config-note">{t('仅列出真实目录中的对话模型，不自动更改模型映射。')}</p>
    </RoutingDialog>
  );
}

export function RoutingMemberAccounts({ member, accounts = [], onAccount, onSave, onClose }: {
  member: RoutingMember;
  /** 该提供商的账号目录（A2A `accounts`）；有目录时逐个账号加入/移出，无需手输 id */
  accounts?: RoutingAccountRef[];
  /** 单账号设置入口（A7） */
  onAccount?(id: string): void;
  onSave(ids: string[]): void;
  onClose(): void;
}): JSX.Element {
  const { t } = useTranslation();
  const [selected, setSelected] = useState<string[]>(member.accounts);
  const [input, setInput] = useState('');
  /** A2A：已选在前（保持顺序），目录里其余账号补后 */
  const order = [...new Set([...selected, ...accounts.map((account) => account.id)])];
  const move = (id: string, direction: number): void => setSelected((current) => {
    const next = [...current];
    const index = next.indexOf(id);
    const to = index + direction;
    if (index >= 0 && to >= 0 && to < next.length) {
      next[index] = next[to];
      next[to] = id;
    }
    return next;
  });
  const add = (): void => {
    const id = input.trim();
    if (!id || selected.includes(id)) {
      setInput('');
      return;
    }
    setSelected((current) => [...current, id]);
    setInput('');
  };
  return (
    <RoutingDialog
      title={`${t('参与账号')} · ${member.model}`}
      width={680}
      onClose={onClose}
      footer={<>
        <button type="button" className="btn btn-outline" onClick={onClose}>{t('取消')}</button>
        <button type="button" className="btn btn-primary" onClick={() => { onSave(selected); onClose(); }}>
          {t('应用到草稿')}
        </button>
      </>}
    >
      <p className="rt-config-note">
        {t('只影响这个路由成员，不改变账号启用状态，也不影响其他模型。新增账号需要手动加入。')}
      </p>
      <p className="rt-config-note">
        {accounts.length
          ? t('列出该提供商的全部账号：勾选加入 / 移出本路由，上下移调整顺序（留空 = 该槽位用此提供商下的全部账号）。')
          : t('账号目录未接入：这里按账号 id 手动维护参与名单与顺序（留空 = 该槽位用此提供商下的全部账号）。')}
      </p>
      <div className="rt-account-add">
        <input
          className="form-input"
          value={input}
          placeholder={t('账号 id，如 acc-1024')}
          aria-label={t('参与账号')}
          onChange={(event) => setInput(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === 'Enter') {
              event.preventDefault();
              add();
            }
          }}
        />
        <button type="button" className="btn btn-secondary btn-sm" onClick={add} disabled={!input.trim()}>
          {t('添加')}
        </button>
      </div>
      <div className="rt-picker-list">
        {order.map((id) => {
          const account = accounts.find((item) => item.id === id);
          const on = selected.includes(id);
          const index = selected.indexOf(id);
          return (
            <div className="rt-picker-row" key={id}>
              <div className="rt-picker-row-main">
                <b>{account ? account.name || account.id : id}</b>
                <small>
                  {!account && on ? t('账号已移除') : account?.enabled === false ? t('账号已禁用') : on ? t('参与本路由') : t('未参与本路由')}
                </small>
              </div>
              {onAccount ? (
                <button
                  type="button"
                  className="btn btn-outline btn-sm"
                  title={t('账号设置')}
                  aria-label={t('{{name}} · 账号设置', { name: account?.name || id })}
                  onClick={() => onAccount(id)}
                >
                  <Settings2 size={13} />
                </button>
              ) : null}
              <button
                type="button"
                className="btn btn-outline btn-sm"
                disabled={!on || index === 0}
                aria-label={t('提高优先级')}
                onClick={() => move(id, -1)}
              >
                <ArrowUp size={13} />
              </button>
              <button
                type="button"
                className="btn btn-outline btn-sm"
                disabled={!on || index === selected.length - 1}
                aria-label={t('降低优先级')}
                onClick={() => move(id, 1)}
              >
                <ArrowDown size={13} />
              </button>
              <button
                type="button"
                className="btn btn-outline btn-sm"
                disabled={!account && !on}
                onClick={() => setSelected((current) => on ? current.filter((value) => value !== id) : [...current, id])}
              >
                {on ? t('移出本路由') : t('加入路由')}
              </button>
            </div>
          );
        })}
        {!order.length ? (
          <p className="rt-config-note">{t('没有点名账号：该槽位使用此提供商下的全部账号。')}</p>
        ) : null}
      </div>
      {member.accounts.length ? (
        <button type="button" className="rt-link-btn" onClick={() => setSelected([])}>
          <Trash2 size={12} aria-hidden="true" /> {t('全部移出')}
        </button>
      ) : null}
    </RoutingDialog>
  );
}