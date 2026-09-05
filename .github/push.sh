#!/bin/bash
# AIGX GitHub Actions 推送脚本
# 用于推送工作流文件到 GitHub 以自动编译

set -e

echo "================================================"
echo "AIGX GitHub Actions 自动编译推送工具"
echo "================================================"
echo ""
echo "推送 GitHub Actions 工作流文件到 GitHub"
echo "将在 GitHub 上自动触发多平台编译任务"
echo ""

# GitHub Token
GITHUB_TOKEN="GITHUB_TOKEN_PLACEHOLDER"
GITHUB_TOKEN="GITHUB_TOKEN_PLACEHOLDER"

# 询问用户信息
echo "请输入您的 GitHub 用户名:"
read USERNAME

# 配置远程仓库
REPO_URL="https://${GITHUB_TOKEN}@github.com/${USERNAME}/aigx.git"

echo ""
echo "准备推送配置..."
echo "仓库: $USERNAME/aigx"
echo "Token: ***${GITHUB_TOKEN:0:7}***"
echo ""

# 创建目录结构
if [ ! -d ".github" ]; then
    echo "创建 .github 目录..."
    mkdir -p .github/workflows
fi

# 检查工作流文件
WORKFLOW_FILE=".github/workflows/github-actions.yml"
if [ ! -f "$WORKFLOW_FILE" ]; then
    echo "❌ 工作流文件不存在: .github/workflows/github-actions.yml"
    echo "请手动创建此文件"
    exit 1
fi

# 添加文件
echo "添加工作流文件..."
git add .github/

# 提交更改
COMMIT_MSG="ci: 添加 GitHub Actions 多平台编译工作流

- Linux AMD64 构建支持
- Linux ARM64 构建支持
- Windows AMD64 构建支持
- 自动创建测试版 Release

推送后将在 GitHub 上自动执行编译任务"
git commit -m "$COMMIT_MSG"

# 推送到 GitHub
echo ""
echo "🚀 开始推送到 GitHub..."
if git push origin main; then
    echo ""
    echo "================================================"
    echo "✅ 推送成功！GitHub Actions 已启动"
    echo "================================================"
    echo ""
    echo "📊 请查看编译进度："
    echo "   https://github.com/$USERNAME/aigx/actions"
    echo ""
    echo "✅ 全部编译成功后，运行以下命令创建正式版："
    echo "   git tag v1.0.0 && git push origin v1.0.0"
    echo ""
    echo "💡 未来推送任何更改到 main 分支都将自动触发编译"
    echo ""
else
    echo ""
    echo "❌ 推送失败，检查配置后重试"
    exit 1
fi