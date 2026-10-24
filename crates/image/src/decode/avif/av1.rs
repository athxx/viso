//! The AV1 decoder an AVIF's items and samples go through: one rav1d
//! context, fed a temporal unit at a time, its picture borrowed until the
//! next.

use std::ffi::c_int;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ptr::{self, NonNull};

use rav1d::Dav1dResult;
use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422,
    DAV1D_PIXEL_LAYOUT_I444,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture,
    dav1d_open, dav1d_picture_unref, dav1d_send_data,
};

use crate::raster::{AssetError, MAX_PIXELS};

/// The `-EAGAIN` rav1d answers when it wants its picture taken before it
/// takes more data, or more data before it has a picture: the platform C
/// library's value.
#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
const EAGAIN: c_int = -35;
#[cfg(not(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
const EAGAIN: c_int = -11;

fn failed(what: &str, res: Dav1dResult) -> AssetError {
    AssetError::Decode(format!("the AV1 decoder {what} ({})", res.0))
}

/// A rav1d context and the picture it last decoded.
pub(crate) struct Av1 {
    ctx: Option<Dav1dContext>,
    picture: Option<Dav1dPicture>,
}

// SAFETY: the context and the picture are this value's alone — nothing else
// holds a reference to either — and the decoder's API may be called from any
// thread as long as no two calls on one context overlap, which `&mut self` on
// every call guarantees; its own worker threads are joined when it closes.
unsafe impl Send for Av1 {}

impl Av1 {
    /// A decoder running `threads` threads, refusing frames past the
    /// raster limit.
    pub(crate) fn new(threads: usize) -> Result<Av1, AssetError> {
        let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
        // SAFETY: `dav1d_default_settings` writes every field of the
        // settings it is given, so they are initialized after it.
        let settings = unsafe {
            dav1d_default_settings(NonNull::from(&mut settings).cast());
            settings.assume_init_mut()
        };
        settings.n_threads = threads.clamp(1, 64) as c_int;
        // One frame in flight: each picture is out as its data goes in.
        settings.max_frame_delay = 1;
        // Film grain is part of the picture the encoder meant.
        settings.apply_grain = 1;
        settings.frame_size_limit = MAX_PIXELS as u32;
        let mut ctx = None;
        // SAFETY: both pointers are to live locals; on success `ctx` holds
        // a context this decoder owns and closes on drop.
        let res =
            unsafe { dav1d_open(Some(NonNull::from(&mut ctx)), Some(NonNull::from(settings))) };
        if res.0 != 0 {
            return Err(failed("did not start", res));
        }
        Ok(Av1 { ctx, picture: None })
    }

    /// Decodes `data`, one temporal unit — an image item, or a sequence's
    /// sample — to its picture; of a layered item's pictures, the last, the
    /// whole image.
    pub(crate) fn decode(&mut self, data: &[u8]) -> Result<Picture<'_>, AssetError> {
        let ctx = self
            .ctx
            .ok_or_else(|| AssetError::Decode("the AV1 decoder is closed".to_owned()))?;
        if let Some(old) = &mut self.picture {
            // SAFETY: `old` is a picture rav1d gave this decoder and nothing
            // borrows it: `Picture` borrows `self` mutably.
            unsafe { dav1d_picture_unref(Some(NonNull::from(old))) };
            self.picture = None;
        }
        let mut latest: Option<Dav1dPicture> = None;
        let keep = |pic: Dav1dPicture, latest: &mut Option<Dav1dPicture>| {
            if let Some(mut prev) = latest.replace(pic) {
                // SAFETY: `prev` is a picture rav1d gave, superseded by a
                // later layer, and referenced nowhere else.
                unsafe { dav1d_picture_unref(Some(NonNull::from(&mut prev))) };
            }
        };
        let mut buffer = Dav1dData::default();
        // SAFETY: `buffer` is a live local; rav1d allocates `data.len()`
        // bytes for it and returns them, or null when it cannot.
        let at = unsafe { dav1d_data_create(Some(NonNull::from(&mut buffer)), data.len()) };
        let Some(at) = NonNull::new(at) else {
            return Err(AssetError::Decode("out of memory for AV1 data".to_owned()));
        };
        // SAFETY: `at` is the start of `data.len()` bytes rav1d just
        // allocated, which `data` cannot overlap.
        unsafe { ptr::copy_nonoverlapping(data.as_ptr(), at.as_ptr(), data.len()) };
        let sent = loop {
            // SAFETY: `ctx` is this decoder's open context and `buffer` the
            // data it was created with; rav1d takes what it consumes.
            let res = unsafe { dav1d_send_data(Some(ctx), Some(NonNull::from(&mut buffer))) };
            if res.0 == 0 {
                break Ok(());
            }
            if res.0 != EAGAIN {
                break Err(failed("refused the data", res));
            }
            // It holds a picture to hand out before taking more.
            let mut pic = Dav1dPicture::default();
            // SAFETY: as above; `pic` is a live local rav1d fills.
            let got = unsafe { dav1d_get_picture(Some(ctx), Some(NonNull::from(&mut pic))) };
            if got.0 != 0 {
                break Err(failed("is stuck", res));
            }
            keep(pic, &mut latest);
        };
        // SAFETY: what rav1d did not take of `buffer` is still this
        // decoder's to release.
        unsafe { dav1d_data_unref(Some(NonNull::from(&mut buffer))) };
        if let Err(e) = sent {
            if let Some(mut pic) = latest {
                // SAFETY: a picture rav1d gave, referenced nowhere else.
                unsafe { dav1d_picture_unref(Some(NonNull::from(&mut pic))) };
            }
            return Err(e);
        }
        loop {
            let mut pic = Dav1dPicture::default();
            // SAFETY: as above.
            let got = unsafe { dav1d_get_picture(Some(ctx), Some(NonNull::from(&mut pic))) };
            if got.0 != 0 {
                break;
            }
            keep(pic, &mut latest);
        }
        let picture = latest.ok_or_else(|| AssetError::Decode("no AV1 picture".to_owned()))?;
        Ok(Picture {
            pic: self.picture.insert(picture),
            _decoder: PhantomData,
        })
    }
}

