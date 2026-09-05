#!/bin/bash
# AIGX GitHub Actions 自动推送到 ojbkxc/AIGX 仓库

set -e

echo "================================================"
echo "AIGX 自动推送到 GitHub 编译算符"
echo "==============================================="
echo ""
echo "推送目标: https://github.com/ojbkxc/AIGX"
echo ""
echo "推送后将在 GitHub 上自动编译所有平台"
echo ""

# GitHub Token
GITHUB_TOKEN="GITHUB_TOKEN_PLACEHOLDER"

# 检查 Git
if ! command -v git &> /dev/null; then
    echo "❌ Git 未安装"
    exit 1
fi

# 检查是否有 .github 目录
if [ ! -d ".github" ]; then
    echo "❌ .github 目录不存在"
    exit 1
fi

# 配置远程仓库
git remote set-url origin https://${GITHUB_TOKEN}@github.com/ojbkxc/AIGX.git

# 获取当前分支
CURRENT_BRANCH=$(git branch --show-current)
echo "当前分支: $CURRENT_BRANCH"
echo ""

# 添加 .github 目录
echo "📦 添加 .github/ 目录..."
git add .github/

# 创建提交
COMMIT_MSG="ci: 添加 GitHub Actions 自动编译工作流

-> 推送到: ojbkxc/AIGX
-> 目的: 多平台自动编译测试

推送后将自动编译：
✅ Linux AMD64
✅ Linux ARM64
✅ Windows AMD64
✅ macOS Intel
✅ macOS ARM64

推送到 main 分支后自动触发编译"
git commit -m "$COMMIT_MSG" || echo "没有需要提交的更改"

# 推送到 GitHub
echo ""
echo "🚀 开始推送到 GitHub..."
git push origin $CURRENT_BRANCH

if [ $? -eq 0 ]; then
    echo ""
    echo "==============================================="
    echo "✅ 推送成功！↓"
    echo "==============================================="
    echo ""
    echo "🔥 GitHub Actions 已自动启动编译！"
    echo ""
    echo "📊 查看编译进度："
    echo "   https://github.com/ojbkxc/AIGX/actions"
    echo ""
    echo "✅ 所有平台编译成功后，运行以下命令创建正式版："
    echo "   git tag v1.0.0 && git push origin v1.0.0"
    echo ""
    echo "💡 停止工作的 5 分钟将编译完成"
    echo "💡 未来每次推送到 main 分支都会自动触发编译"
    echo ""
else
    echo ""
    echo "❌ 推送失败"
    exit 1
fi