//! Resource use of the current process (CPU limit and memory checks: SPEC ch.5 §4,
//! SEM-01/SEM-06).

use std::time::Duration;

/// CPU time (user + kernel) used by this process so far.
pub fn cpu_time() -> Option<Duration> {
    imp::cpu_time()
}

/// Private memory of this process in bytes (Windows: private bytes; elsewhere: RSS).
pub fn private_bytes() -> Option<u64> {
    imp::private_bytes()
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn ticks(f: FILETIME) -> u64 {
        (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
    }

    pub fn cpu_time() -> Option<Duration> {
        let (mut c, mut e, mut k, mut u) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: the pseudo handle of the current process needs no closing; the four
        // pointers are valid, writable FILETIMEs on this stack frame.
        unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) }.ok()?;
        Some(Duration::from_nanos((ticks(k) + ticks(u)).saturating_mul(100)))
    }

    pub fn private_bytes() -> Option<u64> {
        let mut m = PROCESS_MEMORY_COUNTERS_EX::default();
        let cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        // SAFETY: `m` is a PROCESS_MEMORY_COUNTERS_EX of `cb` bytes, which the API accepts
        // through a PROCESS_MEMORY_COUNTERS pointer (the EX struct extends it).
        unsafe { GetProcessMemoryInfo(GetCurrentProcess(), (&raw mut m).cast::<PROCESS_MEMORY_COUNTERS>(), cb) }
            .ok()?;
        Some(m.PrivateUsage as u64)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::time::Duration;

    pub fn cpu_time() -> Option<Duration> {
        // utime and stime (fields 14 and 15) in clock ticks; Linux uses 100 Hz for USER_HZ.
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let rest = &stat[stat.rfind(')')? + 2..];
        let f: Vec<&str> = rest.split_whitespace().collect();
        let t: u64 = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
        Some(Duration::from_millis(t * 10))
    }

    pub fn private_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let kb: u64 = status.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_cpu_and_memory() {
        let before = super::cpu_time().unwrap();
        let mut x = 0u64;
        for i in 0..20_000_000u64 {
            x = x.wrapping_add(i * i);
        }
        assert!(x > 0);
        assert!(super::cpu_time().unwrap() >= before);
        assert!(super::private_bytes().unwrap() > 1 << 20);
    }
}
