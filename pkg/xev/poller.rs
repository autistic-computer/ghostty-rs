//! Readiness notification used by the [`super::epoll::Loop`].
//!
//! This has no direct libxev counterpart: libxev's epoll backend calls
//! `epoll_ctl`/`epoll_wait` inline. The Rust port routes those calls through
//! this small abstraction so that the same loop can also run on non-Linux
//! Unix systems (macOS, BSD) with a level-triggered `poll(2)` emulation. On
//! Linux the real epoll is always used.

use super::posix::{self, Error, Fd};

/// Event bits, using the numeric values of the Linux `EPOLL*` constants on
/// every platform (the `poll(2)` emulation translates them).
pub mod events {
    /// `EPOLLIN`
    pub const IN: u32 = 0x001;
    /// `EPOLLOUT`
    pub const OUT: u32 = 0x004;
    /// `EPOLLERR`
    pub const ERR: u32 = 0x008;
    /// `EPOLLHUP`
    pub const HUP: u32 = 0x010;
    /// `EPOLLRDHUP`
    pub const RDHUP: u32 = 0x2000;
}

/// A ready event: the token passed to [`Poller::add`] and the event bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub token: u64,
    pub events: u32,
}

#[cfg(target_os = "linux")]
pub type Poller = Epoll;
#[cfg(not(target_os = "linux"))]
pub type Poller = PollEmulation;

/// The real epoll (Linux).
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct Epoll {
    fd: Fd,
}

#[cfg(target_os = "linux")]
impl Epoll {
    pub fn new() -> Result<Self, Error> {
        // SAFETY: plain syscall.
        let rc = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if rc < 0 {
            return Err(match posix::errno() {
                libc::EMFILE => Error::ProcessFdQuotaExceeded,
                libc::ENFILE => Error::SystemFdQuotaExceeded,
                libc::ENOMEM => Error::SystemResources,
                e => posix::unexpected_errno(e),
            });
        }
        Ok(Self { fd: rc })
    }

    fn ctl(&self, op: i32, fd: Fd, ev: Option<libc::epoll_event>) -> Result<(), Error> {
        let mut ev = ev;
        let ptr = ev
            .as_mut()
            .map_or(std::ptr::null_mut(), |e| e as *mut libc::epoll_event);
        // SAFETY: `ptr` is null or points to a live epoll_event.
        let rc = unsafe { libc::epoll_ctl(self.fd, op, fd, ptr) };
        if rc == 0 {
            return Ok(());
        }
        Err(match posix::errno() {
            libc::EEXIST => Error::FileDescriptorAlreadyPresentInSet,
            libc::ELOOP => Error::OperationCausesCircularLoop,
            libc::ENOENT => Error::FileDescriptorNotRegistered,
            libc::ENOMEM => Error::SystemResources,
            libc::ENOSPC => Error::UserResourceLimitReached,
            libc::EPERM => Error::FileDescriptorIncompatibleWithEpoll,
            e => posix::unexpected_errno(e),
        })
    }

    pub fn add(&mut self, fd: Fd, events: u32, token: u64) -> Result<(), Error> {
        self.ctl(
            libc::EPOLL_CTL_ADD,
            fd,
            Some(libc::epoll_event { events, u64: token }),
        )
    }

    pub fn del(&mut self, fd: Fd) -> Result<(), Error> {
        self.ctl(libc::EPOLL_CTL_DEL, fd, None)
    }

    /// Wait for events, filling `out` (which is cleared first).
    pub fn wait(&mut self, out: &mut Vec<Event>, max: usize, timeout: i32) {
        out.clear();
        let mut evs: Vec<libc::epoll_event> = vec![libc::epoll_event { events: 0, u64: 0 }; max];
        let n = loop {
            // SAFETY: `evs` has room for `max` events.
            let rc = unsafe { libc::epoll_wait(self.fd, evs.as_mut_ptr(), max as i32, timeout) };
            if rc >= 0 {
                break rc as usize;
            }
            match posix::errno() {
                libc::EINTR => continue,
                e => unreachable!("epoll_wait failed errno={e}"),
            }
        };
        out.extend(evs[..n].iter().map(|e| Event {
            token: e.u64,
            events: e.events,
        }));
    }
}

#[cfg(target_os = "linux")]
impl Drop for Epoll {
    fn drop(&mut self) {
        posix::close(self.fd);
    }
}

