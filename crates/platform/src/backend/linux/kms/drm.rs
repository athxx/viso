//! DRM/KMS through the kernel's ioctls, with no libdrm: the card's
//! resources, connectors, planes and properties, dumb buffers, dma-bufs
//! imported, and framebuffers, atomic commits (and the legacy calls for a driver without
//! atomic), and the page-flip events read back from the card.

use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};

/// `_IOC` on every architecture a KMS display runs on (the generic layout:
/// arm, aarch64, x86, riscv).
const fn ioc(dir: u32, nr: u32, size: usize) -> libc::Ioctl {
    ((dir << 30) | ((size as u32) << 16) | ((b'd' as u32) << 8) | nr) as libc::Ioctl
}
const WRITE: u32 = 1;
const READ: u32 = 2;
const fn iowr<T>(nr: u32) -> libc::Ioctl {
    ioc(READ | WRITE, nr, size_of::<T>())
}
const fn iow<T>(nr: u32) -> libc::Ioctl {
    ioc(WRITE, nr, size_of::<T>())
}

pub(super) const FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
pub(super) const FORMAT_RGB565: u32 = u32::from_le_bytes(*b"RG16");
pub(super) const FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
pub(super) const FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");

const CLIENT_CAP_UNIVERSAL_PLANES: u64 = 2;
const CLIENT_CAP_ATOMIC: u64 = 3;
const CAP_DUMB_BUFFER: u64 = 1;

pub(super) const PAGE_FLIP_EVENT: u32 = 0x01;
pub(super) const ATOMIC_TEST_ONLY: u32 = 0x0100;
pub(super) const ATOMIC_NONBLOCK: u32 = 0x0200;
pub(super) const ATOMIC_ALLOW_MODESET: u32 = 0x0400;

const OBJECT_CRTC: u32 = 0xcccc_cccc;
const OBJECT_CONNECTOR: u32 = 0xc0c0_c0c0;
const OBJECT_PLANE: u32 = 0xeeee_eeee;

const CONNECTED: u32 = 1;
const MODE_TYPE_PREFERRED: u32 = 1 << 3;
const PROP_RANGE: u32 = 1 << 1;
const PROP_IMMUTABLE: u32 = 1 << 2;
const PROP_ENUM: u32 = 1 << 3;
const PROP_BITMASK: u32 = 1 << 5;

const EVENT_VBLANK: u32 = 0x01;
const EVENT_FLIP_COMPLETE: u32 = 0x02;
const EVENT_CRTC_SEQUENCE: u32 = 0x03;
const CRTC_SEQUENCE_RELATIVE: u32 = 0x01;

const CURSOR_BO: u32 = 0x01;
const CURSOR_MOVE: u32 = 0x02;

#[repr(C)]
#[derive(Default)]
struct SetClientCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Default)]
struct GetCap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Default)]
struct CardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

/// A display mode as the kernel describes it (`drm_mode_modeinfo`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ModeInfo {
    pub clock: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    pub vrefresh: u32,
    pub flags: u32,
    pub kind: u32,
    pub name: [u8; 32],
}

#[repr(C)]
#[derive(Default)]
struct Crtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: ModeInfo,
}

#[repr(C)]
#[derive(Default)]
struct GetEncoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetConnector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetPlaneRes {
    plane_id_ptr: u64,
    count_planes: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetPlane {
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    possible_crtcs: u32,
    gamma_size: u32,
    count_format_types: u32,
    format_type_ptr: u64,
}

#[repr(C)]
#[derive(Default)]
struct ObjGetProps {
    props_ptr: u64,
    prop_values_ptr: u64,
    count_props: u32,
    obj_id: u32,
    obj_type: u32,
}

#[repr(C)]
#[derive(Default)]
struct GetProperty {
    values_ptr: u64,
    enum_blob_ptr: u64,
    prop_id: u32,
    flags: u32,
    name: [u8; 32],
    count_values: u32,
    count_enum_blobs: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PropEnum {
    value: u64,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Default)]
struct FbCmd2 {
    fb_id: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    modifier: [u64; 4],
}

#[repr(C)]
#[derive(Default)]
struct CreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default)]
struct MapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Default)]
struct DestroyDumb {
    handle: u32,
}

