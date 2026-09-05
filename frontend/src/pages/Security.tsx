import React, { useState } from 'react';
import { api } from '../api';

interface SecurityProps {
  children?: React.ReactNode;
}

export default function Security(): JSX.Element {
  const [incidents, setIncidents] = useState<any[]>([]);
  const [alerts, setAlerts] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    loadSecurityData();
  }, []);

  const loadSecurityData = async () => {
    setLoading(true);
    try {
      const [incidentsData, alertsData] = await Promise.all([
        api.getSecurityIncidents().catch(() => []),
        api.getSecurityAlerts().catch(() => []),
      ]);
      setIncidents(incidentsData);
      setAlerts(alertsData);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div>
      <div className="page-header">
        <h1>安全监控</h1>
        <p>监控安全事件和风险</p>
      </div>

      {/* 安全事件 */}
      <div className="security-section">
        <h2>安全事件</h2>
        {loading ? (
          <div className="loading">加载中...</div>
        ) : incidents.length > 0 ? (
          incidents.map((incident, index) => (
            <div key={index} className="security-incident">
              <span className="incident-severity">{incident.severity}</span>
              <span className="incident-message">{incident.message}</span>
              <span className="incident-time">{incident.timestamp}</span>
            </div>
          ))
        ) : (
          <p>暂无安全事件</p>
        )}
      </div>

      {/* 安全告警 */}
      <div className="security-section">
        <h2>安全告警</h2>
        {loading ? (
          <div className="loading">加载中...</div>
        ) : alerts.length > 0 ? (
          alerts.map((alert, index) => (
            <div key={index} className="security-alert">
              <span className="alert-type">{alert.type}</span>
              <span className="alert-message">{alert.message}</span>
              <span className="alert-time">{alert.timestamp}</span>
            </div>
          ))
        ) : (
          <p>暂无安全告警</p>
        )}
      </div>
    </div>
  );
}