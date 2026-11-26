//! 本地系统指标：时间、CPU、内存和网络速率。
//!
//! 全部数据来自 `/proc` 和 `/sys`，不需要额外依赖。采样失败时保留上一次
//! 的值，界面显示的是“最近一次成功读取”的状态。

use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 一次采集到的系统状态。
#[derive(Debug, Clone, Default)]
pub(crate) struct Metrics {
    pub(crate) time: String,
    pub(crate) date: String,
    pub(crate) cpu_percent: u8,
    pub(crate) mem_percent: u8,
    pub(crate) net_down: String,
    pub(crate) net_up: String,
    previous_cpu: Option<CpuTimes>,
    previous_net: Option<(u64, u64)>,
    previous_net_at: Option<Instant>,
}

impl Metrics {
    pub(crate) fn new() -> Self {
        let mut metrics = Self::default();
        metrics.refresh();
        metrics
    }

    /// 重新采集一次。CPU 和网络速率依赖上一次的采样，首次调用只会建立基线。
    pub(crate) fn refresh(&mut self) {
        let (time, date) = local_time();
        self.time = time;
        self.date = date;

        if let Some(cpu) = read_cpu() {
            if let Some(previous) = self.previous_cpu {
                let total = cpu.total.saturating_sub(previous.total);
                let idle = cpu.idle.saturating_sub(previous.idle);
                if let Some(percent) = (total - idle)
                    .checked_mul(100)
                    .and_then(|v| v.checked_div(total))
                {
                    self.cpu_percent = percent.min(100) as u8;
                }
            }
            self.previous_cpu = Some(cpu);
        }

        if let Some((used, total)) = read_memory() {
            self.mem_percent = ((used * 100) / total.max(1)).min(100) as u8;
        }

        let now = Instant::now();
        if let Some((rx, tx)) = read_network() {
            if let (Some((previous_rx, previous_tx)), Some(previous_at)) =
                (self.previous_net, self.previous_net_at)
            {
                let elapsed = now.duration_since(previous_at).as_secs_f64().max(0.001);
                self.net_down = format_rate(rx.saturating_sub(previous_rx) as f64 / elapsed);
                self.net_up = format_rate(tx.saturating_sub(previous_tx) as f64 / elapsed);
            }
            self.previous_net = Some((rx, tx));
            self.previous_net_at = Some(now);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

fn read_cpu() -> Option<CpuTimes> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let values: Vec<u64> = stat
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total = values.iter().sum();
    Some(CpuTimes { idle, total })
}

fn read_memory() -> Option<(u64, u64)> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = 0u64;
    let mut available = 0u64;

    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = rest.split_whitespace().next()?.parse().ok()?;
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = rest.split_whitespace().next()?.parse().ok()?;
        }
    }

    (total > 0).then(|| (total.saturating_sub(available), total))
}

fn read_network() -> Option<(u64, u64)> {
    let mut rx = 0u64;
    let mut tx = 0u64;

    for entry in std::fs::read_dir("/sys/class/net").ok()? {
        let entry = entry.ok()?;
        if entry.file_name() == "lo" {
            continue;
        }
        rx += read_counter(&entry.path().join("statistics/rx_bytes")).unwrap_or(0);
        tx += read_counter(&entry.path().join("statistics/tx_bytes")).unwrap_or(0);
    }

    Some((rx, tx))
}

fn read_counter(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn format_rate(bytes_per_second: f64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;

    if bytes_per_second >= MB {
        format!("{:.1} MB/s", bytes_per_second / MB)
    } else if bytes_per_second >= KB {
        format!("{:.0} KB/s", bytes_per_second / KB)
    } else {
        format!("{bytes_per_second:.0} B/s")
    }
}

#[cfg(unix)]
fn local_time() -> (String, String) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&seconds, &mut tm) };

    if result.is_null() {
        return (String::from("--:--:--"), String::from("----.--.--"));
    }

    (
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec),
        format!(
            "{:04}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        ),
    )
}

#[cfg(not(unix))]
fn local_time() -> (String, String) {
    (String::from("--:--:--"), String::from("unsupported"))
}