#[repr(C)]
#[derive(Default)]
struct Atomic {
    flags: u32,
    count_objs: u32,
    objs_ptr: u64,
    count_props_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    reserved: u64,
    user_data: u64,
}

#[repr(C)]
#[derive(Default)]
struct PrimeHandle {
    handle: u32,
    flags: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Default)]
struct QueueSequence {
    crtc_id: u32,
    flags: u32,
    sequence: u64,
    user_data: u64,
}

#[repr(C)]
#[derive(Default)]
struct GemClose {
    handle: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct CreateBlob {
    data: u64,
    length: u32,
    blob_id: u32,
}

#[repr(C)]
#[derive(Default)]
struct DestroyBlob {
    blob_id: u32,
}

#[repr(C)]
#[derive(Default)]
struct PageFlip {
    crtc_id: u32,
    fb_id: u32,
    flags: u32,
    reserved: u32,
    user_data: u64,
}

/// An `FB_DAMAGE_CLIPS` rect.
#[repr(C)]
#[derive(Clone, Copy)]
struct ModeRect {
    x1: i32,
    y1: i32,
    x2: i32,
    y2: i32,
}

#[repr(C)]
#[derive(Default)]
struct Cursor {
    flags: u32,
    crtc_id: u32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    handle: u32,
}

const SET_CLIENT_CAP: libc::Ioctl = iow::<SetClientCap>(0x0d);
const GET_CAP: libc::Ioctl = iowr::<GetCap>(0x0c);
const GEM_CLOSE: libc::Ioctl = iow::<GemClose>(0x09);
const PRIME_FD_TO_HANDLE: libc::Ioctl = iowr::<PrimeHandle>(0x2e);
const CRTC_QUEUE_SEQUENCE: libc::Ioctl = iowr::<QueueSequence>(0x3c);
const MODE_GETRESOURCES: libc::Ioctl = iowr::<CardRes>(0xa0);
const MODE_SETCRTC: libc::Ioctl = iowr::<Crtc>(0xa2);
const MODE_CURSOR: libc::Ioctl = iowr::<Cursor>(0xa3);
const MODE_GETENCODER: libc::Ioctl = iowr::<GetEncoder>(0xa6);
const MODE_GETCONNECTOR: libc::Ioctl = iowr::<GetConnector>(0xa7);
const MODE_GETPROPERTY: libc::Ioctl = iowr::<GetProperty>(0xaa);
const MODE_RMFB: libc::Ioctl = iowr::<u32>(0xaf);
const MODE_PAGE_FLIP: libc::Ioctl = iowr::<PageFlip>(0xb0);
const MODE_CREATE_DUMB: libc::Ioctl = iowr::<CreateDumb>(0xb2);
const MODE_MAP_DUMB: libc::Ioctl = iowr::<MapDumb>(0xb3);
const MODE_DESTROY_DUMB: libc::Ioctl = iowr::<DestroyDumb>(0xb4);
const MODE_GETPLANERESOURCES: libc::Ioctl = iowr::<GetPlaneRes>(0xb5);
const MODE_GETPLANE: libc::Ioctl = iowr::<GetPlane>(0xb6);
const MODE_ADDFB2: libc::Ioctl = iowr::<FbCmd2>(0xb8);
const MODE_OBJ_GETPROPERTIES: libc::Ioctl = iowr::<ObjGetProps>(0xb9);
const MODE_ATOMIC: libc::Ioctl = iowr::<Atomic>(0xbc);
const MODE_CREATEPROPBLOB: libc::Ioctl = iowr::<CreateBlob>(0xbd);
const MODE_DESTROYPROPBLOB: libc::Ioctl = iowr::<DestroyBlob>(0xbe);

/// The address of `v`'s data as the kernel's `u64` pointer field.
fn ptr_of<T>(v: &mut [T]) -> u64 {
    if v.is_empty() {
        0
    } else {
        v.as_mut_ptr() as u64
    }
}

/// A NUL-padded kernel name as a string.
fn name_of(raw: &[u8; 32]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

/// A connector, as probed.
#[derive(Debug, Clone)]
pub(super) struct Connector {
    pub id: u32,
    pub connected: bool,
    pub encoder: u32,
    pub encoders: Vec<u32>,
    pub modes: Vec<ModeInfo>,
    pub mm_width: u32,
}

/// A plane, as probed.
#[derive(Debug, Clone)]
pub(super) struct Plane {
    pub id: u32,
    pub possible_crtcs: u32,
    pub formats: Vec<u32>,
}

/// A property's name and, for an enum or bitmask, its values by name.
#[derive(Debug, Clone)]
pub(super) struct Property {
    pub id: u32,
    pub name: String,
    pub enums: Vec<(u64, String)>,
    /// A range's least and greatest values.
    pub range: Option<(u64, u64)>,
    /// Whether only the kernel sets it.
    pub immutable: bool,
}

/// One page flip that completed, or a refresh asked for that came.
#[derive(Debug, Clone, Copy)]
pub(super) struct Flip {
    pub crtc: u32,
    /// A refresh asked for ([`Card::queue_refresh`]), not a flip.
    pub refresh: bool,
}

/// An open DRM card.
pub(super) struct Card {
    fd: OwnedFd,
    /// Atomic modesetting is available (and enabled).
    pub atomic: bool,
}

impl AsRawFd for Card {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Card {
    /// Adopts a card descriptor (opened by the seat).
    pub fn from_fd(fd: OwnedFd) -> io::Result<Card> {
        let mut card = Card { fd, atomic: false };
        let mut dumb = GetCap {
            capability: CAP_DUMB_BUFFER,
            value: 0,
        };
        card.ioctl(GET_CAP, &mut dumb)?;
        let mut cap = SetClientCap {
            capability: CLIENT_CAP_UNIVERSAL_PLANES,
            value: 1,
        };
        let planes = card.ioctl(SET_CLIENT_CAP, &mut cap).is_ok();
        let mut cap = SetClientCap {
            capability: CLIENT_CAP_ATOMIC,
            value: 1,
        };
        card.atomic = planes
            && std::env::var_os("VISO_KMS_LEGACY").is_none()
            && card.ioctl(SET_CLIENT_CAP, &mut cap).is_ok();
        Ok(card)
    }

    fn ioctl<T>(&self, request: libc::Ioctl, arg: &mut T) -> io::Result<()> {
        loop {
            // SAFETY: `request` encodes `T`'s size and direction, and `arg`
            // is a live, exclusively borrowed `T` for the call; the pointer
            // fields inside it point at live buffers of the counts given.
            let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), request, std::ptr::from_mut(arg)) };
            if r == 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            if !matches!(e.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) {
                return Err(e);
            }
        }
    }

