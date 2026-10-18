//! Notices when the Mac's network changes: a VPN going up or down, another Wi-Fi, a cable,
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
                    // The monotonic clock stops while the Mac sleeps; the wall clock doesn't.
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

#[cfg(test)]
mod tests {
    #[test]
    fn lists_this_macs_addresses() {
        // Whatever the machine has, the call works and loopback is left out.
        let list = super::addresses();
        assert!(list.iter().all(|(name, _)| !name.starts_with("lo")));
    }
}
