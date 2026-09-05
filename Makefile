# AIGX Makefile - 全平台构建自动化

# 默认目标
.DEFAULT_GOAL := help

# 环境配置
BUILD_DIR := build
Dockerfile := Dockerfile
DockerfileFrontend := frontend/Dockerfile
REGISTRY := aigx
VERSION := $(shell date +%Y.%m.%d)

# 平台列表
PLATFORMS := linux/amd64,linux/arm64

# 颜色输出
BLUE := \033[0;34m
GREEN := \033[0;32m
YELLOW := \033[1;33m
NC := \033[0m

# ============================================
# 后端构建
# ============================================

backend-build:
	@echo "$(BLUE)构建 Rust 后端 (多平台)...$(NC)"
	@mkdir -p $(BUILD_DIR)/backend
	@docker buildx create --name multiarch-builder --use > /dev/null 2>&1 || true
	@docker buildx build \
		--platform $(PLATFORMS) \
		--file $(Dockerfile) \
		--target production \
		--tag $(REGISTRY)/aigx:$(VERSION) \
		--push
	@echo "$(GREEN)✓ 后端镜像构建完成$(NC)"

backend-local-amd64:
	@echo "$(BLUE)构建 Linux AMD64 二进制...$(NC)"
	@mkdir -p $(BUILD_DIR)/amd64
	@cargo build --release --target x86_64-unknown-linux-gnu
	@cp target/x86_64-unknown-linux-gnu/release/aigx $(BUILD_DIR)/amd64/aigx
	@strip $(BUILD_DIR)/amd64/aigx
	@echo "$(GREEN)✓ 完成 AMD64 二进制$(NC)"

backend-local-arm64:
	@echo "$(BLUE)构建 Linux ARM64 二进制...$(NC)"
	@mkdir -p $(BUILD_DIR)/arm64
	@cargo build --release --target aarch64-unknown-linux-gnu
	@cp target/aarch64-unknown-linux-gnu/release/aigx $(BUILD_DIR)/arm64/aigx
	@strip $(BUILD_DIR)/arm64/aigx
	@echo "$(GREEN)✓ 完成 ARM64 二进制$(NC)"

# ============================================
# 前端构建
# ============================================

frontend-build:
	@echo "$(BLUE)构建 React 前端 (多平台)...$(NC)"
	@cd frontend && npm ci
	@cd frontend && npm run build
	@mkdir -p $(BUILD_DIR)/frontend/public
	@cp -r frontend/dist/* $(BUILD_DIR)/frontend/public/
	@echo "$(GREEN)✓ 前端构建完成$(NC)"

frontend-docker:
	@mkdir -p $(BUILD_DIR)/frontend
	@docker buildx build \
		--platform $(PLATFORMS) \
		--file $(DockerfileFrontend) \
		--tag $(REGISTRY)/aigx-frontend:$(VERSION) \
		--push
	@echo "$(GREEN)✓ 前端 Docker 镜像完成$(NC)"

# ============================================
# 完整构建
# ============================================

all: backend-build frontend-build
	@echo "$(GREEN)✓ 所有组件构建完成$(NC)"

all-docker: backend-build frontend-docker
	@echo "$(GREEN)✓ 所有 Docker 镜像完成$(NC)"

# ============================================
# 工具和脚本
# ============================================

build-adhoc:
	@echo "$(BLUE)使用 build-binaries.sh 构建所有二进制文件$(NC)"
	@bash build-binaries.sh build

build-multiarch-docker:
	@echo "$(BLUE)使用 docker-build.sh 构建所有 Docker 镜像$(NC)"
	@bash docker-build.sh all

clean:
	@echo "$(YELLOW)清理构建文件...$(NC)"
	@cargo clean
	@rm -rf $(BUILD_DIR)
	@docker buildx rm multiarch-builder || true
	@rm -rf target/
	@echo "$(GREEN)✓ 清理完成$(NC)"

clean-docker:
	@echo "$(YELLOW)清理 Docker 镜像...$(NC)"
	@docker rmi $(REGISTRY)/aigx:latest || true
	@docker rmi $(REGISTRY)/aigx:$(VERSION) || true
	@docker rmi $(REGISTRY)/aigx-frontend:latest || true
	@docker rmi $(REGISTRY)/aigx-frontend:$(VERSION) || true
	@echo "$(GREEN)✓ Docker 镜像清理完成$(NC)"

# ============================================
# 测试和验证
# ============================================

test-backend:
	@echo "$(BLUE)运行后端测试...$(NC)"
	@cargo test --release
	@cargo test --release -- --nocapture

test-frontend:
	@echo "$(BLUE)运行前端测试...$(NC)"
	@cd frontend && npm test

test-integration:
	@echo "$(BLUE)运行集成测试...$(NC)"
	@cargo test --test integration --release

# ============================================
# 文档
# ============================================

docs: README.md CHANGELOG.md DEPLOYMENT.md
	@echo "$(GREEN)✓ 文档更新完成$(NC)"

install: backend-local-amd64
	@echo "$(GREEN)安装二进制到系统$(NC)"
	@sudo cp $(BUILD_DIR)/amd64/aigx /usr/local/bin/
	@chmod +x /usr/local/bin/aigx
	@echo "$(GREEN)✓ 安装完成$(NC)"

# ============================================
# 帮助
# ============================================

help: ## 显示帮助信息
	@echo "$(GREEN)AIGX 构建工具$(NC)"
	@echo ""
	@echo "可用命令:"
	@echo "  $(BLUE)make backend-build$(NC)      构建后端 Docker 镜像"
	@echo "  $(BLUE)make backend-local-amd64$(NC) 构建本地 AMD64 二进制"
	@echo "  $(BLUE)make backend-local-arm64$(NC) 构建本地 ARM64 二进制"
	@echo "  $(BLUE)make frontend-build$(NC)    构建前端"
	@echo "  $(BLUE)make frontend-docker$(NC)   构建前端 Docker 镜像"
	@echo "  $(BLUE)make all$(NC)                构建所有组件"
	@echo "  $(BLUE)make all-docker$(NC)         构建所有 Docker 镜像"
	@echo "  $(BLUE)make build-adhoc$(NC)        使用脚本构建所有二进制"
	@echo "  $(BLUE)make clean$(NC)              清理构建文件"
	@echo "  $(BLUE)make clean-docker$(NC)       清理 Docker 镜像"
	@echo "  $(BLUE)make test-backend$(NC)       运行后端测试"
	@echo "  $(BLUE)make test-frontend$(NC)      运行前端测试"
	@echo "  $(BLUE)make install$(NC)            安装系统"
	@echo "  $(BLUE)make help$(NC)               显示此帮助"
	@echo ""
	@echo "平台支持:"
	@echo "  Linux  AMD64  x86_64"
	@echo "  Linux  ARM64  aarch64"
	@echo "  Windows AMD64 x86_64 (使用 WSL 或 cross)"
	@echo "  Windows ARM64 aarch64 (使用 cross)"
	@echo "  macOS  AMD64  x86_64"
	@echo "  macOS  ARM64  arm64 (Apple Silicon)"

.PHONY: help backend-build backend-local-amd64 backend-local-arm64 frontend-build frontend-docker all all-docker build-adhoc build-multiarch-docker clean clean-docker test-backend test-frontend test-integration docs install