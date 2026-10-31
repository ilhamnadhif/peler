//! Notices when the computer's network changes: a VPN going up or down, another Wi-Fi, a cable,
//! waking from sleep. SSH connections made before often stay open but carry nothing any more
//! (their route is gone), and TCP takes many minutes to give up on them, so the app checks
//! them right away.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;

const POLL: Duration = Duration::from_secs(2);

/// Counts network changes; read it each frame and compare.
#[derive(Clone)]
pub struct NetWatch {
    changes: Arc<AtomicU64>,
}

impl NetWatch {
    pub fn start(ctx: egui::Context) -> Self {
        let changes = Arc::new(AtomicU64::new(0));
        let counter = changes.clone();
        std::thread::Builder::new()
            .name("peler-netwatch".into())
            .spawn(move || {
                let mut last = addresses();
                let (mut mono, mut wall) = (Instant::now(), SystemTime::now());
                loop {
                    std::thread::sleep(POLL);
                    let now = addresses();
                    // The monotonic clock stops while the computer sleeps; the wall clock doesn't.
                    let slept = wall
                        .elapsed()
                        .unwrap_or_default()
                        .saturating_sub(mono.elapsed())
                        > Duration::from_secs(10);
                    (mono, wall) = (Instant::now(), SystemTime::now());
                    if now != last || slept {
                        last = now;
                        counter.fetch_add(1, Ordering::Relaxed);
                        ctx.request_repaint();
                    }
                }
            })
            .ok();
        Self { changes }
    }

    /// How many changes were seen so far.
    pub fn changes(&self) -> u64 {
        self.changes.load(Ordering::Relaxed)
    }

    /// Counts a change by hand (the dev test script).
    #[cfg(debug_assertions)]
    pub fn simulate_change(&self) {
        self.changes.fetch_add(1, Ordering::Relaxed);
    }
}

/// The addresses of the network interfaces that are up (loopback aside), with their names: a
/// VPN adds a `utun` interface, a new network brings another address.
#[cfg(unix)]
fn addresses() -> BTreeSet<(String, Vec<u8>)> {
    let mut out = BTreeSet::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list we only read and then free.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return out;
    }
    let mut cur = list;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getifaddrs returned, valid until freeifaddrs.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        let flags = ifa.ifa_flags as i32;
        if ifa.ifa_addr.is_null() || flags & libc::IFF_UP == 0 || flags & libc::IFF_LOOPBACK != 0 {
            continue;
        }
        // SAFETY: ifa_addr is non-null and points to a sockaddr of the family it names.
        let bytes = unsafe {
            match (*ifa.ifa_addr).sa_family as i32 {
                libc::AF_INET => {
                    let a = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    a.sin_addr.s_addr.to_ne_bytes().to_vec()
                }
                libc::AF_INET6 => {
                    let a = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    a.sin6_addr.s6_addr.to_vec()
                }
                _ => continue,
            }
        };
        // SAFETY: ifa_name is a NUL-terminated C string owned by the list.
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        out.insert((name, bytes));
    }
    // SAFETY: frees the list from getifaddrs exactly once; no node is used after this.
    unsafe { libc::freeifaddrs(list) };
    out
}

/// The same on Windows, from the adapter list.
#[cfg(windows)]
fn addresses() -> BTreeSet<(String, Vec<u8>)> {
    use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
        GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows_sys::Win32::Networking::WinSock::{
        AF_INET, AF_INET6, AF_UNSPEC, SOCKADDR_IN, SOCKADDR_IN6,
    };
    const SOFTWARE_LOOPBACK: u32 = 24;

    let mut out = BTreeSet::new();
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    // u64 for the alignment the adapter structures need.
    let mut buf: Vec<u64> = Vec::new();
    let mut ok = false;
    for _ in 0..3 {
        buf.resize((size as usize).div_ceil(8), 0);
        // SAFETY: `buf` holds `size` bytes, aligned for IP_ADAPTER_ADDRESSES_LH.
        let ret = unsafe {
            GetAdaptersAddresses(
                AF_UNSPEC as u32,
                flags,
                std::ptr::null(),
                buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &mut size,
            )
        };
        if ret == NO_ERROR {
            ok = true;
            break;
        }
        if ret != ERROR_BUFFER_OVERFLOW {
            break;
        }
    }
    if !ok {
        return out;
    }
    let mut cur = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list GetAdaptersAddresses wrote into `buf`, which
        // outlives this loop.
        let a = unsafe { &*cur };
        cur = a.Next;
        if a.OperStatus != IfOperStatusUp
            || a.IfType == SOFTWARE_LOOPBACK
            || a.AdapterName.is_null()
        {
            continue;
        }
        // SAFETY: AdapterName is a NUL-terminated string inside `buf`.
        let name = unsafe { std::ffi::CStr::from_ptr(a.AdapterName.cast()) }
            .to_string_lossy()
            .into_owned();
        let mut ua = a.FirstUnicastAddress;
        while !ua.is_null() {
            // SAFETY: a node of the adapter's unicast address list, inside `buf`.
            let u = unsafe { &*ua };
            ua = u.Next;
            let sa = u.Address.lpSockaddr;
            if sa.is_null() {
                continue;
            }
            // SAFETY: lpSockaddr points to a socket address of the family it names.
            let bytes = unsafe {
                match (*sa).sa_family {
                    AF_INET => (*sa.cast::<SOCKADDR_IN>())
                        .sin_addr
                        .S_un
                        .S_addr
                        .to_ne_bytes()
                        .to_vec(),
                    AF_INET6 => (*sa.cast::<SOCKADDR_IN6>()).sin6_addr.u.Byte.to_vec(),
                    _ => continue,
                }
            };
            out.insert((name.clone(), bytes));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_this_computers_addresses() {
        // Whatever the machine has, the call works and loopback is left out.
        let list = super::addresses();
        assert!(list.iter().all(|(name, _)| !name.starts_with("lo")));
    }
}
