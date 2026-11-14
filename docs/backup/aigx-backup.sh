#!/usr/bin/env bash
# AIGX 备份脚本（PG 主库 + SQLite aigx.db 降级路径）
#
# 三种模式：
#   aigx-backup.sh backup                备份（默认）
#   aigx-backup.sh list                  列出已有备份
#   aigx-backup.sh restore <备份文件>     恢复（破坏性操作，恢复前先停服务）
#
# 环境变量（凭据一律走环境变量，禁止写进仓库/脚本）：
#   PG* 标准 libpq 变量（PGHOST/PGPORT/PGUSER/PGPASSWORD/PGDATABASE）
#     PG 模式下 pg_dump/psql 直接读取；PGDATABASE 必填（pg_dump 的库名）
#   AIGX_BACKUP_DIR       备份目录（默认 /var/lib/aigx/backups）
#   AIGX_BACKUP_RETENTION 保留天数（默认 7）
#   AIGX_BACKUP_MODE      auto | pg | sqlite（默认 auto）
#   AIGX_DB_PATH          SQLite 库文件路径（不设则按候选路径探测）
#   AIGX_BACKUP_OFF_SITE  异地 rsync 目标（含主机:路径，如 user@host:/backups/aigx）；
#                         留空则不同步异地
#   AIGX_BACKUP_RSYNC_OPTS 异地 rsync 附加选项（如隧道场景 -e "ssh -p 10122"）
#
# cron 示例（每天 3:00，异地走 rsync）：
#   0 3 * * *  PGHOST=127.0.0.1 PGPORT=5434 PGUSER=aigx PGPASSWORD='***' PGDATABASE=aigx \
#              /path/to/docs/backup/aigx-backup.sh backup >> /var/log/aigx-backup.log 2>&1

set -euo pipefail

BACKUP_DIR="${AIGX_BACKUP_DIR:-/var/lib/aigx/backups}"
RETENTION_DAYS="${AIGX_BACKUP_RETENTION:-7}"
MODE="${AIGX_BACKUP_MODE:-auto}"
OFF_SITE="${AIGX_BACKUP_OFF_SITE:-}"
RSYNC_OPTS="${AIGX_BACKUP_RSYNC_OPTS:-}"
SQLITE_DB="${AIGX_DB_PATH:-}"
TS="$(date +%Y%m%d-%H%M%S)"

log() { printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"; }
die() { printf '[%s] ERROR: %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >&2; exit 1; }

usage() {
    # 打印文件头注释块（从首行注释到 set 行，行号变化不影响）
    awk '/^# AIGX 备份脚本/{f=1} f&&/^set -euo pipefail/{exit} f' "$0" \
        | sed 's/^# //; s/^#//'
    exit "${1:-0}"
}

# ---------- PG 可用性探测 ----------
pg_available() {
    command -v pg_dump >/dev/null 2>&1 &&
        { [ -n "${PGDATABASE:-}" ] || [ -n "${PGHOST:-}" ] || [ -n "${PGPASSWORD:-}" ]; }
}

# ---------- 模式判定 ----------
resolve_mode() {
    case "$MODE" in
        pg)
            pg_available || die "AIGX_BACKUP_MODE=pg 但 pg_dump 不可用或未配置 PG 环境变量（PGDATABASE 必填）"
            echo pg
            ;;
        sqlite)
            echo sqlite
            ;;
        auto)
            if pg_available; then echo pg; else echo sqlite; fi
            ;;
        *) die "未知 AIGX_BACKUP_MODE: $MODE（可选 auto|pg|sqlite）" ;;
    esac
}

# ---------- SQLite 库文件探测 ----------
find_sqlite_db() {
    if [ -n "$SQLITE_DB" ]; then
        [ -f "$SQLITE_DB" ] || die "AIGX_DB_PATH 指向的文件不存在: $SQLITE_DB"
        echo "$SQLITE_DB"
        return
    fi
    for candidate in "$HOME/.aigx/data/aigx.db" "${AIGX_DATA_DIR:-$HOME/.aigx}/data/aigx.db" \
        "/var/lib/aigx/data/aigx.db" "./data/aigx.db"; do
        if [ -f "$candidate" ]; then
            echo "$candidate"
            return
        fi
    done
    die "未找到 aigx.db（候选: ~/.aigx/data/aigx.db 等），请用 AIGX_DB_PATH 指定"
}

