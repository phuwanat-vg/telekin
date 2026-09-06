//! How much CPU this process has actually used.
//!
//! The pipeline's CPU ceiling is enforced against this rather than against
//! wall-clock time spent in capture and encode. Those are not the same thing,
//! and assuming they were is what made a "5% of one core" cap deliver five
//! times that:
//!
//! * the encoder runs on several threads, so a 30 ms encode can be 60 ms of
//!   CPU;
//! * QUIC encrypts and packetises every frame on the runtime's threads, which
//!   no stage timer covers;
//! * the damage watcher, the clipboard bridge and tokio itself all cost
//!   something.
//!
//! Measuring the whole process closes the loop over all of it, and keeps
//! working when a future change adds another thread.

use std::time::Duration;

/// Total CPU time consumed by this process across every thread, user plus
/// kernel. `None` when the platform cannot report it, in which case the
/// caller should fall back to pacing by frame rate alone.
pub fn process_time() -> Option<Duration> {
    imp::process_time()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::time::Duration;

    pub fn process_time() -> Option<Duration> {
        // /proc/self/stat fields 14 and 15 are utime and stime, in clock
        // ticks. The command name in field 2 can contain spaces and
        // parentheses, so parse after the last ')' rather than splitting the
        // whole line.
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let rest = &stat[stat.rfind(')')? + 1..];
        let mut fields = rest.split_whitespace();
        // Field 3 (state) onwards; utime is the 12th from here.
        let utime: u64 = fields.nth(11)?.parse().ok()?;
        let stime: u64 = fields.next()?.parse().ok()?;

        let hz = clock_ticks_per_second();
        Some(Duration::from_secs_f64((utime + stime) as f64 / hz))
    }

    fn clock_ticks_per_second() -> f64 {
        // _SC_CLK_TCK is 100 on every Linux worth supporting, but ask anyway.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks > 0 {
            ticks as f64
        } else {
            100.0
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    pub fn process_time() -> Option<Duration> {
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();

        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
            .ok()?;
        }
        // FILETIME counts 100-nanosecond intervals.
        Some(Duration::from_nanos(
            (to_u64(kernel) + to_u64(user)).saturating_mul(100),
        ))
    }

    fn to_u64(t: FILETIME) -> u64 {
        ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use std::time::Duration;

    pub fn process_time() -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_when_busy() {
        let Some(before) = process_time() else {
            return; // unsupported platform; nothing to assert
        };
        // Enough arithmetic that the clock has to tick over.
        let mut acc = 0u64;
        for i in 0..40_000_000u64 {
            acc = acc.wrapping_add(i ^ acc);
        }
        assert_ne!(acc, 0);
        let after = process_time().expect("still supported");
        assert!(
            after > before,
            "process CPU time did not advance: {before:?} -> {after:?}"
        );
    }
}
