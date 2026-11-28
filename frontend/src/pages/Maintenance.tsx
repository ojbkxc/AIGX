import { useTranslation } from 'react-i18next';
import PageHeader from '../components/PageHeader';
import MaintenancePanel from '../components/MaintenancePanel';

/**
 * 顶级「自动维护」页（对齐上游 `ui/app.js` 的自动维护入口）。
 *
 * 页壳 + 维护面板：概览（4 个维护任务）、执行记录、逐项明细、失败重试，以及
 * 页内的「维护设置」弹窗。页面自动刷新（日志/请求日志/报表）不在这里——按
 * beta.7 口径已下沉到各页面工具栏（见 `components/PageAutoRefreshControl.tsx`）。
 */
export default function Maintenance(): JSX.Element {
  const { t } = useTranslation();
  return (
    <div>
      <PageHeader
        forceLeftWrapper
        title={t('自动维护')}
        subtitle={t('查看凭证、模型目录、软件与插件更新的每次维护结果，失败和跳过都有原因')}
      />
      <MaintenancePanel />
    </div>
  );
}