# ---------- 备份 ----------
do_backup() {
    mkdir -p "$BACKUP_DIR"
    local mode
    mode="$(resolve_mode)"

    if [ "$mode" = pg ]; then
        command -v pg_dump >/dev/null 2>&1 || die "pg_dump 不可用"
        [ -n "${PGDATABASE:-}" ] || die "PG 模式需要 PGDATABASE（pg_dump 要连哪个库）"
        local out="$BACKUP_DIR/aigx-backup-$TS.sql.gz"
        log "PG 备份: pg_dump $PGDATABASE@${PGHOST:-localhost}:${PGPORT:-5432} -> $out"
        # --no-owner/--no-privileges：异机恢复不依赖原角色权限；
        # --clean --if-exists：恢复幂等（先清再建，可重复导入）
        pg_dump --format=plain --no-owner --no-privileges --clean --if-exists \
            --file=- "$PGDATABASE" | gzip -9 > "$out" \
            || die "pg_dump 失败（检查 PG* 环境变量与凭据）"
        [ -s "$out" ] || die "备份产物为空: $out"
        log "PG 备份完成: $(du -h "$out" | cut -f1)"
    else
        command -v sqlite3 >/dev/null 2>&1 || die "SQLite 降级路径需要 sqlite3 命令"
        local db
        db="$(find_sqlite_db)"
        local out="$BACKUP_DIR/aigx-backup-$TS.db.gz"
        log "SQLite 备份: $db -> $out"
        # .backup 是 SQLite 热备份（在线安全拷贝），比 cp 文件可靠。
        # 中转文件放在备份目录内（原生路径）——mktemp 的路径嵌进
        # .backup 命令字符串时不会被 shell 翻译（Windows 下必坏），
        # 且先写无后缀文件再 gzip：直接 .backup 到 .gz 文件名会让
        # gzip 拒压（"already has .gz suffix"），产物实为未压缩文件。
        local raw
        raw="$BACKUP_DIR/.tmp-aigx-backup-$TS.raw"
        sqlite3 "$db" ".backup '$raw'" || { rm -f "$raw"; die "sqlite3 .backup 失败"; }
        gzip -9 -c "$raw" > "$out" || { rm -f "$raw"; die "gzip 失败"; }
        rm -f "$raw"
        log "SQLite 备份完成: $(du -h "$out" | cut -f1)"
    fi

    # 保留策略：按文件名前缀 + 天数清理（只删本脚本产物）
    find "$BACKUP_DIR" -maxdepth 1 -name 'aigx-backup-*' -type f -mtime +"$RETENTION_DAYS" -delete
    log "保留策略: 删除 $RETENTION_DAYS 天前的备份"

    # 异地同步（可选）
    if [ -n "$OFF_SITE" ]; then
        if command -v rsync >/dev/null 2>&1; then
            # $RSYNC_OPTS 按单词展开（有意为之：隧道场景 -e "ssh -p 10122"）
            rsync -az --info=progress2 $RSYNC_OPTS "$BACKUP_DIR"/ "$OFF_SITE"/ || die "异地 rsync 失败: $OFF_SITE"
            log "异地同步完成: $OFF_SITE"
        else
            log "警告: 配置了异地同步但 rsync 不可用，跳过"
        fi
    fi
}

# ---------- 恢复 ----------
do_restore() {
    [ $# -ge 1 ] || die "用法: $0 restore <备份文件>"
    local file="$1"
    [ -f "$file" ] || die "备份文件不存在: $file"

    if [[ "$file" == *.sql.gz ]]; then
        [ -n "${PGDATABASE:-}" ] || die "PG 恢复需要 PGDATABASE（恢复到哪个库）"
        command -v psql >/dev/null 2>&1 || die "psql 不可用"
        log "警告: 即将把 $file 恢复进 $PGDATABASE@${PGHOST:-localhost}:${PGPORT:-5432}（--clean 会先删后建，破坏现有数据）"
        log "建议先恢复到新建的临时库验证，再切换流量（见 DEPLOYMENT.md「恢复演练」）"
        gunzip -c "$file" | psql -v ON_ERROR_STOP=1 -q "$PGDATABASE" \
            || die "psql 恢复失败"
        log "PG 恢复完成: $PGDATABASE"
    elif [[ "$file" == *.db.gz ]]; then
        command -v sqlite3 >/dev/null 2>&1 || die "sqlite3 不可用"
        # 全损场景库文件可能不存在：AIGX_DB_PATH 直接用；未设则取首个候选，
        # 都没有则用默认候选（父目录自动创建）
        local db
        if [ -n "$SQLITE_DB" ]; then
            db="$SQLITE_DB"
        else
            db=""
            for candidate in "$HOME/.aigx/data/aigx.db" "${AIGX_DATA_DIR:-$HOME/.aigx}/data/aigx.db" \
                "/var/lib/aigx/data/aigx.db" "./data/aigx.db"; do
                if [ -f "$candidate" ]; then db="$candidate"; break; fi
            done
            [ -n "$db" ] || db="$HOME/.aigx/data/aigx.db"
        fi
        mkdir -p "$(dirname "$db")"
        log "警告: 即将用 $file 覆盖 $db"
        log "警告: 恢复前请先停止 AIGX 服务（systemctl stop aigx），避免写中途覆盖"
        local tmp
        tmp="$(mktemp)"
        gunzip -c "$file" > "$tmp"
        sqlite3 "$tmp" "PRAGMA integrity_check;" | grep -q '^ok' \
            || die "备份文件完整性校验失败，拒绝恢复"
        mv "$tmp" "$db"
        log "SQLite 恢复完成: $db（请启动 AIGX 服务并登录面板验证数据）"
    else
        die "无法识别的备份格式（支持 .sql.gz / .db.gz）: $file"
    fi
}

# ---------- 列表 ----------
do_list() {
    [ -d "$BACKUP_DIR" ] || die "备份目录不存在: $BACKUP_DIR"
    # 显式文件参数 ls 不打印 total 头；不用 tail 过滤（单文件时会吞掉唯一一行）
    ls -lht "$BACKUP_DIR"/aigx-backup-* 2>/dev/null ||
        die "$BACKUP_DIR 下没有备份文件"
}

case "${1:-backup}" in
    backup) do_backup ;;
    list) do_list ;;
    restore) shift; do_restore "$@" ;;
    -h | --help | help) usage 0 ;;
    *) usage 1 ;;
esac
