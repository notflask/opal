//! Unix signals as a file descriptor, so the event loop can wait on them.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Block `signals` for this thread and return a signalfd that reports them.
pub fn signal_fd(signals: &[libc::c_int]) -> std::io::Result<OwnedFd> {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut mask);
        for s in signals {
            libc::sigaddset(&mut mask, *s);
        }
        if libc::pthread_sigmask(libc::SIG_BLOCK, &mask, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

/// Read every pending signal; returns their numbers.
pub fn drain(fd: &impl AsRawFd) -> Vec<i32> {
    let mut out = Vec::new();
    loop {
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        let n = unsafe {
            libc::read(fd.as_raw_fd(), (&mut info as *mut libc::signalfd_siginfo).cast(), std::mem::size_of::<libc::signalfd_siginfo>())
        };
        if n != std::mem::size_of::<libc::signalfd_siginfo>() as isize {
            return out;
        }
        out.push(info.ssi_signo as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_a_raised_signal() {
        // run on a fresh thread so blocking SIGUSR2 does not leak into other tests
        std::thread::spawn(|| {
            let fd = signal_fd(&[libc::SIGUSR2]).unwrap();
            unsafe { libc::raise(libc::SIGUSR2) };
            assert_eq!(drain(&fd), vec![libc::SIGUSR2]);
            assert!(drain(&fd).is_empty());
        })
        .join()
        .unwrap();
    }
}