    /// The card's CRTCs and connectors.
    pub fn resources(&self) -> io::Result<(Vec<u32>, Vec<u32>)> {
        loop {
            let mut res = CardRes::default();
            self.ioctl(MODE_GETRESOURCES, &mut res)?;
            let (nc, nk) = (res.count_crtcs, res.count_connectors);
            let mut crtcs = vec![0u32; nc as usize];
            let mut connectors = vec![0u32; nk as usize];
            let mut res = CardRes {
                crtc_id_ptr: ptr_of(&mut crtcs),
                connector_id_ptr: ptr_of(&mut connectors),
                count_crtcs: nc,
                count_connectors: nk,
                ..CardRes::default()
            };
            self.ioctl(MODE_GETRESOURCES, &mut res)?;
            // A hotplug between the calls changes the counts: ask again.
            if res.count_crtcs == nc && res.count_connectors == nk {
                return Ok((crtcs, connectors));
            }
        }
    }

    pub fn connector(&self, id: u32) -> io::Result<Connector> {
        loop {
            let mut c = GetConnector {
                connector_id: id,
                ..GetConnector::default()
            };
            self.ioctl(MODE_GETCONNECTOR, &mut c)?;
            let (nm, ne) = (c.count_modes, c.count_encoders);
            let mut modes = vec![ModeInfo::default(); nm as usize];
            let mut encoders = vec![0u32; ne as usize];
            let mut c = GetConnector {
                connector_id: id,
                modes_ptr: ptr_of(&mut modes),
                encoders_ptr: ptr_of(&mut encoders),
                count_modes: nm,
                count_encoders: ne,
                ..GetConnector::default()
            };
            self.ioctl(MODE_GETCONNECTOR, &mut c)?;
            if c.count_modes == nm && c.count_encoders == ne {
                return Ok(Connector {
                    id,
                    connected: c.connection == CONNECTED,
                    encoder: c.encoder_id,
                    encoders,
                    modes,
                    mm_width: c.mm_width,
                });
            }
        }
    }

