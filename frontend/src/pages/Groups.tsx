import React, { useState } from 'react';
import { api } from '../api';

interface GroupsProps {
  children?: React.ReactNode;
}

export default function Groups(): JSX.Element {
  const [groups, setGroups] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);

  const loadGroups = async () => {
    setLoading(true);
    try {
      const res = await api.listGroups();
      setGroups(res.data || []);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadGroups();
  }, []);

  return (
    <div>
      <div className="page-header">
        <h1>分组管理</h1>
        <p>管理用户分组和权限</p>
      </div>

      {/* 分组列表 */}
      <div className="groups-list">
        {loading ? (
          <div className="loading">加载中...</div>
        ) : groups.length > 0 ? (
          groups.map((group) => (
            <div key={group.id} className="group-item">
              <h3>{group.name}</h3>
              <p>{group.description}</p>
              <button onClick={() => {/* 编辑分组 */}}>编辑</button>
              <button onClick={() => {/* 删除分组 */}}>删除</button>
            </div>
          ))
        ) : (
          <div className="empty-state">
            <p>暂无分组</p>
            <button onClick={() => {/* 创建分组 */}}>
              + 创建分组
            </button>
          </div>
        )}
      </div>
    </div>
  );
}