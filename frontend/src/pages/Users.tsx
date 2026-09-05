import React, { useState } from 'react';
import type { User } from '../types';

interface UsersProps {
  children?: React.ReactNode;
}

export default function Users(): JSX.Element {
  // 状态定义
  const [users, setUsers] = useState<User[]>([]);
  const [loading, setLoading] = useState(true);

  return (
    <div>
      <div className="page-header">
        <h1>用户管理</h1>
        <p>管理系统用户和权限</p>
      </div>

      {/* 用户列表 */}
      <div className="users-list">
        {loading ? (
          <div className="loading">加载用户中...</div>
        ) : users.length > 0 ? (
          users.map((user) => (
            <div key={user.id} className="user-item">
              <h3>{user.username}</h3>
              <p>{user.email}</p>
              <span className={`user-role user-role-${user.role}`}>
                {user.role}
              </span>
            </div>
          ))
        ) : (
          <div className="empty-state">
            <p>暂无用户</p>
          </div>
        )}
      </div>
    </div>
  );
}