    /// An encoder's current CRTC and the CRTCs it can drive (a bit per index
    /// in [`resources`](Self::resources)).
    pub fn encoder(&self, id: u32) -> io::Result<(u32, u32)> {
        let mut e = GetEncoder {
            encoder_id: id,
            ..GetEncoder::default()
        };
        self.ioctl(MODE_GETENCODER, &mut e)?;
        Ok((e.crtc_id, e.possible_crtcs))
    }

    pub fn planes(&self) -> io::Result<Vec<Plane>> {
        let mut res = GetPlaneRes::default();
        self.ioctl(MODE_GETPLANERESOURCES, &mut res)?;
        let mut ids = vec![0u32; res.count_planes as usize];
        let mut res = GetPlaneRes {
            plane_id_ptr: ptr_of(&mut ids),
            count_planes: ids.len() as u32,
        };
        self.ioctl(MODE_GETPLANERESOURCES, &mut res)?;
        ids.truncate(res.count_planes as usize);
        let mut planes = Vec::with_capacity(ids.len());
        for id in ids {
            let mut p = GetPlane {
                plane_id: id,
                ..GetPlane::default()
            };
            self.ioctl(MODE_GETPLANE, &mut p)?;
            let mut formats = vec![0u32; p.count_format_types as usize];
            let mut p = GetPlane {
                plane_id: id,
                count_format_types: formats.len() as u32,
                format_type_ptr: ptr_of(&mut formats),
                ..GetPlane::default()
            };
            self.ioctl(MODE_GETPLANE, &mut p)?;
            planes.push(Plane {
                id,
                possible_crtcs: p.possible_crtcs,
                formats,
            });
        }
        Ok(planes)
    }

    fn object_properties(&self, obj: u32, kind: u32) -> io::Result<Vec<(u32, u64)>> {
        let mut q = ObjGetProps {
            obj_id: obj,
            obj_type: kind,
            ..ObjGetProps::default()
        };
        self.ioctl(MODE_OBJ_GETPROPERTIES, &mut q)?;
        let n = q.count_props as usize;
        let mut ids = vec![0u32; n];
        let mut values = vec![0u64; n];
        let mut q = ObjGetProps {
            props_ptr: ptr_of(&mut ids),
            prop_values_ptr: ptr_of(&mut values),
            count_props: n as u32,
            obj_id: obj,
            obj_type: kind,
        };
        self.ioctl(MODE_OBJ_GETPROPERTIES, &mut q)?;
        let n = (q.count_props as usize).min(n);
        Ok(ids.into_iter().zip(values).take(n).collect())
    }

