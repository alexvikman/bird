//! The plain system calls Rust's standard library has no safe API for,
//! wrapped once so the rest of Bird has no `unsafe`. (The launchd socket
//! handover and the Linux capability drop stay in daemon.rs, next to their
//! only use, and mkdtemp in install.rs.)

/// Whether Bird runs as root (effective user id 0).
pub fn is_root() -> bool {
    // SAFETY: geteuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::geteuid() == 0 }
}

/// The real user id.
pub fn uid() -> u32 {
    // SAFETY: getuid takes no arguments, cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

/// Sends `signal` to the process `pid`, which need not be our child (std's
/// `Child::kill` only reaches our own children, and only with SIGKILL).
pub fn signal(pid: u32, signal: i32) -> std::io::Result<()> {
    // kill(0, …) signals our whole process group and kill(-1, …) every
    // process we may signal: never what a pid from a pid file should do.
    let pid = libc::pid_t::try_from(pid)
        .ok()
        .filter(|&pid| pid > 0)
        .ok_or_else(|| std::io::Error::other(format!("not a process id: {pid}")))?;
    // SAFETY: kill takes two integers and touches no memory.
    if unsafe { libc::kill(pid, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Drops every capability (ambient, effective, permitted, inheritable) of
/// the calling thread. Capabilities are per thread, so this only covers the
/// thread that runs it; it is also safe to run between fork and exec (it only
/// makes system calls), which is how php-fpm gets started without them.
#[cfg(target_os = "linux")]
pub fn drop_capabilities() -> std::io::Result<()> {
    // SAFETY: prctl with PR_CAP_AMBIENT_CLEAR_ALL takes only integers and
    // touches no memory. Its result is ignored: kernels before 4.3 have no
    // ambient capabilities to clear.
    unsafe {
        libc::prctl(
            libc::PR_CAP_AMBIENT,
            libc::PR_CAP_AMBIENT_CLEAR_ALL,
            0,
            0,
            0,
        );
    }
    // capset with an empty set: drop effective/permitted capabilities too.
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
    let mut header = Header {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let empty = || Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    };
    let data = [empty(), empty()];
    // SAFETY: header and data are #[repr(C)] in the kernel's layout for
    // version 3 (a header and two data structs), and both outlive the call.
    if unsafe { libc::syscall(libc::SYS_capset, &mut header, data.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_process_groups() {
        assert!(signal(0, 0).is_err());
        assert!(signal(u32::MAX, 0).is_err(), "-1 as an i32");
        // Signal 0 only checks that the process exists: this one does.
        signal(std::process::id(), 0).unwrap();
    }
}
