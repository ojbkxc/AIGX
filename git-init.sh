#!/bin/bash
# AIGX Git 初始化和推送脚本
# 自动配置远程仓库并推送代码

set -e

echo "================================================"
echo "AIGX 项目 Git 初始化和推送"
echo "================================================"
echo ""

# 设置环境变量
REPO_URL="${AIGX_REPO:-https://github.com/yourusername/aigx.git}"
BRANCH="${AIGX_BRANCH:-main}"

# 检查是否有未跟踪的文件
echo "1️⃣ 检查文件状态..."
if git status >/dev/null 2>&1; then
    echo "   ℹ️  Git 仓库已存在"
    CURRENT_BRANCH=$(git branch --show-current)
    if [ "$CURRENT_BRANCH" != "$BRANCH" ]; then
        echo "   🔄 切换到分支: $BRANCH"
        git checkout $BRANCH
    fi
else
    echo "   🆕 初始化 Git 仓库..."
    git init
fi

# 添加所有文件
echo "2️⃣ 添加所有文件到暂存区..."
git add .

# 创建初始提交
echo "3️⃣ 创建初始提交..."
COMMIT_MSG="feat: 完成 AIGX 多平台构建系统

- 支持 Linux AMD64/ARM64 构建和部署
- 支持 Windows AMD64/ARM64 构建和部署
- 支持 macOS Intel/ARM64 构建和部署
- 完整的 Docker 多平台镜像构建支持
- GitHub Actions CI/CD 自动化
- 前端 React 应用
- 后端 Rust Axum 0.7 服务
- 网络层多协议支持
- 监控和告警系统
- 完整的部署文档"

git commit -m "$COMMIT_MSG"

# 配置远程仓库
echo "4️⃣ 配置远程仓库..."
git remote add origin $REPO_URL

# 创建并切换到目标分支
echo "5️⃣ 设置分支..."
git branch -M $BRANCH

# 第一次推送
echo "6️⃣ 推送到 GitHub..."
if git push -u origin $BRANCH; then
    echo ""
    echo "================================================"
    echo "✅ 推送成功！"
    echo "================================================"
    echo "仓库地址: $REPO_URL"
    echo "分支: $BRANCH"
    echo ""
    echo "下一步："
    echo "1. 访问: https://github.com/yourusername/aigx"
    echo "2. 查看编译状态: github.com/yourusername/aigx/actions"
    echo "3. 在 GitHub 设置中添加 Secret:"
    echo "   - GITHUB_TOKEN: (自动配置，无需手动设置)"
    echo ""
else
    echo ""
    echo "❌ 推送失败，检查配置后重试"
    exit 1
fi