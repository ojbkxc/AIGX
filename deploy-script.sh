#!/bin/bash
# AIGX 部署脚本 - 使用 GitHub token 推送代码

echo "================================================"
echo "AIGX 自动部署脚本"
echo "================================================"
echo ""

# GitHub Token
TOKEN="GITHUB_TOKEN_PLACEHOLDER"

# 用户名和仓库名
USERNAME="${AIGX_USERNAME:-YOUR_USERNAME}"
REPO="${AIGX_REPO:-YOUR_USERNAME/aigx}"
BRANCH="${AIGX_BRANCH:-main}"

# 检查参数
if [ "$USERNAME" = "YOUR_USERNAME" ]; then
    echo "❌ 请先设置用户名:"
    echo "   export AIGX_USERNAME=yourname"
    echo "   export AIGX_REPO=yourname/aigx"
    echo ""
    echo "或者直接使用命令:"
    echo "   export AIGX_USERNAME=你的用户名"
    echo "   export AIGX_REPO=你的用户名/aigx"
    exit 1
fi

# 完整仓库 URL
REPO_URL="https://${TOKEN}@github.com/${REPO}.git"

echo "构建信息:"
echo "  用户: $USERNAME"
echo "  仓库: $REPO"
echo "  分支: $BRANCH"
echo "  URL: $REPO_URL"
echo ""

# 检查 git
if ! command -v git &> /dev/null; then
    echo "❌ Git 未安装"
    exit 1
fi

echo "检查仓库状态..."
if [ -d ".git" ]; then
    echo "✅ Git 仓库已存在"
    CURRENT_BRANCH=$(git branch --show-current)
    if [ "$CURRENT_BRANCH" != "$BRANCH" ]; then
        echo "🔄 切换到分支: $BRANCH"
        git checkout $BRANCH

        # 拉取最新代码
        echo "📥 拉取最新代码..."
        git pull origin $BRANCH || echo "提示: 可能是首次推送，可以跳过此步骤"
    fi
else
    echo "🆕 初始化 Git 仓库..."
    git init
fi

# 添加文件
echo "📦 添加文件..."
git add .

# 创建提交
echo "📝 创建提交..."
COMMIT_MSG="feat: 完成 AIGX 多平台构建系统！支持 Linux/Windows/macOS 全平台构建和部署

- 多平台二进制文件自动构建
- Docker 多平台镜像支持
- GitHub Actions CI/CD 自动化
- 完整的部署文档和配置
- 前端 React 应用和后端 Rust 服务
- 监控和告警系统"

git commit -m "$COMMIT_MSG"

# 添加远程仓库
echo "🔗 配置远程仓库..."
git remote add origin "$REPO_URL"

# 推送代码
echo ""
echo "🚀 开始推送到 GitHub..."
if git push -u origin $BRANCH; then
    echo ""
    echo "================================================"
    echo "✅ 部署成功！"
    echo "================================================"
    echo ""
    echo "仓库地址: https://github.com/${REPO}"
    echo "查看编译状态: https://github.com/${REPO}/actions"
    echo "查看发布: https://github.com/${REPO}/releases"
    echo ""
    echo "重要提醒:"
    echo "1. 确保 GitHub 仓库可见性设置为 Public 或在仓库设置中添加 GITHUB_TOKEN"
    echo "2. 生成 tags 触发完整编译: git tag v2.0.0 && git push origin v2.0.0"
    echo ""
else
    echo ""
    echo "❌ 推送失败，错误信息:"
    git push origin $BRANCH
    exit 1
fi