use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::sysinfo::SystemInfo;

use super::check::Check;

pub fn load(info: &SystemInfo) -> Check {
    let cores = f64::from(info.ncores.max(1));
    let per_core = info.load_1 / cores;
    let value = format!("{:.2} ({:.2} per core)", info.load_1, per_core);
    if per_core > 1.0 {
        Check::warn(
            "load",
            "System Load",
            value,
            "system is under sustained load",
        )
    } else {
        Check::ok("load", "System Load", value)
    }
}

pub fn memory(info: &SystemInfo) -> Check {
    if info.mem_total == 0 {
        return Check::unknown("memory", "Memory Usage", "unknown");
    }
    #[allow(clippy::cast_precision_loss)]
    let pct = (info.mem_used as f64 / info.mem_total as f64) * 100.0;
    let value = format!("{pct:.1}%");
    if pct > 95.0 {
        Check::critical(
            "memory",
            "Memory Usage",
            value,
            "free memory: close apps or restart heavy processes",
        )
    } else if pct > 90.0 {
        Check::warn("memory", "Memory Usage", value, "memory pressure rising")
    } else {
        Check::ok("memory", "Memory Usage", value)
    }
}

pub fn disk(info: &SystemInfo) -> Check {
    if info.disk_total == 0 {
        return Check::unknown("disk", "Disk Usage", "unknown");
    }
    #[allow(clippy::cast_precision_loss)]
    let pct = (info.disk_used as f64 / info.disk_total as f64) * 100.0;
    let value = format!("{pct:.1}%");
    if pct > 95.0 {
        Check::critical(
            "disk",
            "Disk Usage",
            value,
            "free disk space: clean caches or move data",
        )
    } else if pct > 90.0 {
        Check::warn("disk", "Disk Usage", value, "disk space getting low")
    } else {
        Check::ok("disk", "Disk Usage", value)
    }
}

pub fn ip_address(info: &SystemInfo) -> Check {
    if info.ip_addr.is_empty() {
        Check::unknown("ip", "IP Address", "unknown")
    } else {
        Check::ok("ip", "IP Address", info.ip_addr.clone())
    }
}

pub fn uptime(info: &SystemInfo) -> Check {
    if info.uptime_secs == 0 {
        return Check::unknown("uptime", "System Uptime", "unknown");
    }
    let days = info.uptime_secs / 86_400;
    let hours = (info.uptime_secs % 86_400) / 3_600;
    let minutes = (info.uptime_secs % 3_600) / 60;
    let value = format!("{days} days, {hours} hours, {minutes} minutes");
    Check::ok("uptime", "System Uptime", value)
}

const NETWORK_HOST: &str = "1.1.1.1:53";
const NETWORK_TIMEOUT: Duration = Duration::from_millis(500);

pub fn network() -> Check {
    match try_connect(NETWORK_HOST, NETWORK_TIMEOUT) {
        Ok(latency_ms) => Check::ok(
            "network",
            "Network Status",
            format!("Connected ({latency_ms}ms to {NETWORK_HOST})"),
        ),
        Err(reason) => Check::critical(
            "network",
            "Network Status",
            format!("Disconnected ({reason})"),
            "check network connection",
        ),
    }
}

fn try_connect(host: &str, timeout: Duration) -> Result<u128, String> {
    let addr: SocketAddr = host
        .to_socket_addrs()
        .map_err(|e| format!("resolve: {e}"))?
        .next()
        .ok_or_else(|| "no addresses".to_string())?;
    let start = std::time::Instant::now();
    TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("{e}"))?;
    Ok(start.elapsed().as_millis())
}

#[cfg(test)]
mod tests {
    use super::super::check::Severity;
    use super::*;
    use crate::sysinfo::SystemInfo;

    fn fake(mem_used: u64, mem_total: u64, disk_used: u64, disk_total: u64) -> SystemInfo {
        SystemInfo {
            hostname: String::new(),
            os: "test",
            arch: "test",
            date: String::new(),
            ncores: 8,
            load_1: 0.5,
            load_5: 0.0,
            load_15: 0.0,
            mem_total,
            mem_used,
            mem_wired: 0,
            mem_compressed: 0,
            disk_total,
            disk_used,
            uptime_secs: 3661,
            proc_count: 0,
            ip_addr: String::new(),
        }
    }

    #[test]
    fn load_ok_when_per_core_below_one() {
        let info = fake(0, 1, 0, 1);
        let c = load(&info);
        assert_eq!(c.severity, Severity::Ok);
    }

    #[test]
    fn load_warns_when_per_core_over_one() {
        let mut info = fake(0, 1, 0, 1);
        info.ncores = 2;
        info.load_1 = 3.0;
        let c = load(&info);
        assert_eq!(c.severity, Severity::Warn);
    }

    #[test]
    fn memory_unknown_when_total_zero() {
        let info = fake(0, 0, 0, 1);
        let c = memory(&info);
        assert_eq!(c.severity, Severity::Unknown);
    }

    #[test]
    fn memory_warn_at_91_pct() {
        let info = fake(91, 100, 0, 1);
        let c = memory(&info);
        assert_eq!(c.severity, Severity::Warn);
    }

    #[test]
    fn memory_critical_at_96_pct() {
        let info = fake(96, 100, 0, 1);
        let c = memory(&info);
        assert_eq!(c.severity, Severity::Critical);
    }

    #[test]
    fn disk_critical_at_96_pct() {
        let info = fake(0, 1, 96, 100);
        let c = disk(&info);
        assert_eq!(c.severity, Severity::Critical);
    }

    #[test]
    fn ip_unknown_when_empty() {
        let info = fake(0, 1, 0, 1);
        let c = ip_address(&info);
        assert_eq!(c.severity, Severity::Unknown);
    }

    #[test]
    fn ip_ok_when_present() {
        let mut info = fake(0, 1, 0, 1);
        info.ip_addr = "192.168.1.10".to_string();
        let c = ip_address(&info);
        assert_eq!(c.severity, Severity::Ok);
        assert_eq!(c.value, "192.168.1.10");
    }

    #[test]
    fn uptime_formats_days_hours_minutes() {
        let info = fake(0, 1, 0, 1); // uptime_secs = 3661 → 0d 1h 1m
        let c = uptime(&info);
        assert!(c.value.contains("0 days"));
        assert!(c.value.contains("1 hours"));
        assert!(c.value.contains("1 minutes"));
    }
}
