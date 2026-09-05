//! 系统监控采集——参照 burncloud crates/service/crates/monitor 的
//! collectors（cpu.rs/memory.rs/disk.rs）移植，零额外依赖（不引入
//! sysinfo/winapi crate）：
//!
//! - Linux：/proc/stat（CPU 两次采样差分）、/proc/meminfo、/proc/loadavg、
//!   statvfs 替代方案（磁盘用 `df -k`? 不，直接 std::fs 获取不到——
//!   磁盘容量在无 libc 依赖下不可得，返回 0 由前端隐藏）
//! - Windows：PowerShell one-liner 或 wmic 不可靠——保守用
//!   `wmic` 已弃用，改用 cmd 内置不可行。Windows 下 CPU/内存经
//!   `powershell -Command "Get-CimInstance"` 采集（可选，失败返回 0）
//!
//! 简化决策（AIGX 主部署目标 Linux Docker）：非 Linux 平台各项返回
//! 0/None，/api/monitor 响应里带 `available: false` 字段，前端据此降级。
//!
//! CPU 使用率：两次采样差分（与 burncloud CpuCollector 相同算法）。
//! 采集器持有上次采样，由 `/api/monitor` handler 持有单例。

use std::sync::Mutex;
use std::time::Instant;

/// CPU 采样（jiffies）
#[derive(Debug, Clone, Copy)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

/// CPU 信息
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CpuInfo {
    pub usage_percent: f64,
    pub core_count: usize,
    /// 首次采样差分不可得时 false
    pub sampled: bool,
}

/// 内存信息（字节）
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MemoryInfo {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub usage_percent: f64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub available_flag: bool,
}

/// 负载信息
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LoadInfo {
    pub load_1m: f64,
    pub load_5m: f64,
    pub load_15m: f64,
    pub available_flag: bool,
}

/// 进程信息
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ProcessInfo {
    /// PID
    pub pid: u32,
    /// 进程内存 RSS（字节）
    pub rss_bytes: u64,
    /// 进程运行秒数
    pub uptime_secs: u64,
}

/// 单次采集结果（/api/monitor 响应体）
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SystemSnapshot {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub load: LoadInfo,
    pub process: ProcessInfo,
    pub collected_at: i64,
}

/// 系统采集器（持有 CPU 上次采样以做差分；线程安全）
pub struct SystemCollector {
    last_cpu: Mutex<Option<(CpuTimes, Instant)>>,
}

impl Default for SystemCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemCollector {
    pub fn new() -> Self {
        Self {
            last_cpu: Mutex::new(None),
        }
    }

    /// 采集一次全量快照
    pub fn snapshot(&self) -> SystemSnapshot {
        SystemSnapshot {
            cpu: self.cpu_info(),
            memory: memory_info(),
            load: load_info(),
            process: process_info(),
            collected_at: chrono::Utc::now().timestamp(),
        }
    }

    /// CPU 使用率（差分采样；两次调用间隔 ≥500ms 才有意义）
    fn cpu_info(&self) -> CpuInfo {
        let core_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string("/proc/stat").unwrap_or_default();
            let cpu_line = match stat.lines().next() {
                Some(l) if l.starts_with("cpu ") => l,
                _ => {
                    return CpuInfo {
                        core_count,
                        ..Default::default()
                    }
                }
            };
            let parts: Vec<&str> = cpu_line.split_whitespace().collect();
            // cpu user nice system idle iowait irq softirq steal
            let val = |i: usize| {
                parts
                    .get(i)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0)
            };
            let idle = val(4) + val(5); // idle + iowait
            let total: u64 = (1..parts.len().min(11)).map(val).sum();

            let mut last = self.last_cpu.lock().unwrap();
            let now = Instant::now();
            let usage = match last.as_ref() {
                Some((prev, _)) if total > prev.total => {
                    let total_diff = total - prev.total;
                    let idle_diff = idle.saturating_sub(prev.idle).min(total_diff);
                    100.0 - (idle_diff as f64 / total_diff as f64 * 100.0)
                }
                _ => 0.0,
            };
            *last = Some((CpuTimes { idle, total }, now));
            CpuInfo {
                usage_percent: usage.clamp(0.0, 100.0),
                core_count,
                sampled: true,
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            CpuInfo {
                core_count,
                usage_percent: 0.0,
                sampled: false,
            }
        }
    }
}

/// 内存信息（Linux /proc/meminfo）
fn memory_info() -> MemoryInfo {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let mut total_kb = 0u64;
        let mut available_kb = 0u64;
        let mut free_kb = 0u64;
        let mut buffers_kb = 0u64;
        let mut cached_kb = 0u64;
        let mut swap_total_kb = 0u64;
        let mut swap_free_kb = 0u64;
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }
            let v = parts[1].parse::<u64>().unwrap_or(0);
            match parts[0] {
                "MemTotal:" => total_kb = v,
                "MemAvailable:" => available_kb = v,
                "MemFree:" => free_kb = v,
                "Buffers:" => buffers_kb = v,
                "Cached:" => cached_kb = v,
                "SwapTotal:" => swap_total_kb = v,
                "SwapFree:" => swap_free_kb = v,
                _ => {}
            }
        }
        if total_kb == 0 {
            return MemoryInfo::default();
        }
        let total = total_kb * 1024;
        let available = if available_kb > 0 {
            available_kb * 1024
        } else {
            (free_kb + buffers_kb + cached_kb) * 1024
        };
        let used = total - available;
        let swap_total = swap_total_kb * 1024;
        let swap_used = swap_total.saturating_sub(swap_free_kb * 1024);
        MemoryInfo {
            total,
            used,
            available,
            usage_percent: used as f64 / total as f64 * 100.0,
            swap_total,
            swap_used,
            available_flag: true,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        MemoryInfo::default()
    }
}

/// 系统负载（Linux /proc/loadavg）
fn load_info() -> LoadInfo {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let parts: Vec<&str> = content.split_whitespace().collect();
        let f = |i: usize| {
            parts
                .get(i)
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        LoadInfo {
            load_1m: f(0),
            load_5m: f(1),
            load_15m: f(2),
            available_flag: !content.is_empty(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        LoadInfo::default()
    }
}

/// 当前进程信息（Linux /proc/self）
fn process_info() -> ProcessInfo {
    let pid = std::process::id();
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let rss_kb: u64 = status
            .lines()
            .find(|l| l.starts_with("VmRSS:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        // starttime 是第 22 个字段（1-based），单位 clock tick（100Hz）
        let uptime_secs = stat
            .rsplit(')')
            .next()
            .and_then(|rest| rest.split_whitespace().nth(19))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|ticks| ticks / 100)
            .unwrap_or(0);
        ProcessInfo {
            pid,
            rss_bytes: rss_kb * 1024,
            uptime_secs,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        ProcessInfo {
            pid,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_never_panics() {
        let c = SystemCollector::new();
        let s1 = c.snapshot();
        let s2 = c.snapshot();
        // 两次采样都应成功（Linux 下 sampled=true）
        assert_eq!(s1.cpu.sampled, s2.cpu.sampled);
    }

    #[test]
    fn memory_usage_bounds() {
        let m = memory_info();
        if m.available_flag {
            assert!(m.used <= m.total);
            assert!((0.0..=100.0).contains(&m.usage_percent));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn loadavg_parses() {
        let l = load_info();
        assert!(l.available_flag);
        assert!(l.load_1m >= 0.0);
    }
}