    fn property(&self, id: u32) -> io::Result<Property> {
        let mut p = GetProperty {
            prop_id: id,
            ..GetProperty::default()
        };
        self.ioctl(MODE_GETPROPERTY, &mut p)?;
        let mut enums = Vec::new();
        if p.flags & (PROP_ENUM | PROP_BITMASK) != 0 && p.count_enum_blobs > 0 {
            let mut blobs = vec![PropEnum::default(); p.count_enum_blobs as usize];
            let mut values = vec![0u64; p.count_values as usize];
            let mut q = GetProperty {
                prop_id: id,
                values_ptr: ptr_of(&mut values),
                enum_blob_ptr: ptr_of(&mut blobs),
                count_values: values.len() as u32,
                count_enum_blobs: blobs.len() as u32,
                ..GetProperty::default()
            };
            self.ioctl(MODE_GETPROPERTY, &mut q)?;
            enums = blobs.iter().map(|e| (e.value, name_of(&e.name))).collect();
        }
        let mut range = None;
        if p.flags & PROP_RANGE != 0 && p.count_values == 2 {
            let mut values = [0u64; 2];
            let mut q = GetProperty {
                prop_id: id,
                values_ptr: ptr_of(&mut values),
                count_values: 2,
                ..GetProperty::default()
            };
            self.ioctl(MODE_GETPROPERTY, &mut q)?;
            range = Some((values[0], values[1]));
        }
        Ok(Property {
            id,
            name: name_of(&p.name),
            enums,
            range,
            immutable: p.flags & PROP_IMMUTABLE != 0,
        })
    }

    /// Every property of an object, with its current value.
    fn properties(&self, obj: u32, kind: u32) -> io::Result<Vec<(Property, u64)>> {
        self.object_properties(obj, kind)?
            .into_iter()
            .map(|(id, value)| Ok((self.property(id)?, value)))
            .collect()
    }

    pub fn crtc_properties(&self, crtc: u32) -> io::Result<Vec<(Property, u64)>> {
        self.properties(crtc, OBJECT_CRTC)
    }

    pub fn connector_properties(&self, connector: u32) -> io::Result<Vec<(Property, u64)>> {
        self.properties(connector, OBJECT_CONNECTOR)
    }

    pub fn plane_properties(&self, plane: u32) -> io::Result<Vec<(Property, u64)>> {
        self.properties(plane, OBJECT_PLANE)
    }

    pub fn create_blob<T>(&self, data: &[T]) -> io::Result<u32> {
        let mut b = CreateBlob {
            data: data.as_ptr() as u64,
            length: size_of_val(data) as u32,
            blob_id: 0,
        };
        self.ioctl(MODE_CREATEPROPBLOB, &mut b)?;
        Ok(b.blob_id)
    }

    pub fn destroy_blob(&self, id: u32) {
        let _ = self.ioctl(MODE_DESTROYPROPBLOB, &mut DestroyBlob { blob_id: id });
    }

    /// A blob of `FB_DAMAGE_CLIPS` for `rects` (`(x, y, w, h)`).
    pub fn damage_blob(&self, rects: &[(u32, u32, u32, u32)]) -> io::Result<u32> {
        let clips: Vec<ModeRect> = rects
            .iter()
            .map(|&(x, y, w, h)| ModeRect {
                x1: x as i32,
                y1: y as i32,
                x2: (x + w) as i32,
                y2: (y + h) as i32,
            })
            .collect();
        self.create_blob(&clips)
    }

    /// A framebuffer over buffer object `handle`.
    pub fn add_fb(
        &self,
        width: u32,
        height: u32,
        format: u32,
        handle: u32,
        pitch: u32,
    ) -> io::Result<u32> {
        let mut f = FbCmd2 {
            width,
            height,
            pixel_format: format,
            handles: [handle, 0, 0, 0],
            pitches: [pitch, 0, 0, 0],
            ..FbCmd2::default()
        };
        self.ioctl(MODE_ADDFB2, &mut f)?;
        Ok(f.fb_id)
    }

