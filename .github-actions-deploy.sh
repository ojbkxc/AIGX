#!/bin/bash
# AIGX GitHub Actions 推送脚本
# 先推送工作流文件进行编译测试

echo "================================================"
echo "AIGX GitHub Actions 编译推送"
echo "================================================"
echo ""

# GitHub Token
TOKEN="GITHUB_TOKEN_PLACEHOLDER"

# 输入用户名
echo "请输入您的 GitHub 用户名:"
read USERNAME

# 检查是否已有仓库
if git remote -v | grep -q "$USERNAME/aigx"; then
    echo "✅ 找到仓库: $USERNAME/aigx"
else
    echo "❌ 未找到仓库，请确保仓库已创建"
    exit 1
fi

REPO="https://${TOKEN}@github.com/$USERNAME/aigx.git"

echo "准备推送 .github/workflows 文件..."
echo "仓库: $USERNAME/aigx"
echo ""

# 添加工作流文件
git add .github/

# 提交
git commit -m "ci: 添加 GitHub Actions 工作流配置

- 多平台编译工作流
- Linux AMD64/ARM64 构建
- Windows AMD64 构建
- 测试版 Creates测试"

# 推送
if git push origin main; then
    echo ""
    echo "================================================"
    echo "✅ 推送成功！GitHub Actions 已启动"
    echo "================================================"
    echo ""
    echo "🚀 编译中，请等待..."
    echo "📊 查看编译状态:"
    echo "   https://github.com/$USERNAME/aigx/actions"
    echo ""
    echo "✅ 所有平台构建成功后，请运行以下命令创建正式版:"
    echo "   git tag v1.0.0 && git push origin v1.0.0"
    echo ""
else
    echo ""
    echo "❌ 推送失败"
    exit 1
fi