import { useState } from 'react';
import { api } from '../api';

export default function Mappings(): JSX.Element {
  const [mappings, setMappings] = useState<any[]>([]);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    loadMappings();
  }, []);

  const loadMappings = async () => {
    setLoading(true);
    try {
      const res = await api.getModelMappings();
      setMappings(res.data || []);
    } catch (err) {
      console.error('加载映射失败:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleSave = async (mapping: any) => {
    try {
      await api.saveModelMapping(mapping);
      loadMappings();
      alert('保存成功！');
    } catch (err) {
      console.error('保存失败:', err);
    }
  };

  const handleDelete = async (id: string) => {
    if (!confirm('确定要删除这个映射吗？')) return;

    try {
      await api.deleteModelMapping(id);
      loadMappings();
    } catch (err) {
      console.error('删除失败:', err);
    }
  };

  return (
    <div>
      <div className="page-header">
        <h1>模型映射</h1>
        <p>配置模型路由和加速</p>
      </div>

      {/* 映射列表 */}
      <div className="mappings-list">
        {loading ? (
          <div className="loading">加载中...</div>
        ) : mappings.length > 0 ? (
          mappings.map((mapping) => (
            <div key={mapping.id} className="mapping-item">
              <div className="mapping-source">{mapping.source_model}</div>
              <div className="mapping-target">{mapping.target_model}</div>
              <div className="mapping-actions">
                <button onClick={() => handleSave(mapping)}>编辑</button>
                <button onClick={() => handleDelete(mapping.id)}>删除</button>
              </div>
            </div>
          ))
        ) : (
          <p>暂无映射配置</p>
        )}
      </div>

      {/* 添加映射 */}
      <div className="mapping-form">
        <h3>添加映射</h3>
        <div className="form-group">
          <label>源模型</label>
          <input
            type="text"
            placeholder="如: gpt-3.5-turbo"
          />
        </div>
        <div className="form-group">
          <label>目标模型</label>
          <input
            type="text"
            placeholder="如: openai/gpt-3.5-turbo"
          />
        </div>
        <button onClick={() => {/* 保存映射 */}}>添加映射</button>
      </div>
    </div>
  );
}
