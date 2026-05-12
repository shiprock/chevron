use std::env;

pub struct SystemInfo {
    pub hostname: String,
    pub os: &'static str,
    pub arch: &'static str,
    pub date: String,
    // CPU / load
    pub ncores: u32,
    pub load_1: f64,
    pub load_5: f64,
    pub load_15: f64,
    // Memory (bytes)
    pub mem_total: u64,
    pub mem_used: u64,
    #[allow(dead_code)]
    pub mem_wired: u64,
    #[allow(dead_code)]
    pub mem_compressed: u64,
    // Disk (bytes)
    pub disk_total: u64,
    pub disk_used: u64,
    // System
    pub uptime_secs: u64,
    pub proc_count: u32,
    pub ip_addr: String,
}

impl SystemInfo {
    pub fn gather() -> Self {
        Self {
            hostname: get_hostname(),
            os: env::consts::OS,
            arch: env::consts::ARCH,
            date: get_date(),
            ncores: get_ncores(),
            load_1: get_load_avg(0),
            load_5: get_load_avg(1),
            load_15: get_load_avg(2),
            mem_total: get_mem_total(),
            mem_used: get_mem_used(),
            mem_wired: get_mem_wired(),
            mem_compressed: get_mem_compressed(),
            disk_total: get_disk_total(),
            disk_used: get_disk_used(),
            uptime_secs: get_uptime_secs(),
            proc_count: get_proc_count(),
            ip_addr: get_ip_addr(),
        }
    }

    /// Format load as a simple string (for classic layout backwards compat).
    pub fn load_string(&self) -> String {
        format!("{:.2}", self.load_1)
    }

    /// Format memory as a simple string (for classic layout backwards compat).
    pub fn memory_string(&self) -> String {
        if self.mem_total > 0 {
            format!("{}GB", self.mem_total / 1_073_741_824)
        } else {
            "?GB".to_string()
        }
    }
}

fn get_hostname() -> String {
    let mut buf = [0u8; 256];
    let ret = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if ret == 0 {
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8_lossy(&buf[..len])
            .split('.')
            .next()
            .unwrap_or("unknown")
            .to_string()
    } else {
        "unknown".to_string()
    }
}

fn get_date() -> String {
    let mut t: libc::time_t = 0;
    unsafe { libc::time(&raw mut t) };
    let tm = unsafe { libc::localtime(&raw const t) };
    if tm.is_null() {
        return "????-??-??".to_string();
    }
    let tm = unsafe { &*tm };
    format!(
        "{:04}-{:02}-{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
    )
}

// ---------------------------------------------------------------------------
// macOS implementations
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn sysctl_u32(name: &std::ffi::CStr) -> Option<u32> {
    let mut val: u32 = 0;
    let mut len = std::mem::size_of::<u32>();
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut val).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 { Some(val) } else { None }
}

#[cfg(target_os = "macos")]
fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut val: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let ret = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut val).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 { Some(val) } else { None }
}

#[cfg(target_os = "macos")]
fn get_ncores() -> u32 {
    sysctl_u32(c"hw.ncpu").unwrap_or(1)
}

