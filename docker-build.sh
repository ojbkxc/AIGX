#!/bin/bash
# AIGX 多平台 Docker 构建

set -e

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}AIGX 多平台 Docker 镜像构建工具${NC}"
echo "------------------------------------------------"

# 构建配置
REGISTRY="${REGISTRY:-aigx}"
IMAGE_NAME="${REGISTRY}/aigx:${VERSION:-latest}"
FRONTEND_IMAGE="${REGISTRY}/aigx-frontend:${VERSION:-latest}"

# 支持的平台
PLATFORMS="linux/amd64,linux/arm64"

# Docker 构建选项
DOCKER_BUILDKIT=1
BUILD_ARGS="--progress=plain"

# 解析命令行参数
ACTION="${ACTION:-build}"

if [ "$1" = "-p" ] || [ "$1" = "--platform" ]; then
    PLATFORMS="$2"
    shift 2
fi

echo -e "${YELLOW}构建配置:${NC}"
echo "  平台: $PLATFORMS"
echo "  镜像: $IMAGE_NAME"
echo ""

build_backend() {
    echo -e "${GREEN}构建后端镜像...${NC}"

    # 构建参数
    BUILD_ARGS="${BUILD_ARGS} --file Dockerfile --build-arg BUILDKIT_INLINE_CACHE=1"

    # 多平台构建
    docker buildx build \
        --platform "$PLATFORMS" \
        $BUILD_ARGS \
        --tag "$IMAGE_NAME" \
        --push \
        .

    echo -e "${GREEN}✓ 后端镜像构建完成${NC}"
}

build_frontend() {
    echo -e "${GREEN}构建前端镜像...${NC}"

    # 切换到前端目录
    cd frontend

    # 构建参数
    BUILD_ARGS="${BUILD_ARGS} --file Dockerfile --build-arg BUILDKIT_INLINE_CACHE=1"

    # 多平台构建
    docker buildx build \
        --platform "$PLATFORMS" \
        $BUILD_ARGS \
        --tag "$FRONTEND_IMAGE" \
        --push \
        .

    cd ..
    echo -e "${GREEN}✓ 前端镜像构建完成${NC}"
}

build_all() {
    build_backend
    build_frontend
}

create_manifest() {
    echo -e "${GREEN}创建多架构清单...${NC}"

    docker buildx imagetools create \
        --tag "$IMAGE_NAME" "$IMAGE_NAME"
}

manifest_inspect() {
    echo -e "${GREEN}查看构建的镜像信息...${NC}"

    docker buildx imagetools inspect "$IMAGE_NAME"
}

# 执行命令
case "$ACTION" in
    backend)
        build_backend
        ;;
    frontend)
        build_frontend
        ;;
    all)
        create_multiarch_builder
        build_all
        ;;
    manifest)
        manifest_inspect
        ;;
    clean)
        echo -e "${YELLOW}清理构建缓存...${NC}"
        docker buildx prune -f
        ;;
    *)
        echo -e "${YELLOW}使用方法:${NC}"
        echo "  $0 [options] <command>"
        echo ""
        echo "选项:"
        echo "  -p, --platform PLATFORMS   设置平台 (默认: $PLATFORMS)"
        echo "  REGISTRY=registry         设置镜像仓库 (默认: $REGISTRY)"
        echo "  VERSION=tag               设置版本标签 (默认: latest)"
        echo ""
        echo "命令:"
        echo "  backend                    构建后端镜像"
        echo "  frontend                   构建前端镜像"
        echo "  all                        构建所有镜像"
        echo "  manifest                   查看镜像信息"
        echo "  clean                      清理构建缓存"
        echo ""
        echo "示例:"
        echo "  $0 backend -p linux/arm64"
        echo "  REGISTRY=myregistry.com VERSION=1.0.0 $0 all"
        exit 1
        ;;
esac

echo ""
echo -e "${GREEN}✓ 构建完成！${NC}"