/// A level-triggered emulation of epoll on top of `poll(2)`, for Unix
/// systems without epoll. Compiled everywhere so it is tested on Linux too.
#[derive(Debug, Default)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub struct PollEmulation {
    /// Registered (fd, epoll-style events, token), in registration order.
    fds: Vec<(Fd, u32, u64)>,
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
impl PollEmulation {
    pub fn new() -> Result<Self, Error> {
        Ok(Self::default())
    }

    pub fn add(&mut self, fd: Fd, events: u32, token: u64) -> Result<(), Error> {
        if self.fds.iter().any(|&(f, _, _)| f == fd) {
            return Err(Error::FileDescriptorAlreadyPresentInSet);
        }
        self.fds.push((fd, events, token));
        Ok(())
    }

    pub fn del(&mut self, fd: Fd) -> Result<(), Error> {
        let Some(i) = self.fds.iter().position(|&(f, _, _)| f == fd) else {
            return Err(Error::FileDescriptorNotRegistered);
        };
        self.fds.remove(i);
        Ok(())
    }

    pub fn wait(&mut self, out: &mut Vec<Event>, max: usize, timeout: i32) {
        out.clear();
        let mut pfds: Vec<libc::pollfd> = self
            .fds
            .iter()
            .map(|&(fd, ev, _)| {
                let mut e: libc::c_short = 0;
                if ev & (events::IN | events::RDHUP) != 0 {
                    e |= libc::POLLIN;
                }
                if ev & events::OUT != 0 {
                    e |= libc::POLLOUT;
                }
                libc::pollfd {
                    fd,
                    events: e,
                    revents: 0,
                }
            })
            .collect();

        loop {
            // SAFETY: `pfds` is a valid array of pollfd.
            let rc = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout) };
            if rc >= 0 {
                break;
            }
            match posix::errno() {
                libc::EINTR => continue,
                e => unreachable!("poll failed errno={e}"),
            }
        }

        for (pfd, &(_, _, token)) in pfds.iter().zip(self.fds.iter()) {
            if out.len() >= max {
                break;
            }
            if pfd.revents == 0 {
                continue;
            }
            let mut ev = 0;
            if pfd.revents & libc::POLLIN != 0 {
                ev |= events::IN;
            }
            if pfd.revents & libc::POLLOUT != 0 {
                ev |= events::OUT;
            }
            if pfd.revents & libc::POLLERR != 0 {
                ev |= events::ERR;
            }
            if pfd.revents & libc::POLLHUP != 0 {
                ev |= events::HUP;
            }
            out.push(Event { token, events: ev });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe() -> (Fd, Fd) {
        let mut fds = [0; 2];
        // SAFETY: valid out array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    #[test]
    fn poll_emulation_level_triggered() {
        let (r, w) = pipe();
        let mut p = PollEmulation::new().unwrap();
        p.add(r, events::IN, 7).unwrap();
        assert_eq!(
            p.add(r, events::IN, 7),
            Err(Error::FileDescriptorAlreadyPresentInSet)
        );

        let mut out = Vec::new();
        p.wait(&mut out, 16, 0);
        assert!(out.is_empty());

        posix::write(w, b"x").unwrap();
        p.wait(&mut out, 16, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].token, 7);
        assert!(out[0].events & events::IN != 0);

        // Still readable: level triggered.
        p.wait(&mut out, 16, 0);
        assert_eq!(out.len(), 1);

        let mut buf = [0u8; 8];
        posix::read(r, &mut buf).unwrap();
        p.wait(&mut out, 16, 0);
        assert!(out.is_empty());

        p.del(r).unwrap();
        assert_eq!(p.del(r), Err(Error::FileDescriptorNotRegistered));
        posix::close(r);
        posix::close(w);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn epoll_level_triggered() {
        let (r, w) = pipe();
        let mut p = Epoll::new().unwrap();
        p.add(r, events::IN, 9).unwrap();
        let mut out = Vec::new();
        p.wait(&mut out, 16, 0);
        assert!(out.is_empty());
        posix::write(w, b"x").unwrap();
        p.wait(&mut out, 16, 0);
        assert_eq!(
            out,
            vec![Event {
                token: 9,
                events: events::IN
            }]
        );
        p.del(r).unwrap();
        posix::close(r);
        posix::close(w);
    }
}