#[cfg(target_os = "macos")]
fn get_load_avg(index: usize) -> f64 {
    // Must match C struct loadavg { fixpt_t ldavg[3]; long fscale; }
    #[repr(C)]
    struct LoadAvg {
        ldavg: [u32; 3],
        fscale: libc::c_long,
    }
    let mut avg = std::mem::MaybeUninit::<LoadAvg>::uninit();
    let mut len = std::mem::size_of::<LoadAvg>();
    let ret = unsafe {
        libc::sysctlbyname(
            c"vm.loadavg".as_ptr(),
            avg.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 && index < 3 {
        let avg = unsafe { avg.assume_init() };
        #[allow(clippy::cast_precision_loss)]
        let fscale = (avg.fscale.max(1)) as f64;
        f64::from(avg.ldavg[index]) / fscale
    } else {
        0.0
    }
}

#[cfg(target_os = "macos")]
fn get_mem_total() -> u64 {
    sysctl_u64(c"hw.memsize").unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn get_page_size() -> u64 {
    sysctl_u64(c"hw.pagesize").unwrap_or(4096)
}

#[cfg(target_os = "macos")]
mod mach_vm {
    //! Minimal bindings for `host_statistics64` (VM info).
    use libc::c_int;

    pub const HOST_VM_INFO64: c_int = 4;
    #[allow(clippy::cast_possible_truncation)]
    pub const HOST_VM_INFO64_COUNT: u32 =
        (std::mem::size_of::<VmStatistics64>() / std::mem::size_of::<c_int>()) as u32;

    pub type MachPort = u32;

    /// Matches `struct vm_statistics64` from `<mach/vm_statistics.h>`.
    /// Fields are `natural_t` (u32) or `uint64_t` depending on their kind.
    #[repr(C)]
    #[derive(Default)]
    pub struct VmStatistics64 {
        pub free_count: u32,
        pub active_count: u32,
        pub inactive_count: u32,
        pub wire_count: u32,
        pub zero_fill_count: u64,
        pub reactivations: u64,
        pub pageins: u64,
        pub pageouts: u64,
        pub faults: u64,
        pub cow_faults: u64,
        pub lookups: u64,
        pub hits: u64,
        pub purges: u64,
        pub purgeable_count: u32,
        pub speculative_count: u32,
        pub decompressions: u64,
        pub compressions: u64,
        pub swapins: u64,
        pub swapouts: u64,
        pub compressor_page_count: u32,
        pub throttled_count: u32,
        pub external_page_count: u32,
        pub internal_page_count: u32,
        pub total_uncompressed_pages_in_compressor: u64,
    }

    unsafe extern "C" {
        pub fn mach_host_self() -> MachPort;
        pub fn host_statistics64(
            host: MachPort,
            flavor: c_int,
            info: *mut VmStatistics64,
            count: *mut u32,
        ) -> c_int;
    }
}

#[cfg(target_os = "macos")]
fn get_vm_stats() -> Option<mach_vm::VmStatistics64> {
    let mut stats = mach_vm::VmStatistics64::default();
    let mut count = mach_vm::HOST_VM_INFO64_COUNT;
    let ret = unsafe {
        mach_vm::host_statistics64(
            mach_vm::mach_host_self(),
            mach_vm::HOST_VM_INFO64,
            &raw mut stats,
            &raw mut count,
        )
    };
    if ret == 0 { Some(stats) } else { None }
}

#[cfg(target_os = "macos")]
fn get_mem_used() -> u64 {
    let page = get_page_size();
    get_vm_stats().map_or(0, |s| {
        (u64::from(s.active_count) + u64::from(s.wire_count) + u64::from(s.compressor_page_count))
            * page
    })
}

#[cfg(target_os = "macos")]
fn get_mem_wired() -> u64 {
    let page = get_page_size();
    get_vm_stats().map_or(0, |s| u64::from(s.wire_count) * page)
}

#[cfg(target_os = "macos")]
fn get_mem_compressed() -> u64 {
    let page = get_page_size();
    get_vm_stats().map_or(0, |s| u64::from(s.compressor_page_count) * page)
}

#[cfg(target_os = "macos")]
fn get_disk_info() -> (u64, u64) {
    let path = c"/";
    let mut stat: std::mem::MaybeUninit<libc::statvfs> = std::mem::MaybeUninit::uninit();
    let ret = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if ret == 0 {
        let s = unsafe { stat.assume_init() };
        #[allow(clippy::unnecessary_cast)]
        let total = s.f_frsize as u64 * u64::from(s.f_blocks);
        #[allow(clippy::unnecessary_cast)]
        let free = s.f_frsize as u64 * u64::from(s.f_bavail);
        (total, total.saturating_sub(free))
    } else {
        (0, 0)
    }
}

#[cfg(target_os = "macos")]
fn get_disk_total() -> u64 {
    get_disk_info().0
}

#[cfg(target_os = "macos")]
fn get_disk_used() -> u64 {
    get_disk_info().1
}

#[cfg(target_os = "macos")]
fn get_uptime_secs() -> u64 {
    #[repr(C)]
    struct Timeval {
        tv_sec: i64,
        tv_usec: i32,
    }
    let mut tv = std::mem::MaybeUninit::<Timeval>::uninit();
    let mut len = std::mem::size_of::<Timeval>();
    let ret = unsafe {
        libc::sysctlbyname(
            c"kern.boottime".as_ptr(),
            tv.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 {
        let tv = unsafe { tv.assume_init() };
        let mut now: libc::time_t = 0;
        unsafe { libc::time(&raw mut now) };
        #[allow(clippy::cast_sign_loss)]
        let up = (now - tv.tv_sec).max(0) as u64;
        up
    } else {
        0
    }
}

#[cfg(target_os = "macos")]
fn get_proc_count() -> u32 {
    unsafe extern "C" {
        fn proc_listallpids(buffer: *mut libc::c_void, buffersize: libc::c_int) -> libc::c_int;
    }
    let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
    #[allow(clippy::cast_sign_loss)]
    if count > 0 { count as u32 } else { 0 }
}

#[cfg(target_os = "macos")]
fn get_ip_addr() -> String {
    get_ip_addr_common()
}

// ---------------------------------------------------------------------------
// Linux implementations
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn get_ncores() -> u32 {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .map_or(1, |s| {
            #[allow(clippy::cast_possible_truncation)]
            let n = s.lines().filter(|l| l.starts_with("processor")).count() as u32;
            n.max(1)
        })
}

#[cfg(target_os = "linux")]
fn get_load_avg(index: usize) -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().nth(index).and_then(|v| v.parse().ok()))
        .unwrap_or(0.0)
}

#[cfg(target_os = "linux")]
fn get_mem_total() -> u64 {
    parse_meminfo_kb("MemTotal") * 1024
}

#[cfg(target_os = "linux")]
fn get_mem_used() -> u64 {
    let total = parse_meminfo_kb("MemTotal");
    let avail = parse_meminfo_kb("MemAvailable");
    total.saturating_sub(avail) * 1024
}

#[cfg(target_os = "linux")]
fn get_mem_wired() -> u64 {
    // Linux doesn't have a direct wired equivalent; use Shmem as proxy
    parse_meminfo_kb("Shmem") * 1024
}

#[cfg(target_os = "linux")]
fn get_mem_compressed() -> u64 {
    // SwapCached is a rough proxy for compressed on Linux
    parse_meminfo_kb("SwapCached") * 1024
}

#[cfg(target_os = "linux")]
fn parse_meminfo_kb(key: &str) -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn get_disk_info() -> (u64, u64) {
    let path = c"/";
    let mut stat: std::mem::MaybeUninit<libc::statvfs> = std::mem::MaybeUninit::uninit();
    let ret = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if ret == 0 {
        let s = unsafe { stat.assume_init() };
        let total = s.f_frsize * s.f_blocks;
        let free = s.f_frsize * s.f_bavail;
        (total, total.saturating_sub(free))
    } else {
        (0, 0)
    }
}

#[cfg(target_os = "linux")]
fn get_disk_total() -> u64 {
    get_disk_info().0
}

#[cfg(target_os = "linux")]
fn get_disk_used() -> u64 {
    get_disk_info().1
}

#[cfg(target_os = "linux")]
fn get_uptime_secs() -> u64 {
    std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
        })
        .map_or(0, |v| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                v as u64
            }
        })
}