    /// A framebuffer of `format` over up to four planes, each a buffer
    /// object's handle, pitch and byte offset (unused ones zero).
    pub fn add_fb_planes(
        &self,
        (width, height): (u32, u32),
        format: u32,
        handles: [u32; 4],
        pitches: [u32; 4],
        offsets: [u32; 4],
    ) -> io::Result<u32> {
        let mut f = FbCmd2 {
            width,
            height,
            pixel_format: format,
            handles,
            pitches,
            offsets,
            ..FbCmd2::default()
        };
        self.ioctl(MODE_ADDFB2, &mut f)?;
        Ok(f.fb_id)
    }

    /// A buffer object of the card's over dma-buf `fd`, which stays the
    /// caller's; let it go with [`close_handle`](Self::close_handle). The
    /// same dma-buf imported again yields the same handle.
    pub fn import_dma_buf(&self, fd: RawFd) -> io::Result<u32> {
        let mut p = PrimeHandle {
            fd,
            ..PrimeHandle::default()
        };
        self.ioctl(PRIME_FD_TO_HANDLE, &mut p)?;
        Ok(p.handle)
    }

    pub fn close_handle(&self, handle: u32) {
        let _ = self.ioctl(
            GEM_CLOSE,
            &mut GemClose {
                handle,
                ..GemClose::default()
            },
        );
    }

    pub fn remove_fb(&self, fb: u32) {
        let mut fb = fb;
        let _ = self.ioctl(MODE_RMFB, &mut fb);
    }

    /// A dumb buffer: its handle, pitch and byte size.
    pub fn create_dumb(&self, width: u32, height: u32, bpp: u32) -> io::Result<(u32, u32, u64)> {
        let mut d = CreateDumb {
            width,
            height,
            bpp,
            ..CreateDumb::default()
        };
        self.ioctl(MODE_CREATE_DUMB, &mut d)?;
        Ok((d.handle, d.pitch, d.size))
    }

    /// The `mmap` offset of dumb buffer `handle`.
    pub fn map_dumb(&self, handle: u32) -> io::Result<u64> {
        let mut m = MapDumb {
            handle,
            ..MapDumb::default()
        };
        self.ioctl(MODE_MAP_DUMB, &mut m)?;
        Ok(m.offset)
    }

    pub fn destroy_dumb(&self, handle: u32) {
        let _ = self.ioctl(MODE_DESTROY_DUMB, &mut DestroyDumb { handle });
    }

    /// One atomic commit.
    pub fn commit(&self, req: &Request, flags: u32) -> io::Result<()> {
        let mut objs = Vec::new();
        let mut counts: Vec<u32> = Vec::new();
        let mut props = Vec::with_capacity(req.0.len());
        let mut values = Vec::with_capacity(req.0.len());
        for &(obj, prop, value) in &req.0 {
            if objs.last() != Some(&obj) {
                objs.push(obj);
                counts.push(0);
            }
            *counts.last_mut().expect("pushed above") += 1;
            props.push(prop);
            values.push(value);
        }
        let mut a = Atomic {
            flags,
            count_objs: objs.len() as u32,
            objs_ptr: ptr_of(&mut objs),
            count_props_ptr: ptr_of(&mut counts),
            props_ptr: ptr_of(&mut props),
            prop_values_ptr: ptr_of(&mut values),
            ..Atomic::default()
        };
        self.ioctl(MODE_ATOMIC, &mut a)
    }

    /// The legacy modeset: scan out `fb` on `crtc` through `connector`.
    pub fn set_crtc(&self, crtc: u32, fb: u32, connector: u32, mode: ModeInfo) -> io::Result<()> {
        let mut connectors = [connector];
        let mut c = Crtc {
            set_connectors_ptr: ptr_of(&mut connectors),
            count_connectors: 1,
            crtc_id: crtc,
            fb_id: fb,
            mode_valid: 1,
            mode,
            ..Crtc::default()
        };
        self.ioctl(MODE_SETCRTC, &mut c)
    }

