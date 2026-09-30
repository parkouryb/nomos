use std::os::unix::net::UnixStream;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PeerIdentity {
    pub pid: Option<u32>,
    pub uid: u32,
    pub gid: u32,
}

pub fn get_peer_identity(stream: &UnixStream) -> Result<PeerIdentity, std::io::Error> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let fd = stream.as_raw_fd();
        let ucred = nix::sys::socket::getsockopt(
            unsafe { &std::os::fd::BorrowedFd::borrow_raw(fd) },
            nix::sys::socket::sockopt::PeerCredentials,
        )?;
        Ok(PeerIdentity {
            pid: Some(ucred.pid() as u32),
            uid: ucred.uid(),
            gid: ucred.gid(),
        })
    }

    #[cfg(target_os = "macos")]
    {
        use std::os::unix::io::AsRawFd;
        let fd = stream.as_raw_fd();
        let mut uid: nix::libc::uid_t = 0;
        let mut gid: nix::libc::gid_t = 0;
        let res = unsafe { nix::libc::getpeereid(fd, &mut uid, &mut gid) };
        if res == 0 {
            Ok(PeerIdentity {
                pid: None,
                uid,
                gid,
            })
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Ok(PeerIdentity {
            pid: None,
            uid: 1000,
            gid: 1000,
        })
    }
}
