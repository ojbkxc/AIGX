#!/bin/bash
# AIGX 多二进制文件构建脚本
# 支持平台: Linux AMD64, Linux ARM64, Windows AMD64 (exe), Windows ARM64 (exe), macOS AMD64, macOS ARM64

set -e

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}AIGX 多二进制文件构建工具${NC}"
echo "------------------------------------------------"

# 默认配置
VERSION=${VERSION:-$(date +%Y.%m.%d)}
REPO=${AIGX_REPO:-https://github.com/yourusername/aigx}
BUILD_DIR=${BUILD_DIR:-./build}

# 目标列表
declare -A TARGETS
TARGETS=(
    # Linux
    ["x86_64-unknown-linux-gnu"]="2024-Linux-x86_64"
    ["aarch64-unknown-linux-gnu"]="2024-Linux-arm64"

    # Windows
    ["x86_64-pc-windows-msvc"]="2024-Windows-x86_64.exe"
    ["aarch64-pc-windows-msvc"]="2024-Windows-arm64.exe"

    # macOS
    ["x86_64-apple-darwin"]="2024-macOS-x86_64"
    ["aarch64-apple-darwin"]="2024-macOS-arm64"
)

# 构建配置
CARGO_OPTIONS="--release"

# 解析命令行参数
ACTION="${ACTION:-build}"

if [ "$1" = "-v" ] || [ "$1" = "--version" ]; then
    echo "AIGX Installer Builder v1.0"
    echo "Supports: Linux AMD64/ARM64, Windows AMD64/ARM64, macOS AMD64/ARM64"
    exit 0
fi

if [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
    usage
    exit 0
fi

usage() {
    cat << EOF
Usage: $0 [options] <command>

选项:
  -v, --version      显示版本信息
  -h, --help         显示帮助信息
  -d, --directory DIR  指定构建目录 (默认: $BUILD_DIR)
  -V, --version STR   设置版本号 (默认: 日期)

命令:
  build              构建所有平台的二进制文件
  linux              构建所有 Linux 平台
  windows            构建所有 Windows 平台
  macos              构建所有 macOS 平台
  upload             构建并上传到 GitHub Releases
  test               测试构建的二进制文件

示例:
  $0 build
  BUILD_DIR=/tmp/build $0 build
  VERSION=1.0.0 $0 upload

支持的架构:
EOF
    for target in "${!TARGETS[@]}"; do
        echo "  - $target: MINGW: ${TARGETS[$target]}"
    done
}

# 初始化构建目录
init_build_dir() {
    mkdir -p "$BUILD_DIR"
    local timestamp=$(date +%Y%m%d)
    echo -e "${YELLOW}构建目录: $BUILD_DIR/${NC}"
}

# 检查依赖
check_dependencies() {
    echo -e "${YELLOW}检查构建依赖...${NC}"

    if ! command -v rustc &> /dev/null && ! command -v cargo &> /dev/null; then
        echo -e "${RED}错误: 未找到 Rust 工具链${NC}"
        echo "请访问 https://rustup.rs/ 下载安装"
        exit 1
    fi

    echo -e "${GREEN}✓ Rust 工具链已安装${NC}"

    # 检查交叉编译工具链
    for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
        if ! rustup target list | grep "$target" &> /dev/null; then
            echo -e "${YELLOW}安装目标: $target${NC}"
            rustup target add $target || true
        fi
    done
    echo -e "${GREEN}✓ target list 检查完成${NC}"
}

# 使用 cross 工具进行交叉编译（推荐）
build_with_cross() {
    echo -e "${YELLOW}使用 cross 工具进行交叉编译${NC}"

    if ! command -v cross &> /dev/null; then
        echo -e "${YELLOW}cross 未安装，正在安装...${NC}"
        cargo install cross --git https://github.com/cross-rs/cross --bin cross --locked
    fi

    for target in "${!TARGETS[@]}"; do
        echo -e "${BLUE}构建 ${target}...${NC}"

        cross build $CARGO_OPTIONS \
            --target $target \
            --manifest-path Cargo.toml

        # 复制二进制文件到构建目录
        local src_dir=$(rustc --print sysroot)/../lib/rustlib/$target/bin
        local binary_name="aigx"

        if [ "$(uname -s)" = "Windows" ]; then
            binary_name="aigx.exe"
        fi

        cp "$src_dir/$binary_name" "$BUILD_DIR/${TARGETS[$target]}"

        echo -e "${GREEN}✓ 完成于 ${target} -> ${BUILD_DIR}/${TARGETS[$target]}${NC}"
    done
}

# 手动交叉编译（备用方案）
build_manually() {
    echo -e "${YELLOW}使用手动交叉编译方案${NC}"

    for target in "${!TARGETS[@]}"; do
        echo -e "${BLUE}构建 ${target}...${NC}"

        # 设置交叉编译环境
        case $target in
            x86_64-pc-windows-gnu)
                CC_x86_64_pc_windows_gnu="x86_64-w64-mingw32-gcc"
                AR_x86_64_pc_windows_gnu="x86_64-w64-mingw32-gcc-ar"
                ;;
            aarch64-pc-windows-gnu)
                CC_aarch64_pc_windows_gnu="aarch64-w64-mingw32-gcc"
                AR_aarch64_pc_windows_gnu="aarch64-w64-mingw32-gcc-ar"
                ;;
        esac

        # 构建指定目标
        set -x
        cargo build $CARGO_OPTIONS \
            --target $target \
            --features "sqlite-kv" \
            --manifest-path Cargo.toml
        set +x

        # 复制二进制文件
        local src_dir=$(rustc --print sysroot)/../lib/rustlib/$target/bin
        local binary_name="aigx"

        if [ "$(uname -s)" = "Windows" ]; then
            binary_name="aigx.exe"
        fi

        cp "$src_dir/$binary_name" "$BUILD_DIR/${TARGETS[$target]}"

        echo -e "${GREEN}✓ 完成于 ${target} -> ${BUILD_DIR}/${TARGETS[$target]}${NC}"
    done
}