impl Drop for Av1 {
    fn drop(&mut self) {
        if let Some(pic) = &mut self.picture {
            // SAFETY: the last picture rav1d gave, borrowed by nothing now.
            unsafe { dav1d_picture_unref(Some(NonNull::from(pic))) };
        }
        // SAFETY: `ctx` was opened by `dav1d_open` and is closed only here;
        // rav1d accepts an already-null context.
        unsafe { dav1d_close(Some(NonNull::from(&mut self.ctx))) };
    }
}

/// How a picture's chroma is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Layout {
    /// No chroma.
    Mono,
    /// Chroma at half the width and half the height.
    Half,
    /// Chroma at half the width.
    HalfAcross,
    /// Chroma at full size.
    Full,
}

/// One plane's samples, rows `stride` samples apart.
#[derive(Clone, Copy)]
pub(crate) enum Samples<'a> {
    Eight(&'a [u8]),
    Wide(&'a [u16]),
}

/// A plane of a picture.
#[derive(Clone, Copy)]
pub(crate) struct Plane<'a> {
    pub(crate) samples: Samples<'a>,
    pub(crate) stride: usize,
    pub(crate) w: usize,
    pub(crate) h: usize,
}

impl Plane<'_> {
    /// The sample at `x, y`, clamped into the plane.
    #[inline(always)]
    pub(crate) fn at(&self, x: usize, y: usize) -> u16 {
        let i = y.min(self.h - 1) * self.stride + x.min(self.w - 1);
        match self.samples {
            Samples::Eight(s) => u16::from(s[i]),
            Samples::Wide(s) => s[i],
        }
    }
}

/// The colour signal of a picture's sequence header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Signal {
    pub(crate) primaries: u16,
    pub(crate) transfer: u16,
    pub(crate) matrix: u16,
    pub(crate) full: bool,
}

/// A decoded picture, valid until its decoder decodes again.
pub(crate) struct Picture<'a> {
    pic: &'a Dav1dPicture,
    _decoder: PhantomData<&'a mut Av1>,
}

impl Picture<'_> {
    pub(crate) fn size(&self) -> (usize, usize) {
        (self.pic.p.w.max(0) as usize, self.pic.p.h.max(0) as usize)
    }

    /// Bits a sample.
    pub(crate) fn depth(&self) -> u8 {
        self.pic.p.bpc.clamp(8, 16) as u8
    }

    pub(crate) fn layout(&self) -> Result<Layout, AssetError> {
        Ok(match self.pic.p.layout {
            DAV1D_PIXEL_LAYOUT_I400 => Layout::Mono,
            DAV1D_PIXEL_LAYOUT_I420 => Layout::Half,
            DAV1D_PIXEL_LAYOUT_I422 => Layout::HalfAcross,
            DAV1D_PIXEL_LAYOUT_I444 => Layout::Full,
            _ => {
                return Err(AssetError::Decode(
                    "an unknown AV1 chroma layout".to_owned(),
                ));
            }
        })
    }

    /// The signal its sequence header declares.
    pub(crate) fn signal(&self) -> Option<Signal> {
        // SAFETY: a picture's sequence header lives as long as the picture,
        // which `self` borrows.
        let seq = unsafe { self.pic.seq_hdr?.as_ref() };
        Some(Signal {
            primaries: seq.pri as u16,
            transfer: seq.trc as u16,
            matrix: seq.mtrx as u16,
            full: seq.color_range != 0,
        })
    }

    /// Plane `n`: luma, then the two chroma planes.
    pub(crate) fn plane(&self, n: usize) -> Result<Plane<'_>, AssetError> {
        let bad = || AssetError::Decode("an AV1 plane is not its size".to_owned());
        let wide = self.pic.p.bpc > 8;
        let (sx, sy) = match (n, self.layout()?) {
            (0, _) | (_, Layout::Full) => (0, 0),
            (_, Layout::Half) => (1, 1),
            (_, Layout::HalfAcross) => (1, 0),
            (_, Layout::Mono) => return Err(bad()),
        };
        let (w, h) = self.size();
        let (w, h) = ((w + sx) >> sx, (h + sy) >> sy);
        let data = self.pic.data[n].ok_or_else(bad)?;
        let unit = if wide { 2 } else { 1 };
        // Luma has its stride, the chroma planes share theirs.
        let bytes = self.pic.stride[usize::from(n != 0)];
        if bytes <= 0 || !(bytes as usize).is_multiple_of(unit) || w == 0 || h == 0 {
            return Err(bad());
        }
        let stride = bytes as usize / unit;
        if stride < w {
            return Err(bad());
        }
        let len = stride * (h - 1) + w;
        // SAFETY: rav1d allocates each plane as `h` rows of `stride`
        // samples, aligned for its sample type, alive while the picture is;
        // `len` stays within them.
        let samples = unsafe {
            if wide {
                Samples::Wide(std::slice::from_raw_parts(data.as_ptr().cast::<u16>(), len))
            } else {
                Samples::Eight(std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), len))
            }
        };
        Ok(Plane {
            samples,
            stride,
            w,
            h,
        })
    }
}
