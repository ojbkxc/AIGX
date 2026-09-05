import { useState } from 'react';
import { api } from '../api';

export default function Epay(): JSX.Element {
  const [config, setConfig] = useState<EpayConfig>({
    app_id: '',
    secret: '',
    notify_url: '',
    return_url: '',
  });
  const [loading, setLoading] = useState(false);

  const handleSave = async () => {
    setLoading(true);
    try {
      await api.saveEpayConfig(config);
      alert('保存成功！');
    } catch (err) {
      console.error('保存失败:', err);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div>
      <div className="page-header">
        <h1>易支付配置</h1>
        <p>配置线上支付接口</p>
      </div>

      <div className="settings-section">
        <h2>API 配置</h2>
        <div className="form-group">
          <label>商户 ID</label>
          <input
            type="text"
            value={config.app_id}
            onChange={(e) => setConfig({ ...config, app_id: e.target.value })}
            placeholder="输入易支付商户 ID"
          />
        </div>
        <div className="form-group">
          <label>API 密钥</label>
          <input
            type="password"
            value={config.secret}
            onChange={(e) => setConfig({ ...config, secret: e.target.value })}
            placeholder="输入 API 密钥"
          />
        </div>
        <div className="form-group">
          <label>异步通知地址</label>
          <input
            type="url"
            value={config.notify_url}
            onChange={(e) => setConfig({ ...config, notify_url: e.target.value })}
            placeholder="https://yourdomain.com/api/epay/notify"
          />
        </div>
        <div className="form-group">
          <label>同步跳转地址</label>
          <input
            type="url"
            value={config.return_url}
            onChange={(e) => setConfig({ ...config, return_url: e.target.value })}
            placeholder="https://yourdomain.com/epay/return"
          />
        </div>
      </div>

      <button onClick={handleSave} disabled={loading}>
        {loading ? '保存中...' : '保存配置'}
      </button>
    </div>
  );
}

interface EpayConfig {
  app_id: string;
  secret: string;
  notify_url: string;
  return_url: string;
}