    /// The legacy page flip, with a completion event.
    pub fn page_flip(&self, crtc: u32, fb: u32) -> io::Result<()> {
        let mut f = PageFlip {
            crtc_id: crtc,
            fb_id: fb,
            flags: PAGE_FLIP_EVENT,
            ..PageFlip::default()
        };
        self.ioctl(MODE_PAGE_FLIP, &mut f)
    }

    /// Asks for an event at `crtc`'s next refresh, read back as a
    /// [`Flip`] that is a `refresh`.
    pub fn queue_refresh(&self, crtc: u32) -> io::Result<()> {
        let mut q = QueueSequence {
            crtc_id: crtc,
            flags: CRTC_SEQUENCE_RELATIVE,
            sequence: 1,
            user_data: u64::from(crtc),
        };
        self.ioctl(CRTC_QUEUE_SEQUENCE, &mut q)
    }

    /// Shows dumb buffer `handle` (a `w`×`h` ARGB8888 image; `0` hides it)
    /// as `crtc`'s cursor.
    pub fn set_cursor(&self, crtc: u32, handle: u32, w: u32, h: u32) -> io::Result<()> {
        let mut c = Cursor {
            flags: CURSOR_BO,
            crtc_id: crtc,
            width: w,
            height: h,
            handle,
            ..Cursor::default()
        };
        self.ioctl(MODE_CURSOR, &mut c)
    }

    pub fn move_cursor(&self, crtc: u32, x: i32, y: i32) {
        let mut c = Cursor {
            flags: CURSOR_MOVE,
            crtc_id: crtc,
            x,
            y,
            ..Cursor::default()
        };
        let _ = self.ioctl(MODE_CURSOR, &mut c);
    }

    /// Reads the card's pending events, calling `f` for each completed flip.
    pub fn read_events(&self, mut f: impl FnMut(Flip)) {
        let mut buf = [0u8; 1024];
        loop {
            // SAFETY: reading into a live local buffer of its length from the
            // card's non-blocking descriptor.
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                return;
            }
            let mut at = 0usize;
            let n = n as usize;
            // Each event: type, length, then (for vblank / flip) user data,
            // time, sequence and the CRTC.
            while at + 8 <= n {
                let word =
                    |o: usize| u32::from_ne_bytes(buf[o..o + 4].try_into().expect("4 bytes"));
                let (kind, len) = (word(at), word(at + 4) as usize);
                if len < 8 || at + len > n {
                    break;
                }
                if matches!(kind, EVENT_VBLANK | EVENT_FLIP_COMPLETE) && len >= 32 {
                    f(Flip {
                        crtc: word(at + 28),
                        refresh: false,
                    });
                } else if kind == EVENT_CRTC_SEQUENCE && len >= 32 {
                    f(Flip {
                        crtc: word(at + 8),
                        refresh: true,
                    });
                }
                at += len;
            }
        }
    }
}

/// An atomic request: `(object, property, value)` rows, each object's rows
/// together.
#[derive(Default)]
pub(super) struct Request(Vec<(u32, u32, u64)>);

impl Request {
    pub fn set(&mut self, obj: u32, prop: u32, value: u64) {
        let at = self
            .0
            .iter()
            .rposition(|r| r.0 == obj)
            .map_or(self.0.len(), |i| i + 1);
        self.0.insert(at, (obj, prop, value));
    }
}

/// The mode a connector should run: `VISO_KMS_MODE` (`1280x720`, or
/// `1280x720@60`) when it names one, else the preferred mode, else the first.
pub(super) fn pick_mode(modes: &[ModeInfo], want: Option<&str>) -> Option<ModeInfo> {
    if let Some(((w, h), rate)) = want.and_then(crate::panel::parse_mode)
        && let Some(m) = modes
            .iter()
            .find(|m| m.hdisplay == w && m.vdisplay == h && rate.is_none_or(|r| m.vrefresh == r))
    {
        return Some(*m);
    }
    modes
        .iter()
        .find(|m| m.kind & MODE_TYPE_PREFERRED != 0)
        .or_else(|| modes.first())
        .copied()
}

