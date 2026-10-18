//! Device hotplug from the kernel's uevent netlink broadcast: input devices
//! arriving and leaving, and connector changes on the card. No udev daemon
//! is needed.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;

const NETLINK_KOBJECT_UEVENT: libc::c_int = 15;
/// The kernel's own broadcast group (udev's re-broadcast is group 2).
const KERNEL_GROUP: u32 = 1;

/// A hotplug event the display acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Hotplug {
    InputAdded(PathBuf),
    InputRemoved(PathBuf),
    /// A card's connectors changed.
    Display,
}

pub(super) struct Uevents {
    fd: OwnedFd,
}

impl AsRawFd for Uevents {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Uevents {
    pub fn open() -> io::Result<Uevents> {
        // SAFETY: creating a socket; the descriptor is owned below.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                NETLINK_KOBJECT_UEVENT,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh descriptor nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: an all-zero `sockaddr_nl` is valid; the fields set below
        // select the kernel's group.
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        addr.nl_groups = KERNEL_GROUP;
        // SAFETY: binding the socket to a live, initialized address of its size.
        let r = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                std::ptr::from_ref(&addr).cast(),
                size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Uevents { fd })
    }

    /// Every pending event the display acts on.
    pub fn read(&self, out: &mut Vec<Hotplug>) {
        let mut buf = [0u8; 8192];
        loop {
            // SAFETY: receiving into a live local buffer of its length from
            // the non-blocking socket.
            let n =
                unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                return;
            }
            out.extend(parse(&buf[..n as usize]));
        }
    }
}

/// One uevent message: `action@devpath`, then `KEY=value` fields, each
/// NUL-terminated.
pub(super) fn parse(msg: &[u8]) -> Option<Hotplug> {
    let mut action = None;
    let mut subsystem = None;
    let mut devname = None;
    let mut hotplug = false;
    for field in msg.split(|&b| b == 0).skip(1) {
        let Ok(field) = std::str::from_utf8(field) else {
            continue;
        };
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "ACTION" => action = Some(value),
            "SUBSYSTEM" => subsystem = Some(value),
            "DEVNAME" => devname = Some(value),
            "HOTPLUG" => hotplug = value == "1",
            _ => {}
        }
    }
    match (subsystem?, action?) {
        ("drm", "change") if hotplug => Some(Hotplug::Display),
        ("input", action @ ("add" | "remove")) => {
            let name = devname?;
            let leaf = name.rsplit('/').next()?;
            if !leaf.starts_with("event") {
                return None;
            }
            let path = PathBuf::from("/dev").join(name);
            Some(if action == "add" {
                Hotplug::InputAdded(path)
            } else {
                Hotplug::InputRemoved(path)
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_and_connector_events_parse() {
        let add = b"add@/devices/platform/i2c/input/input3/event3\0ACTION=add\0DEVPATH=/devices/x\0SUBSYSTEM=input\0MAJOR=13\0DEVNAME=input/event3\0SEQNUM=1\0";
        assert_eq!(
            parse(add),
            Some(Hotplug::InputAdded(PathBuf::from("/dev/input/event3")))
        );
        let parent = b"add@/devices/x/input3\0ACTION=add\0SUBSYSTEM=input\0";
        assert_eq!(parse(parent), None);
        let mouse = b"add@/x\0ACTION=add\0SUBSYSTEM=input\0DEVNAME=input/mouse0\0";
        assert_eq!(parse(mouse), None);
        let card = b"change@/devices/x/drm/card0\0ACTION=change\0SUBSYSTEM=drm\0HOTPLUG=1\0DEVNAME=dri/card0\0";
        assert_eq!(parse(card), Some(Hotplug::Display));
        let gone = b"remove@/x\0ACTION=remove\0SUBSYSTEM=input\0DEVNAME=input/event7\0";
        assert_eq!(
            parse(gone),
            Some(Hotplug::InputRemoved(PathBuf::from("/dev/input/event7")))
        );
    }
}
