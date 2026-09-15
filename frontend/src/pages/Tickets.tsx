import { useState, useEffect, type FormEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { MessageSquarePlus, ChevronLeft, Send, X } from 'lucide-react';
import { api } from '../api';
import { useToast } from '../components/Toast';
import { isAdmin } from '../lib/utils';
import { Button, Card, Input, Select, Textarea, EmptyState, SkeletonTable, Pagination, Badge } from '../components/ui';
import type { TicketItem, TicketMessageItem } from '../types';
import './Tickets.css';

const LEVELS = [0, 1, 2] as const;

function levelText(t: (k: string) => string, level: number): string {
  if (level === 2) return t('高');
  if (level === 1) return t('中');
  return t('低');
}

function levelTone(level: number): 'danger' | 'warning' | 'neutral' {
  if (level === 2) return 'danger';
  if (level === 1) return 'warning';
  return 'neutral';
}

function statusBadge(t: (k: string) => string, status: number): JSX.Element {
  return status === 0
    ? <Badge tone="info">{t('处理中')}</Badge>
    : <Badge tone="neutral">{t('已关闭')}</Badge>;
}

function replyBadge(t: (k: string) => string, replyStatus: number): JSX.Element {
  return replyStatus === 1
    ? <Badge tone="warning">{t('待用户回复')}</Badge>
    : <Badge tone="info">{t('待客服回复')}</Badge>;
}

function fmtTime(ts: number | undefined): string {
  if (!ts) return '—';
  return new Date(ts > 1e12 ? ts : ts * 1000).toLocaleString();
}

export default function Tickets(): JSX.Element {
  const { t } = useTranslation();
  const addToast = useToast();
  const admin = isAdmin();

  const [tickets, setTickets] = useState<TicketItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');

  // 列表态 / 详情态
  const [detail, setDetail] = useState<TicketItem | null>(null);
  const [messages, setMessages] = useState<TicketMessageItem[]>([]);
  const [replyText, setReplyText] = useState('');
  const [sending, setSending] = useState(false);

  // 新建工单表单
  const [showCreate, setShowCreate] = useState(false);
  const [createForm, setCreateForm] = useState({ subject: '', level: 1, message: '' });
  const [creating, setCreating] = useState(false);

  // 管理员列表筛选
  const [statusFilter, setStatusFilter] = useState<string>('');
  const [replyFilter, setReplyFilter] = useState<string>('');
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(1);
  const size = 20;
  const [total, setTotal] = useState(0);

  const loadList = async () => {
    setLoading(true);
    setError('');
    try {
      if (admin) {
        const params: Record<string, string | number> = { page, page_size: size };
        if (statusFilter !== '') params.status = Number(statusFilter);
        if (replyFilter !== '') params.reply_status = Number(replyFilter);
        const res = await api.adminListTickets(params);
        const data = res?.data;
        const list = Array.isArray(data) ? (data as unknown as TicketItem[]) : [];
        setTickets(list);
        setTotal((res as unknown as { total?: number })?.total ?? list.length);
      } else {
        const res = await api.listTickets();
        const data = res?.data;
        setTickets(Array.isArray(data) ? (data as unknown as TicketItem[]) : []);
        setTotal(Array.isArray(data) ? data.length : 0);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void loadList();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [admin, page, statusFilter, replyFilter]);

  const openDetail = async (id: string) => {
    setError('');
    try {
      const res = admin ? await api.adminListTickets({ id }) : await api.listTickets(id);
      const data = res?.data;
      const ticket = (Array.isArray(data) ? (data as unknown as TicketItem[])[0] : data) as TicketItem | undefined;
      if (!ticket) {
        addToast(t('工单不存在'), 'error');
        return;
      }
      setDetail(ticket);
      setMessages(Array.isArray(ticket.message) ? (ticket.message as TicketMessageItem[]) : []);
      setReplyText('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const closeDetail = () => {
    setDetail(null);
    setMessages([]);
    setReplyText('');
  };

  const handleCreate = async (e: FormEvent) => {
    e.preventDefault();
    if (!createForm.subject.trim()) {
      addToast(t('工单主题不能为空'), 'error');
      return;
    }
    if (!createForm.message.trim()) {
      addToast(t('工单内容不能为空'), 'error');
      return;
    }
    setCreating(true);
    setError('');
    try {
      await api.createTicket({
        subject: createForm.subject.trim(),
        level: createForm.level,
        message: createForm.message.trim(),
      });
      addToast(t('工单已提交'));
      setShowCreate(false);
      setCreateForm({ subject: '', level: 1, message: '' });
      await loadList();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCreating(false);
    }
  };

  const handleReply = async () => {
    if (!detail) return;
    if (!replyText.trim()) {
      addToast(t('回复内容不能为空'), 'error');
      return;
    }
    setSending(true);
    setError('');
    try {
      const res = admin
        ? await api.adminReplyTicket(detail.id, replyText.trim())
        : await api.replyTicket(detail.id, replyText.trim());
      const updated = res?.data as TicketItem | undefined;
      if (updated) setDetail(updated);
      setReplyText('');
      addToast(t('回复成功'));
      // 重新拉取详情（含最新消息）
      await openDetail(detail.id);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSending(false);
    }
  };

  const handleClose = async () => {
    if (!detail) return;
    setError('');
    try {
      const res = admin
        ? await api.adminCloseTicket(detail.id)
        : await api.closeTicket(detail.id);
      const updated = res?.data as TicketItem | undefined;
      if (updated) setDetail(updated);
      addToast(t('工单已关闭'));
      await openDetail(detail.id);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const q = query.trim().toLowerCase();
  const visibleTickets = q
    ? tickets.filter((tk) =>
        (tk.subject || '').toLowerCase().includes(q) || (tk.email || '').toLowerCase().includes(q))
    : tickets;

  // ── 详情视图 ──────────────────────────────────────────
  if (detail) {
    const closed = detail.status === 1;
    return (
      <div>
        <div className="page-header">
          <button className="tk-back" type="button" onClick={closeDetail}>
            <ChevronLeft size={16} />
            {t('返回')}
          </button>
          <h1>{detail.subject}</h1>
          <p>{t('工单')} #{String(detail.id).slice(0, 8)}</p>
        </div>

        {error && <div className="error-message">{error}</div>}

        <Card
          title={
            <div className="tk-meta">
              {statusBadge(t, detail.status)}
              {replyBadge(t, detail.reply_status)}
              <Badge tone={levelTone(detail.level)}>{levelText(t, detail.level)}</Badge>
            </div>
          }
          actions={<span className="tk-time">{t('最后更新')}: {fmtTime(detail.updated_at)}</span>}
        >
          <div className="tk-thread">
            {messages.length === 0 ? (
              <EmptyState message={t('暂无工单')} />
            ) : (
              messages.map((m) => (
                <div key={m.id} className={`tk-msg ${m.is_me ? 'tk-msg-me' : 'tk-msg-other'}`}>
                  <div className="tk-msg-head">
                    <span>{m.is_me ? t('我') : t('客服')}</span>
                    <span className="tk-msg-time">{fmtTime(m.created_at)}</span>
                  </div>
                  <div className="tk-msg-body">{m.message}</div>
                </div>
              ))
            )}
          </div>

          {!closed && (
            <div className="tk-reply-box">
              <Textarea
                placeholder={t('输入回复内容…')}
                value={replyText}
                onChange={(e) => setReplyText(e.target.value)}
                rows={3}
              />
              <div className="tk-reply-actions">
                <Button variant="outline" onClick={handleClose} disabled={sending}>
                  <X size={15} />
                  {t('关闭工单')}
                </Button>
                <Button onClick={handleReply} disabled={sending}>
                  <Send size={15} />
                  {sending ? t('发送中...') : t('发送回复')}
                </Button>
              </div>
            </div>
          )}
        </Card>
      </div>
    );
  }

  // ── 列表视图 ──────────────────────────────────────────
  return (
    <div>
      <div className="page-header">
        <h1>{admin ? t('全部工单') : t('我的工单')}</h1>
        <p>{t('提交工单，联系客服解决问题')}</p>
      </div>

      {error && <div className="error-message">{error}</div>}

      {!admin && (
        <Card bodyClassName="">
          <Button onClick={() => setShowCreate((v) => !v)}>
            <MessageSquarePlus size={15} />
            {showCreate ? t('取消') : t('提交工单')}
          </Button>
        </Card>
      )}

      {!admin && showCreate && (
        <Card title={t('提交工单')}>
          <form onSubmit={handleCreate}>
            <div className="tk-create-grid">
              <Input
                label={t('主题')}
                placeholder={t('请输入主题')}
                value={createForm.subject}
                onChange={(e) => setCreateForm({ ...createForm, subject: e.target.value })}
              />
              <Select
                label={t('优先级')}
                value={createForm.level}
                onChange={(e) => setCreateForm({ ...createForm, level: Number(e.target.value) })}
              >
                {LEVELS.map((l) => (
                  <option key={l} value={l}>{levelText(t, l)}</option>
                ))}
              </Select>
            </div>
            <Textarea
              label={t('工单内容')}
              placeholder={t('请输入内容')}
              value={createForm.message}
              onChange={(e) => setCreateForm({ ...createForm, message: e.target.value })}
              rows={5}
            />
            <div style={{ marginTop: 16 }}>
              <Button type="submit" disabled={creating}>
                {creating ? t('提交中...') : t('提交工单')}
              </Button>
            </div>
          </form>
        </Card>
      )}

      <Card
        title={`${admin ? t('全部工单') : t('我的工单')} (${total})`}
        actions={
          admin ? (
            <div className="tk-filters">
              <Input
                placeholder={t('搜索工单主题 / 提交人…')}
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                style={{ width: 200 }}
              />
              <Select
                value={statusFilter}
                onChange={(e) => { setStatusFilter(e.target.value); setPage(1); }}
                style={{ width: 120 }}
              >
                <option value="">{t('全部状态')}</option>
                <option value="0">{t('处理中')}</option>
                <option value="1">{t('已关闭')}</option>
              </Select>
              <Select
                value={replyFilter}
                onChange={(e) => { setReplyFilter(e.target.value); setPage(1); }}
                style={{ width: 130 }}
              >
                <option value="">{t('全部回复状态')}</option>
                <option value="0">{t('待客服回复')}</option>
                <option value="1">{t('待用户回复')}</option>
              </Select>
            </div>
          ) : undefined
        }
      >
        {loading ? (
          <SkeletonTable columns={admin ? 8 : 7} rows={6} />
        ) : visibleTickets.length === 0 ? (
          <EmptyState message={t('暂无工单')} icon="🎫" />
        ) : (
          <div className="table-wrapper">
            <table>
              <thead>
                <tr>
                  {admin && <th>{t('提交人')}</th>}
                  <th>{t('主题')}</th>
                  <th>{t('优先级')}</th>
                  <th>{t('状态')}</th>
                  <th>{t('回复状态')}</th>
                  <th>{t('提交时间')}</th>
                  <th>{t('最后更新')}</th>
                  <th>{t('操作')}</th>
                </tr>
              </thead>
              <tbody>
                {visibleTickets.map((tk) => (
                  <tr key={tk.id}>
                    {admin && <td>{tk.email || tk.user_id}</td>}
                    <td><strong>{tk.subject}</strong></td>
                    <td><Badge tone={levelTone(tk.level)}>{levelText(t, tk.level)}</Badge></td>
                    <td>{statusBadge(t, tk.status)}</td>
                    <td>{replyBadge(t, tk.reply_status)}</td>
                    <td>{fmtTime(tk.created_at)}</td>
                    <td>{fmtTime(tk.updated_at)}</td>
                    <td>
                      <Button variant="outline" size="sm" onClick={() => void openDetail(tk.id)}>
                        {t('展开详情')}
                      </Button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}

        {admin && !loading && !q && (
          <Pagination
            page={page}
            totalPages={Math.max(1, Math.ceil(total / size))}
            onChange={setPage}
          />
        )}
      </Card>
    </div>
  );
}