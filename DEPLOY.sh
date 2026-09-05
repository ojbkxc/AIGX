#!/bin/bash
# AIGX 部署脚本 - 推送所有更改到 GitHub

echo "================================================"
echo "AIGX 自动部署和推送脚本"
echo "================================================"
echo ""

# GitHub Token (从用户提供的)
GITHUB_TOKEN="GITHUB_TOKEN_PLACEHOLDER"

# 询问用户信息
echo "请输入您的 GitHub 用户名:"
read USERNAME

echo "请输入仓库名 (例如: aigx):"
read REPO_NAME

REPO="$USERNAME/$REPO_NAME"
BRANCH="${AIGX_BRANCH:-main}"

echo ""
echo "部署配置:"
echo "  用户: $USERNAME"
echo "  仓库: $REPO"
echo "  分支: $BRANCH"
echo "  Token: ***${GITHUB_TOKEN:0:7}***"
echo ""

# 完整的仓库 URL
REPO_URL="https://${GITHUB_TOKEN}@github.com/${REPO}.git"

# 检查 git
if ! command -v git &> /dev/null; then
    echo "❌ Git 未安装，请先安装 Git"
    exit 1
fi

echo "检查仓库状态..."
if [ -d ".git" ]; then
    echo "✅ Git 仓库已存在"
    CURRENT_BRANCH=$(git branch --show-current)
    echo "当前分支: $CURRENT_BRANCH"

    # 拉取最新代码
    echo "📥 拉取最新代码..."
    git pull origin $BRANCH || echo "⚠️  可能是首次推送"
else
    echo "❌ Git 仓库不存在"
    exit 1
fi

# 添加所有新文件
echo "📦 添加新文件到暂存区..."
git add .

# 检查是否有更改
if git diff --cached --name-only | grep -c . > /dev/null; then
    echo "📝 准备提交..."

    # 创建提交
    COMMIT_MSG="feat: 完成 AIGX 多平台构建系统！支持所有平台编译部署

- 添加 GitHub Actions CI/CD 配置
- 多平台二进制自动构建 (Linux AMD64/ARM64, Windows AMD64/ARM64, macOS Intel/ARM64)
- Docker 多平台镜像构建和推送
- 完整的部署文档和示例
- 多种构建脚本支持
- 前端和后端源代码
- 监控和告警系统"

    git commit -m "$COMMIT_MSG"

    echo "🔄 推送到 GitHub..."
    if git push -u origin $BRANCH; then
        echo ""
        echo "================================================"
        echo "✅ 部署成功！"
        echo "================================================"
        echo ""
        echo "仓库地址: https://github.com/$REPO"
        echo "查看编译: https://github.com/$REPO/actions"
        echo "查看发布: https://github.com/$REPO/releases"
        echo ""
        echo "开始自动编译..."
        read -p "按回车键返回..."
    else
        echo ""
        echo "❌ 推送失败"
    fi
else
    echo "ℹ️  没有需要提交的更改"
fi