#[cfg(target_os = "linux")]
fn get_proc_count() -> u32 {
    std::fs::read_dir("/proc").ok().map_or(0, |entries| {
        #[allow(clippy::cast_possible_truncation)]
        let n = entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|s| s.chars().all(|c| c.is_ascii_digit()))
            })
            .count() as u32;
        n
    })
}

#[cfg(target_os = "linux")]
fn get_ip_addr() -> String {
    get_ip_addr_common()
}

// ---------------------------------------------------------------------------
// Shared: IP address via getifaddrs
// ---------------------------------------------------------------------------

fn get_ip_addr_common() -> String {
    let mut addrs: *mut libc::ifaddrs = std::ptr::null_mut();
    let ret = unsafe { libc::getifaddrs(&raw mut addrs) };
    if ret != 0 || addrs.is_null() {
        return String::new();
    }
    let mut result = String::new();
    let mut cur = addrs;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        let sa = ifa.ifa_addr;
        if !sa.is_null() {
            let family = unsafe { (*sa).sa_family };
            #[allow(clippy::cast_lossless)]
            if family as i32 == libc::AF_INET {
                // Check it's not loopback
                let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) };
                if let Ok(n) = name.to_str()
                    && n != "lo"
                    && n != "lo0"
                {
                    #[allow(clippy::cast_ptr_alignment)]
                    let sin = sa.cast::<libc::sockaddr_in>();
                    let ip = unsafe { (*sin).sin_addr.s_addr };
                    let bytes = ip.to_ne_bytes();
                    result = format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]);
                    break;
                }
            }
        }
        cur = ifa.ifa_next;
    }
    unsafe { libc::freeifaddrs(addrs) };
    result
}