/// The property of `props` called `name`.
pub(super) fn find<'p>(props: &'p [(Property, u64)], name: &str) -> Option<&'p (Property, u64)> {
    props.iter().find(|(p, _)| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_kernel_abi() {
        assert_eq!(size_of::<ModeInfo>(), 68);
        assert_eq!(size_of::<CardRes>(), 64);
        assert_eq!(size_of::<Crtc>(), 104);
        assert_eq!(size_of::<GetConnector>(), 80);
        assert_eq!(size_of::<GetPlaneRes>(), 16);
        assert_eq!(size_of::<GetPlane>(), 32);
        assert_eq!(size_of::<ObjGetProps>(), 32);
        assert_eq!(size_of::<GetProperty>(), 64);
        assert_eq!(size_of::<PropEnum>(), 40);
        assert_eq!(size_of::<FbCmd2>(), 104);
        assert_eq!(size_of::<CreateDumb>(), 32);
        assert_eq!(size_of::<Atomic>(), 56);
        assert_eq!(size_of::<PageFlip>(), 24);
        assert_eq!(size_of::<Cursor>(), 28);
        assert_eq!(size_of::<PrimeHandle>(), 12);
        assert_eq!(size_of::<GemClose>(), 8);
        assert_eq!(size_of::<QueueSequence>(), 24);
    }

    #[test]
    // `Ioctl` is a `c_ulong` with glibc and a `c_int` with musl.
    #[allow(clippy::unnecessary_cast)]
    fn ioctl_numbers_match_the_kernel_headers() {
        assert_eq!(MODE_GETRESOURCES as u64, 0xc040_64a0);
        assert_eq!(MODE_ADDFB2 as u64, 0xc068_64b8);
        assert_eq!(MODE_ATOMIC as u64, 0xc038_64bc);
        assert_eq!(MODE_CREATE_DUMB as u64, 0xc020_64b2);
        assert_eq!(SET_CLIENT_CAP as u64, 0x4010_640d);
        assert_eq!(PRIME_FD_TO_HANDLE as u64, 0xc00c_642e);
        assert_eq!(GEM_CLOSE as u64, 0x4008_6409);
        assert_eq!(CRTC_QUEUE_SEQUENCE as u64, 0xc018_643c);
    }

    #[test]
    fn a_named_mode_wins_then_the_preferred_one() {
        let mode = |w: u16, h: u16, r: u32, preferred: bool| ModeInfo {
            hdisplay: w,
            vdisplay: h,
            vrefresh: r,
            kind: if preferred { MODE_TYPE_PREFERRED } else { 0 },
            ..ModeInfo::default()
        };
        let modes = [
            mode(1920, 1080, 60, true),
            mode(1280, 720, 60, false),
            mode(1280, 720, 50, false),
        ];
        assert_eq!(pick_mode(&modes, None).unwrap().hdisplay, 1920);
        assert_eq!(pick_mode(&modes, Some("1280x720")).unwrap().vrefresh, 60);
        assert_eq!(pick_mode(&modes, Some("1280x720@50")).unwrap().vrefresh, 50);
        assert_eq!(pick_mode(&modes, Some("640x480")).unwrap().hdisplay, 1920);
    }

    #[test]
    fn an_atomic_request_keeps_each_objects_rows_together() {
        let mut r = Request::default();
        r.set(1, 10, 0);
        r.set(2, 20, 0);
        r.set(1, 11, 0);
        assert_eq!(r.0.iter().map(|x| x.0).collect::<Vec<_>>(), [1, 1, 2]);
    }
}
