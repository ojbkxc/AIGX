// 插件管理页入口。
//
// 形态已按 Agent2API v3.5.0-beta.2 重做（双源结构 + 来源/状态筛选 + 搜索 +
// 本地安装 + 补全缺失插件 + 缺失横幅），实现见
// `components/plugins/PluginManagerPage.tsx`。本文件只保留路由入口与
// 既有样式表引入（卡片/网格/进度条的类名在 ./Plugins.css）。
import './Plugins.css';
export { default } from '../components/plugins/PluginManagerPage';
