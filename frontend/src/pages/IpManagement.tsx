import { useState, useEffect } from 'react';
import { api } from '../api';

export default function IpManagement(): JSX.Element {
  const [whitelist, setWhitelist] = useState<string[]>([]);
  const [blacklist, setBlacklist] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [whiteInput, setWhiteInput] = useState('');
  const [blackInput, setBlackInput] = useState('');

  const loadIpLists = async () => {
    setLoading(true);
    try {
      const res = await api.getIpLists();
      setWhitelist(res.data?.whitelist || []);
      setBlacklist(res.data?.blacklist || []);
    } finally {
      setLoading(false);
    }
  };

  const addToWhitelist = async (ip: string) => {
    try {
      await api.addIpWhitelist(ip);
      loadIpLists();
      alert(`已添加 ${ip} 到白名单`);
    } catch (err) {
      console.error('添加失败:', err);
    }
  };

  const addToBlacklist = async (ip: string) => {
    try {
      await api.addIpBlacklist(ip);
      loadIpLists();
      alert(`已添加 ${ip} 到黑名单`);
    } catch (err) {
      console.error('添加失败:', err);
    }
  };

  const removeFromWhitelist = async (pattern: string) => {
    try {
      await api.removeIpWhitelist(pattern);
      loadIpLists();
      alert('已从白名单移除');
    } catch (err) {
      console.error('删除失败:', err);
    }
  };

  const removeFromBlacklist = async (pattern: string) => {
    try {
      await api.removeIpBlacklist(pattern);
      loadIpLists();
      alert('已从黑名单移除');
    } catch (err) {
      console.error('删除失败:', err);
    }
  };

  useEffect(() => {
    void loadIpLists();
  }, []);

  return (
    <div>
      <div className="page-header">
        <h1>IP 过滤管理</h1>
        <p>管理 IP 白名单和黑名单</p>
      </div>

      {/* 白名单 */}
      <div className="ip-section">
        <h2>IP 白名单</h2>
        <div className="ip-form">
          <input
            type="text"
            placeholder="输入 IP 地址或端口范围（如 192.168.1.1 或 :8080）"
            value={whiteInput}
            onChange={(e) => setWhiteInput(e.target.value)}
          />
          <button onClick={() => void addToWhitelist(whiteInput)}>添加到白名单</button>
        </div>
        <div className="ip-list">
          {loading ? (
            <div className="loading">加载中...</div>
          ) : whitelist.length > 0 ? (
            whitelist.map((ip, index) => (
              <div key={index} className="ip-item">
                <span>{ip}</span>
                <button onClick={() => removeFromWhitelist(ip)}>删除</button>
              </div>
            ))
          ) : (
            <p>白列表为空</p>
          )}
        </div>
      </div>

      {/* 黑名单 */}
      <div className="ip-section">
        <h2>IP 黑名单</h2>
        <div className="ip-form">
          <input
            type="text"
            placeholder="输入 IP 地址或端口范围"
            value={blackInput}
            onChange={(e) => setBlackInput(e.target.value)}
          />
          <button onClick={() => void addToBlacklist(blackInput)}>添加到黑名单</button>
        </div>
        <div className="ip-list">
          {loading ? (
            <div className="loading">加载中...</div>
          ) : blacklist.length > 0 ? (
            blacklist.map((ip, index) => (
              <div key={index} className="ip-item">
                <span>{ip}</span>
                <button onClick={() => removeFromBlacklist(ip)}>删除</button>
              </div>
            ))
          ) : (
            <p>黑列表为空</p>
          )}
        </div>
      </div>
    </div>
  );
}
