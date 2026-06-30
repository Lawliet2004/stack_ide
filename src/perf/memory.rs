//! Platform-specific resident set size (RSS) measurement.
//!
//! # Sampling policy
//! Callers are responsible for rate-limiting. `App` polls at most once per
//! 2 seconds using an `Instant`-based gate so `/proc/self/status` is never
//! read on every frame.
//!
//! # Supported platforms
//! - **Linux** — reads `VmRSS` from `/proc/self/status`.
//! - **macOS** — calls `task_info(TASK_BASIC_INFO)` via `libc`.
//! - **Windows** — calls `GetProcessMemoryInfo` (psapi).
//! - **Other** — returns `None`.

// ─── Public API ───────────────────────────────────────────────────────────────

/// Return the current resident set size of this process in bytes, or `None`
/// if the platform is unsupported or the query fails.
pub fn get_rss_bytes() -> Option<u64> {
    platform::rss()
}

/// Format a byte count as a human-readable string (B / KB / MB / GB).
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{} B", bytes)
    }
}

// ─── Platform implementations ─────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod platform {
    pub fn rss() -> Option<u64> {
        let text = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                // Format: "VmRSS:  1234 kB"
                let kb: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())?;
                return Some(kb * 1024);
            }
        }
        None
    }
}

#[cfg(target_os = "macos")]
mod platform {
    pub fn rss() -> Option<u64> {
        // Use mach task_info to read resident size.
        // TASK_BASIC_INFO_COUNT = 29; task_basic_info.resident_size is at offset 8 bytes.
        // We call via the raw libc bindings since the `mach` crate is heavyweight.
        // Safety: calling task_info with TASK_BASIC_INFO on mach_task_self() is always
        // valid; we check the return value before reading the output.
        use std::mem;

        // Equivalent to the C struct task_basic_info (mach/task_info.h).
        // We only need resident_size which is the 2nd field (after virtual_size).
        #[repr(C)]
        struct TaskBasicInfo {
            virtual_size: u64,
            resident_size: u64,
            _rest: [u32; 25],
        }

        extern "C" {
            fn mach_task_self() -> u32;
            fn task_info(
                task: u32,
                flavor: u32,
                task_info_out: *mut i32,
                task_info_count: *mut u32,
            ) -> i32;
        }

        const TASK_BASIC_INFO: u32 = 5;
        const TASK_BASIC_INFO_COUNT: u32 =
            (mem::size_of::<TaskBasicInfo>() / mem::size_of::<i32>()) as u32;

        let mut info: TaskBasicInfo = unsafe { mem::zeroed() };
        let mut count = TASK_BASIC_INFO_COUNT;
        let ret = unsafe {
            task_info(
                mach_task_self(),
                TASK_BASIC_INFO,
                &mut info as *mut _ as *mut i32,
                &mut count,
            )
        };
        if ret == 0 {
            Some(info.resident_size)
        } else {
            None
        }
    }
}

#[cfg(windows)]
mod platform {
    pub fn rss() -> Option<u64> {
        use std::mem;

        // PROCESS_MEMORY_COUNTERS from psapi.h
        #[repr(C)]
        #[allow(non_snake_case)]
        struct ProcessMemoryCounters {
            cb: u32,
            PageFaultCount: u32,
            PeakWorkingSetSize: usize,
            WorkingSetSize: usize,
            QuotaPeakPagedPoolUsage: usize,
            QuotaPagedPoolUsage: usize,
            QuotaPeakNonPagedPoolUsage: usize,
            QuotaNonPagedPoolUsage: usize,
            PagefileUsage: usize,
            PeakPagefileUsage: usize,
        }

        extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn GetProcessMemoryInfo(
                hprocess: *mut std::ffi::c_void,
                ppsmemcounters: *mut ProcessMemoryCounters,
                cb: u32,
            ) -> i32;
        }

        let mut pmc: ProcessMemoryCounters = unsafe { mem::zeroed() };
        pmc.cb = mem::size_of::<ProcessMemoryCounters>() as u32;
        let ret = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut pmc,
                mem::size_of::<ProcessMemoryCounters>() as u32,
            )
        };
        if ret != 0 {
            Some(pmc.WorkingSetSize as u64)
        } else {
            None
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod platform {
    pub fn rss() -> Option<u64> {
        None
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_scales_correctly() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(1536 * 1024), "1.5 MB");
        // GB boundary
        let two_gb = 2u64 * 1024 * 1024 * 1024;
        assert!(format_bytes(two_gb).contains("GB"));
    }

    #[test]
    fn get_rss_bytes_returns_nonzero_on_supported_platform() {
        // On supported platforms (Linux, macOS, Windows) this should return Some.
        // On unsupported platforms it legitimately returns None.
        if let Some(rss) = get_rss_bytes() {
            // The IDE must use at least 1 MB of memory.
            assert!(rss > 1024 * 1024, "RSS suspiciously small: {rss} bytes");
        }
        // None is acceptable on unsupported platforms — no assertion needed.
    }
}
