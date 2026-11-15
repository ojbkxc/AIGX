import { useState, useEffect, useRef, type ChangeEvent, type FormEvent } from 'react';
import { useNavigate, Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { SUPPORTED_LANGS, setLanguage } from '../i18n';
import { Eye, EyeOff } from 'lucide-react';
import { api, type RegisterOptions } from '../api';
import { cycleTheme } from '../lib/theme';
import { useAltcha } from '../hooks/useAltcha';
import AltchaWidget from '../components/AltchaWidget';
import './Register.css';

type UsernameStatus = 'idle' | 'checking' | 'available' | 'taken' | 'error';

interface UsernameCheckResponse {
  available?: boolean;
  exists?: boolean;
  data?: { available?: boolean; exists?: boolean };
}

interface RegisterResponse {
  success?: boolean;
  error?: string;
}

export default function Register() {
  const [email, setEmail] = useState('');
  // 白名单启用时：邮箱拆成「本地部分 + 域名下拉」（域名含 @ 前缀，如 @163.com）
  const [localPart, setLocalPart] = useState('');
  const [selectedDomain, setSelectedDomain] = useState('');
  const [options, setOptions] = useState<RegisterOptions | null>(null);
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [confirmPassword, setConfirmPassword] = useState('');
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const navigate = useNavigate();
  const { t, i18n } = useTranslation();

  // 机器人校验（ALTCHA）：提交时带上答案，失败后必须重新领题（服务端用后即焚）
  const altcha = useAltcha();

  // 邀请码：从 ?aff= 或 ?aff_code= 查询参数读入（new-api 前端从 localStorage 读）
  const [affCode, setAffCode] = useState('');
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const aff = params.get('aff') || params.get('aff_code') || '';
    if (aff) setAffCode(aff);
  }, []);

  // 注册选项（公共端点）：注册开关 / 邮箱白名单 / 机器人校验。
  // 拉取失败时按「注册开放、单邮箱输入框」降级，避免整页不可用。
  useEffect(() => {
    let cancelled = false;
    api.getRegisterOptions()
      .then((res) => {
        if (!cancelled && res?.success && res.data) setOptions(res.data);
      })
      .catch(() => {
        // 端点不可用（如旧后端）时保持默认：注册开放 + 单邮箱输入框
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // 白名单启用：邮箱拆为「本地部分 + 域名下拉」
  const whitelistEnabled = options?.email_whitelist_enable === true;
  // 注册开关：未取到选项前默认视为开放（避免首屏误判为关闭）
  const registerClosed = options !== null && !options.register_enabled;
  // 机器人校验开关（options 未取到时沿用旧行为：由 ALTCHA 自身状态兜底）
  const captchaRequired = options?.captcha_required === true;
  const emailSuffixes = options?.email_whitelist_suffix ?? [];
  // 默认选中的域名（下拉首项）。单独取出来当 effect 依赖：emailSuffixes 每次
  // 渲染都是新数组（`?? []`），直接依赖会让 effect 每帧都跑。
  const defaultDomain = emailSuffixes.length > 0 ? `@${emailSuffixes[0]}` : '';

  // 默认选中第一项域名：多数人只有一个可选域名，让用户只需填本地部分。
  // 只在「还没选」时补默认 —— 用户手动选过别的域名后不能被后续渲染冲掉。
  useEffect(() => {
    if (!whitelistEnabled || defaultDomain === '') return;
    if (selectedDomain === '') setSelectedDomain(defaultDomain);
  }, [whitelistEnabled, defaultDomain, selectedDomain]);

  // ── 用户名实时可用性检查（防抖 300ms）──
  // 状态：'idle' 未检查 / 'checking' 检查中 / 'available' 可用 / 'taken' 已占用 / 'error' 检查失败
  const [usernameStatus, setUsernameStatus] = useState<UsernameStatus>('idle');
  const usernameTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const usernameCheckedRef = useRef('');

  // 防抖检查用户名是否可用，300ms 内若再次输入则取消上次未发出的请求
  const checkUsernameAvailability = (value: string) => {
    if (usernameTimerRef.current) {
      clearTimeout(usernameTimerRef.current);
    }
    if (!value.trim()) {
      setUsernameStatus('idle');
      return;
    }
    setUsernameStatus('checking');
    usernameTimerRef.current = setTimeout(async () => {
      try {
        const res = (await api.checkUsername(value.trim())) as UsernameCheckResponse;
        // 后端返回 { available: true/false } 或 { data: { available: ... } }
        const data = res?.data ?? res;
        const available = data?.available ?? !data?.exists;
        setUsernameStatus(available ? 'available' : 'taken');
        usernameCheckedRef.current = value.trim();
      } catch {
        setUsernameStatus('error');
      }
    }, 300);
  };

  const handleUsernameChange = (e: ChangeEvent<HTMLInputElement>) => {
    const value = e.target.value;
    setUsername(value);
    checkUsernameAvailability(value);
  };

  // 渲染用户名输入框右侧的状态提示
  const renderUsernameHint = () => {
    if (!username.trim() || usernameStatus === 'idle') {
      return null;
    }
    if (usernameStatus === 'checking') {
      return (
        <span style={{ fontSize: 12, color: 'var(--text-muted)', marginLeft: 6 }}>
          {t('检查中...')}
        </span>
      );
    }
    if (usernameStatus === 'available') {
      return (
        <span style={{ fontSize: 12, color: 'rgb(34,197,94)', marginLeft: 6 }}>
          ✓ {t('可用')}
        </span>
      );
    }
    if (usernameStatus === 'taken') {
      return (
        <span style={{ fontSize: 12, color: 'rgb(239,68,68)', marginLeft: 6 }}>
          ✗ {t('已占用')}
        </span>
      );
    }
    return (
      <span style={{ fontSize: 12, color: 'var(--text-muted)', marginLeft: 6 }}>
        {t('检查失败')}
      </span>
    );
  };

  // 组件卸载时清理防抖定时器
  useEffect(() => {
    return () => {
      if (usernameTimerRef.current) clearTimeout(usernameTimerRef.current);
    };
  }, []);

  const handleSubmit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    setError('');

    if (registerClosed) {
      setError(t('注册已关闭'));
      return;
    }
    // 白名单启用：本地部分非空 + 已选域名，拼成完整邮箱（域名已含 @ 前缀）
    if (whitelistEnabled && (!localPart.trim() || !selectedDomain)) {
      setError(t('请填写本地部分和选择域名'));
      return;
    }
    const finalEmail = whitelistEnabled ? `${localPart.trim()}${selectedDomain}` : email;
    if (!finalEmail || !password) {
      setError(t('请填写邮箱和密码'));
      return;
    }
    if (password.length < 6) {
      setError(t('密码长度至少6位'));
      return;
    }
    if (password !== confirmPassword) {
      setError(t('两次密码输入不一致'));
      return;
    }
    if (altcha.state === 'error') {
      setError(t('机器人校验失败，请点击校验块重试'));
      return;
    }
    if (altcha.state === 'verifying') {
      setError(t('正在完成机器人校验，请稍候…'));
      return;
    }
    // 服务端要求机器人校验但当前未拿到答案（如开关中途开启）：给出明确提示
    if (captchaRequired && altcha.state !== 'verified') {
      setError(t('需要完成机器人校验'));
      return;
    }

    setLoading(true);
    try {
      const res = (await api.register(finalEmail, password, username || undefined, affCode || undefined, altcha.payload ?? undefined)) as RegisterResponse;
      if (res.success) {
        // 注册成功后自动跳转登录页
        navigate('/login', { state: { registered: true, email: finalEmail } });
      } else {
        setError(res.error || t('注册失败'));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : t('注册失败'));
    } finally {
      setLoading(false);
      // 服务端校验题用后即焚：每次提交后都重新领题
      altcha.reset();
    }
  };

  const toggleTheme = (): void => {
    void cycleTheme();
  };

  // 语言切换：循环 6 种语言（与登录页一致）
  const toggleLanguage = (): void => {
    const idx = SUPPORTED_LANGS.findIndex((l) => l.code === (i18n.language || 'zh'));
    const next = SUPPORTED_LANGS[(idx + 1) % SUPPORTED_LANGS.length].code;
    setLanguage(next);
  };

  return (
    <div style={{
      minHeight: '100vh',
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'center',
      background: 'var(--bg-color)',
      padding: '16px',
      position: 'relative',
      overflow: 'hidden',
    }}>

      {/* 右上角工具组：语言切换 + 主题切换 */}
      <div style={{
        position: 'fixed',
        top: '20px',
        right: '20px',
        display: 'flex',
        gap: '8px',
        zIndex: 1000,
      }}>
        <button
          onClick={toggleLanguage}
          style={{
            width: '44px',
            height: '44px',
            borderRadius: '12px',
            background: 'var(--card-bg)',
            border: '1px solid var(--border-color)',
            color: 'var(--text-main)',
            cursor: 'pointer',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            boxShadow: 'var(--card-shadow)',
            backdropFilter: 'blur(var(--glass-blur))',
            transition: 'all 0.3s cubic-bezier(0.16, 1, 0.3, 1)',
            fontSize: '13px',
            fontWeight: 600,
          }}
          title={t('切换语言')}
        >
          {(() => {
            const idx = SUPPORTED_LANGS.findIndex((l) => l.code === (i18n.language || 'zh'));
            return SUPPORTED_LANGS[(idx + 1) % SUPPORTED_LANGS.length].label;
          })()}
        </button>
        <button
          onClick={toggleTheme}
          style={{
            width: '44px',
            height: '44px',
            borderRadius: '12px',
            background: 'var(--card-bg)',
            border: '1px solid var(--border-color)',
            color: 'var(--text-main)',
            cursor: 'pointer',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            boxShadow: 'var(--card-shadow)',
            backdropFilter: 'blur(var(--glass-blur))',
            zIndex: 1000,
            transition: 'all 0.3s cubic-bezier(0.16, 1, 0.3, 1)',
          }}
          title={t('切换主题')}
        >
          <svg xmlns="http://www.w3.org/2000/svg" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" style={{ width: '20px', height: '20px' }}>
            <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
          </svg>
        </button>
      </div>

      <div style={{
        background: 'var(--card-bg)',
        border: '1px solid var(--border-color)',
        borderRadius: '16px',
        padding: '32px',
        width: '100%',
        maxWidth: '380px',
        boxShadow: 'var(--card-shadow)',
        backdropFilter: 'blur(var(--glass-blur))',
        WebkitBackdropFilter: 'blur(var(--glass-blur))',
        animation: 'fadeIn 0.25s ease',
      }}>
        <div style={{ textAlign: 'center', marginBottom: '22px' }}>
          <div style={{
            width: '44px',
            height: '44px',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            margin: '0 auto 14px',
          }}>
            <img src="/logo.png" alt="AIGX" width={44} height={44} />
          </div>
          <h1 style={{ fontSize: '20px', fontWeight: 700, color: 'var(--text-main)', marginBottom: '4px', fontFamily: "'Inter', sans-serif" }}>
            {t('创建账号')}
          </h1>
          <p style={{ fontSize: '13px', color: 'var(--text-muted)' }}>
            {t('注册 AIGX Gateway')}
          </p>
        </div>

        {error && (
          <div className="error-message">{error}</div>
        )}

        {/* 机器人校验：挂载即自动求解，真人不需操作；失败可点击重试 */}
        <AltchaWidget state={altcha.state} onRetry={altcha.reset} />

        <form onSubmit={handleSubmit} style={{ display: 'flex', flexDirection: 'column', gap: '4px' }}>
          {/* 注册已关闭（register_enabled=false）：提示 + 表单整体禁用 */}
          {registerClosed && (
            <div className="register-closed-notice">{t('注册已关闭')}</div>
          )}
          <div className="form-group">
            <label htmlFor={whitelistEnabled ? 'emailLocal' : 'email'}>{t('邮箱 *')}</label>
            {whitelistEnabled ? (
              // 白名单启用：只填本地部分（如 143），域名从下拉选择（如 @163.com）
              <div className="email-split">
                <input
                  id="emailLocal"
                  type="text"
                  className="form-input email-split-local"
                  placeholder={t('本地部分')}
                  value={localPart}
                  onChange={(e) => setLocalPart(e.target.value.replace(/@/g, ''))}
                  autoFocus
                  disabled={loading || registerClosed}
                  autoComplete="off"
                  spellCheck={false}
                />
                <select
                  id="emailDomain"
                  className="form-input email-split-domain"
                  value={selectedDomain}
                  onChange={(e) => setSelectedDomain(e.target.value)}
                  disabled={loading || registerClosed}
                  aria-label={t('域名')}
                >
                  {/* 列表为空时（白名单开启但没配域名 = 没人能注册）保留空占位；
                      非空时首项在下面的 effect 里被默认选中，不渲染「请选择域名」 */}
                  {emailSuffixes.length === 0 && (
                    <option value="">{t('请选择域名')}</option>
                  )}
                  {emailSuffixes.map((suffix) => (
                    <option key={suffix} value={`@${suffix}`}>{`@${suffix}`}</option>
                  ))}
                </select>
              </div>
            ) : (
              <input
                id="email"
                type="email"
                className="form-input"
                placeholder={t('请输入邮箱')}
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                autoFocus
                disabled={loading || registerClosed}
              />
            )}
            {whitelistEnabled && (
              <span className="form-hint">{t('邮箱后缀由白名单限定')}</span>
            )}
          </div>
          <div className="form-group">
            <label htmlFor="username">{t('昵称（可选）')}</label>
            <input
              id="username"
              type="text"
              className="form-input"
              placeholder={t('设置一个昵称（选填）')}
              value={username}
              onChange={handleUsernameChange}
              disabled={loading || registerClosed}
            />
            {renderUsernameHint()}
          </div>
          <div className="form-group">
            <label htmlFor="password">{t('密码 *')}</label>
            <div className="password-input-wrap">
              <input
                id="password"
                type={showPassword ? 'text' : 'password'}
                className="form-input password-input"
                placeholder={t('至少6位密码')}
                autoComplete="new-password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={loading || registerClosed}
              />
              <button
                type="button"
                className="password-input-toggle"
                tabIndex={-1}
                onClick={() => setShowPassword((v) => !v)}
                aria-label={showPassword ? t('隐藏密码') : t('显示密码')}
                title={showPassword ? t('隐藏密码') : t('显示密码')}
              >
                {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
              </button>
            </div>
          </div>
          <div className="form-group">
            <label htmlFor="confirmPassword">{t('确认密码 *')}</label>
            <input
              id="confirmPassword"
              type={showPassword ? 'text' : 'password'}
              className="form-input"
              placeholder={t('再次输入密码')}
              autoComplete="new-password"
              value={confirmPassword}
              onChange={(e) => setConfirmPassword(e.target.value)}
              disabled={loading || registerClosed}
            />
          </div>
          <div className="form-group">
            <label htmlFor="affCode">{t('邀请码（可选）')}</label>
            <input
              id="affCode"
              type="text"
              className="form-input"
              placeholder={t('填写邀请码，双方均可获得奖励')}
              value={affCode}
              onChange={(e) => setAffCode(e.target.value.trim())}
              disabled={loading || registerClosed}
              style={{ fontFamily: 'monospace', letterSpacing: 1 }}
            />
          </div>
          <button
            type="submit"
            className="btn btn-primary"
            disabled={loading || registerClosed}
            style={{ width: '100%', justifyContent: 'center', padding: '9px', marginTop: '8px', fontSize: '13px' }}
          >
            {loading ? t('注册中...') : t('注册')}
          </button>
        </form>

        <div style={{ textAlign: 'center', marginTop: '16px' }}>
          <Link to="/login" style={{ fontSize: '13px', color: 'var(--accent-color)', textDecoration: 'none' }}>
            {t('已有账号？立即登录')}
          </Link>
        </div>
      </div>
    </div>
  );
}
