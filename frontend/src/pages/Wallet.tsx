import { useState, useEffect } from 'react';
import { api } from '../api';

export default function Wallet(): JSX.Element {
  const [balance, setBalance] = useState<number>(0);
  const [transactions, setTransactions] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);

  const loadBalance = async () => {
    setLoading(true);
    try {
      const res = await api.getBalance();
      setBalance(res.data?.balance || 0);
    } finally {
      setLoading(false);
    }
  };

  const loadTransactions = async () => {
    setLoading(true);
    try {
      const res = await api.getTransactions();
      setTransactions(res.data || []);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void loadBalance();
    void loadTransactions();
  }, []);

  return (
    <div>
      <div className="page-header">
        <h1>钱包管理</h1>
        <p>查看钱包余额和交易记录</p>
      </div>

      {/* 钱包余额 */}
      <div className="balance-card">
        <h2>当前余额</h2>
        <p>¥{balance.toFixed(2)}</p>
        <button onClick={() => {/* 充值逻辑 */}}>
          充值
        </button>
      </div>

      {/* 交易记录 */}
      <div className="transactions-list">
        <h2>交易记录</h2>
        {loading ? (
          <div className="loading">加载中...</div>
        ) : transactions.length > 0 ? (
          transactions.map((transaction) => (
            <div key={transaction.id} className="transaction-item">
              <span>{transaction.description}</span>
              <span className={`transaction-amount transaction-amount-${transaction.type}`}>
                {transaction.type === 'credit' ? '+' : '-'}¥{transaction.amount}
              </span>
            </div>
          ))
        ) : (
          <p>暂无交易记录</p>
        )}
      </div>
    </div>
  );
}
