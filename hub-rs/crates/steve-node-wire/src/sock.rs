//! Socket options of a link.

/// The unsent bytes a link's socket holds at most before it takes more ([`low_water`]).
pub const LOW_WATER: u32 = 32 * 1024;

/// Sets `TCP_NOTSENT_LOWAT` where the platform has it (Linux, macOS): the kernel then keeps only
/// about [`LOW_WATER`] bytes not yet sent, so the link's writer picks its next frame (a permission
/// answer before the next file chunk) at the last moment instead of queueing it behind megabytes
/// already in the send buffer. Elsewhere, and if the option is refused, nothing changes.
pub fn low_water(s: &tokio::net::TcpStream) {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        use std::os::fd::AsRawFd;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const TCP_NOTSENT_LOWAT: libc::c_int = libc::TCP_NOTSENT_LOWAT;
        // <netinet/tcp.h>
        #[cfg(target_vendor = "apple")]
        const TCP_NOTSENT_LOWAT: libc::c_int = 0x201;
        let v: libc::c_int = LOW_WATER as libc::c_int;
        // SAFETY: a valid socket and a c_int option value of the right size
        let r = unsafe {
            libc::setsockopt(
                s.as_raw_fd(),
                libc::IPPROTO_TCP,
                TCP_NOTSENT_LOWAT,
                (&v as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if r != 0 {
            tracing::debug!("TCP_NOTSENT_LOWAT: {}", std::io::Error::last_os_error());
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    let _ = s;
}

/// What [`low_water`] set on `s`, if the platform has the option.
pub fn low_water_of(s: &tokio::net::TcpStream) -> Option<u32> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        use std::os::fd::AsRawFd;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        const TCP_NOTSENT_LOWAT: libc::c_int = libc::TCP_NOTSENT_LOWAT;
        #[cfg(target_vendor = "apple")]
        const TCP_NOTSENT_LOWAT: libc::c_int = 0x201;
        let mut v: libc::c_int = 0;
        let mut n = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        // SAFETY: a valid socket and room for a c_int
        let r = unsafe {
            libc::getsockopt(
                s.as_raw_fd(),
                libc::IPPROTO_TCP,
                TCP_NOTSENT_LOWAT,
                (&mut v as *mut libc::c_int).cast(),
                &mut n,
            )
        };
        (r == 0).then_some(v as u32)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        let _ = s;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_links_socket_keeps_little_unsent() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = tokio::net::TcpStream::connect(l.local_addr().unwrap())
            .await
            .unwrap();
        low_water(&c);
        if cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_vendor = "apple"
        )) {
            assert_eq!(low_water_of(&c), Some(LOW_WATER));
        }
    }
}
