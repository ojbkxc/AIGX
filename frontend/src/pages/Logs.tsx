import { useState } from 'react';
import { api } from '../api';

export default function Logs(): JSX.Element {
  const [logs, setLogs] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);
  const [selectedLog, setSelectedLog] = useState<any | null>(null);

  const loadLogs = async () => {
    setLoading(true);
    try {
      const res = await api.getRequestLogs();
      setLogs(res.data || []);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div>
      <div className="page-header">
        <h1>请求日志</h1>
        <p>查看所有 API 请求日志</p>
      </div>

      {/* 日志列表 */}
      <div className="logs-list">
        {loading ? (
          <div className="loading">加载中...</div>
        ) : logs.length > 0 ? (
          logs.map((log) => (
            <div key={log.id} className="log-item">
              <div className="log-header">
                <span className="log-status">{log.status}</span>
                <span>{log.method} {log.path}</span>
                <span>{log.timestamp}</span>
              </div>
              <div className="log-details">
                <Button onClick={() => setSelectedLog(log)}>
                  查看详情
                </Button>
              </div>
            </div>
          ))
        ) : (
          <p>暂无日志</p>
        )}
      </div>

      {/* 日志详情 */}
      {selectedLog && (
        <div className="log-detail">
          <h2>日志详情</h2>
          <pre>{JSON.stringify(selectedLog, null, 2)}</pre>
          <button onClick={() => setSelectedLog(null)}>关闭</button>
        </div>
      )}
    </div>
  );
}