# 构建特定平台
build_platform() {
    platform=$1
    shift

    init_build_dir
    check_dependencies

    echo -e "${BLUE}开始构建所有 $platform 平台...${NC}"

    case $platform in
        linux|Linux)
            build_with_cross
            ;;
        windows|Windows)
            if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "win32" ]]; then
                build_with_cross
            else
                echo -e "${YELLOW}在 Linux/Unix 系统上构建 Windows 二进制文件需要交叉编译工具链${NC}"
                echo "请安装: x86_64-w64-mingw32 toolchain"
                echo "或使用: https://github.com/zulip/mingw-w64-toolchain"
                exit 1
            fi
            ;;
        macos|MacOS)
            echo -e "${YELLOW}注意: macOS 构建需要 Apple Silicon 或 Intel 单片机${NC}"
            read -p "是否继续? (y/n) " -n 1 -r
            echo
            if [[ ! $REPLY =~ ^[Yy]$ ]]; then
                exit 1
            fi
            build_with_cross
            ;;
        all|All)
            build_with_cross
            ;;
        *)
            echo "未知平台: $platform"
            usage
            exit 1
            ;;
    esac
}

# 测试构建的二进制文件
test_binaries() {
    echo -e "${BLUE}测试二进制文件...${NC}"

    for binary in "$BUILD_DIR"/*; do
        if [ -f "$binary" ]; then
            echo -e "${YELLOW}测试: $binary${NC}"

            # Linux/macOS
            if [[ "$OSTYPE" == "linux-gnu"* ]]; then
                chmod +x "$binary"
                file "$binary"

                # 尝试启动检查是否有依赖问题
                timeout 5s "$binary" --help 2>&1 || true

            # Windows
            elif [[ "$OSTYPE" == "msys" || "$OSTYPE" == "win32" ]]; then
                file "$binary"
                timeout 5s "$binary" --help 2>&1 || true
            fi
        fi
    done

    echo -e "${GREEN}✓ 测试完成${NC}"
}

# 创建安装包
create_packages() {
    echo -e "${BLUE}创建安装包...${NC}"

    local packages_dir="$BUILD_DIR/packages"
    mkdir -p "$packages_dir"

    # Linux tar.gz 包
    if [ -f "$BUILD_DIR/2024-Linux-x86_64" ]; then
        tar -czvf "$packages_dir/aigx-linux-x86_64-${VERSION}.tar.gz" \
            -C "$BUILD_DIR" "2024-Linux-x86_64" \
            --format=ustar --owner=0 --group=0
    fi

    if [ -f "$BUILD_DIR/2024-Linux-arm64" ]; then
        tar -czvf "$packages_dir/aigx-linux-arm64-${VERSION}.tar.gz" \
            -C "$BUILD_DIR" "2024-Linux-arm64" \
            --format=ustar --owner=0 --group=0
    fi

    # Windows ZIP 包
    if [ -f "$BUILD_DIR/2024-Windows-x86_64.exe" ]; then
        powershell -Command "Compress-Archive -Path '$BUILD_DIR/*Windows*x86*.exe' -DestinationPath '$packages_dir/aigx-windows-x86_64-${VERSION}.zip'"
    fi

    if [ -f "$BUILD_DIR/2024-Windows-arm64.exe" ]; then
        powershell -Command "Compress-Archive -Path '$BUILD_DIR/*Windows*arm*.exe' -DestinationPath '$packages_dir/aigx-windows-arm64-${VERSION}.zip'"
    fi

    # macOS DMG 包（需要特殊处理）
    if [ -f "$BUILD_DIR/2024-macOS-x86_64" ]; then
        echo "macOS DMG 需要 macOS 系统: $packages_dir/aigx-macos-x86_64-${VERSION}.dmg"
    fi

    if [ -f "$BUILD_DIR/2024-macOS-arm64" ]; then
        echo "macOS DMG 需要 macOS 系统: $packages_dir/aigx-macos-arm64-${VERSION}.dmg"
    fi

    echo -e "${GREEN}✓ 安装包创建完成到: $packages_dir/${NC}"
}

# 上传到 GitHub Releases
upload_to_github() {
    echo -e "${YELLOW}准备上传到 GitHub Releases...${NC}"

    if [ -z "$GITHUB_TOKEN" ]; then
        echo -e "${RED}错误: 未设置 GITHUB_TOKEN 环境变量${NC}"
        echo "请先使用: export GITHUB_TOKEN=pk_xxx"
        exit 1
    fi

    # 创建 Release
    local release_name="v${VERSION}"
    local upload_url=$(curl -s -H "Authorization: token ${GITHUB_TOKEN}" \
        -X POST \
        -H "Content-Type: application/json" \
        -d "{\"tag_name\": \"${VERSION}\", \"name\": \"AIGX ${VERSION}\", \"body\": \"Release ${VERSION} of AIGX AI Gateway\"}" \
        https://api.github.com/repos/${REPO}/releases | \
        grep -o '"upload_url": "[^"]*' | sed -e "s/\"upload_url\": \"//" -e 's/\/assets\/:name_id.*/\/assets?name=/')

    if [ -z "$upload_url" ]; then
        echo "获取 upload_url 失败"
        exit 1
    fi

    # 上传二进制文件
    echo -e "${BLUE}上传文件...${NC}"
    for binary in "$BUILD_DIR"/*; do
        if [ -f "$binary" ]; then
            local filename=$(basename "$binary")
            echo "上传: $filename"

            curl -X POST \
                -H "Authorization: token ${GITHUB_TOKEN}" \
                -H "Content-Type: application/octet-stream" \
                --data-binary @"$binary" \
                "${upload_url}${filename}"
        fi
    done

    echo -e "${GREEN}✓ 上传完成${NC}"
}

# 主函数
main() {
    case "$ACTION" in
        build|all)
            build_platform "all"
            create_packages
            ;;
        linux)
            build_platform "linux"
            ;;
        windows)
            build_platform "windows"
            ;;
        macos)
            build_platform "macos"
            ;;
        test)
            test_binaries
            ;;
        package)
            init_build_dir
            create_packages
            ;;
        upload)
            upload_to_github
            ;;
        *)
            usage
            exit 1
            ;;
    esac

    echo ""
    echo -e "${GREEN}✓ 构建完成！${NC}"
    echo "构建输出目录: $BUILD_DIR"
    echo "可使用: ls -lh $BUILD_DIR"
}

# 执行主函数
main "$@"