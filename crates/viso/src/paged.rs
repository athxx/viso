//! Paged capture: a view laid out at the width of a page and cut into pages,
//! each page's primitives handed over with its text as glyph runs.
//!
//! [`capture`] takes a [`Paged`] — the view, built like any other with a
//! [`BuildCx`], and the paper it goes on — and once the view's text and
//! images are ready, a frame or a few later, runs the closure it is given on
//! the [`Capture`]: every page's [`PagePart`]s (its body, header and footer)
//! in paint order, a [`PageSource`] that hands out each glyph's outline and
//! draws what has no vector form into a raster, all while the window's
//! device is at hand. The window it is built in lends it its fonts, theme
//! and images; nothing of it shows there. A document writer — a PDF one, a
//! print spooler's — works from the capture alone.
//!
//! ```no_run
//! use viso::paged::{self, Margins, PageSize, Paged};
//! use viso::ui::{BuildCx, FlexStyle};
//!
//! fn invoice(cx: &mut BuildCx<'_>) {
//!     cx.flex(FlexStyle::default(), |_cx| {
//!         // The lines of the invoice.
//!     });
//! }
//!
//! let reply = paged::capture(
//!     Paged::new(invoice)
//!         .size(PageSize::A4)
//!         .margins(Margins::all(48.0))
//!         .footer(|cx, page| {
//!             let _ = (cx, page.number, page.count);
//!         }),
//!     |capture| capture.pages().len(),
//! );
//! // `reply` resolves to what the closure returned.
//! # drop(reply);
//! ```
//!
//! The page holds the view at [`Paged::scale`] points per pixel, 0.75 by
//! default: a 16 px body text prints at 12 pt, as a browser prints a page.
//! Pages break between lines of text and around images and other leaves,
//! never through one shorter than a page; [`page_break`] forces a break and
//! [`keep_together`] keeps a box on one page. A header and a footer, built
//! once for each page with its number, sit in the top and bottom margins.
//!
//! Text comes as [`TextRun`]s of glyphs, each mapped back to the text it
//! shows; the source gives a glyph's outline, or a coverage mask where the
//! face's outline is out of reach. Rasters draw through the window's
//! renderer; on the web, and on a GLES device, there are none.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use viso_gpu::{GpuBackend, TextureDesc, TextureFormat};
use viso_math::{Affine2, linear_to_srgb, srgb_to_linear};
use viso_platform::LoopWaker;
use viso_render::{PathCmd, Point, Primitive, Rect, Rgba, TransformSlot};
use viso_services::{Reply, ServiceError, reply};
use viso_text::FontFaceId;
use viso_ui::BoxStyle;
use viso_ui::{
    Align, Axis, BuildCx, Content, FlexStyle, Handle, Justify, LeafStyle, Length, NodeId, Sizing,
    TransformTable,
};

use crate::driver::{GpuState, WindowState};
use crate::text_content::{ParagraphSlot, TextShaper};

/// A paper size, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    pub const A3: PageSize = PageSize::new(841.89, 1190.55);
    pub const A4: PageSize = PageSize::new(595.28, 841.89);
    pub const A5: PageSize = PageSize::new(419.53, 595.28);
    pub const LETTER: PageSize = PageSize::new(612.0, 792.0);
    pub const LEGAL: PageSize = PageSize::new(612.0, 1008.0);
    pub const TABLOID: PageSize = PageSize::new(792.0, 1224.0);

    pub const fn new(width: f32, height: f32) -> PageSize {
        PageSize { width, height }
    }

    /// The size turned on its side: its longer edge across.
    pub const fn landscape(self) -> PageSize {
        if self.width >= self.height {
            self
        } else {
            PageSize::new(self.height, self.width)
        }
    }
}

/// The margins around a page's content, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Margins {
    pub const fn new(top: f32, right: f32, bottom: f32, left: f32) -> Margins {
        Margins {
            top,
            right,
            bottom,
            left,
        }
    }

    pub const fn all(points: f32) -> Margins {
        Margins::new(points, points, points, points)
    }
}

impl Default for Margins {
    /// Half an inch all round.
    fn default() -> Margins {
        Margins::all(36.0)
    }
}

/// Which page a header or footer is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageNumber {
    /// From 1.
    pub number: usize,
    pub count: usize,
}

type View = Box<dyn FnOnce(&mut BuildCx<'_>)>;
type Band = Rc<dyn Fn(&mut BuildCx<'_>, PageNumber)>;

/// A view to capture and the paper it goes on.
pub struct Paged {
    view: View,
    size: PageSize,
    margins: Margins,
    scale: f32,
    header: Option<Band>,
    footer: Option<Band>,
}

impl Paged {
    /// The view `view` builds, on A4 with half-inch margins.
    pub fn new(view: impl FnOnce(&mut BuildCx<'_>) + 'static) -> Paged {
        Paged {
            view: Box::new(view),
            size: PageSize::A4,
            margins: Margins::default(),
            scale: 0.75,
            header: None,
            footer: None,
        }
    }

    pub fn size(mut self, size: PageSize) -> Paged {
        self.size = size;
        self
    }

    pub fn margins(mut self, margins: Margins) -> Paged {
        self.margins = margins;
        self
    }

    /// How many points a pixel of the view takes on paper; 0.75 by default.
    pub fn scale(mut self, points_per_pixel: f32) -> Paged {
        if points_per_pixel.is_finite() && points_per_pixel > 0.0 {
            self.scale = points_per_pixel;
        }
        self
    }

    /// Builds a header in each page's top margin.
    pub fn header(mut self, header: impl Fn(&mut BuildCx<'_>, PageNumber) + 'static) -> Paged {
        self.header = Some(Rc::new(header));
        self
    }

    /// Builds a footer in each page's bottom margin.
    pub fn footer(mut self, footer: impl Fn(&mut BuildCx<'_>, PageNumber) + 'static) -> Paged {
        self.footer = Some(Rc::new(footer));
        self
    }
}

/// One stretch of content a page shows: a slice of content pixels mapped
/// into a box of the paper. A page shows its body this way, and its header
/// and footer.
#[derive(Debug, Clone, Copy)]
pub struct PagePart<'a> {
    /// The box on the paper the content shows in, in points from the top
    /// left; nothing of it draws outside.
    pub area: Rect,
    /// Points per content pixel.
    pub scale: f32,
    /// The content pixel at the area's top left.
    pub origin: [f32; 2],
    /// What it draws, in paint order.
    pub draws: &'a [Draw<'a>],
    /// Where each transform root of `draws` is placed.
    pub places: &'a [Affine2],
}

/// One captured page: its paper and what shows on it, in order.
#[derive(Debug, Clone)]
pub struct Page<'a> {
    /// The paper, in points.
    pub size: [f32; 2],
    pub parts: Vec<PagePart<'a>>,
}

/// One thing a page draws, in paint order.
#[derive(Debug, Clone, Copy)]
pub enum Draw<'a> {
    Primitive(&'a Primitive),
    Text(&'a TextRun),
}

/// A run of glyphs of one face, size and color.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// The face, as the [`PageSource`] names it.
    pub face: u32,
    /// The size in content pixels per em.
    pub size: f32,
    pub color: Rgba,
    pub glyphs: Vec<TextGlyph>,
}

/// A glyph placed on its baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct TextGlyph {
    pub glyph: u16,
    /// Its pen position on the baseline, in content pixels.
    pub origin: [f32; 2],
    /// The text it shows; empty for a glyph that continues another's.
    pub text: Box<str>,
}

/// A glyph's design, in font units, y up from the baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    pub units_per_em: f32,
    /// The face's reach above the baseline and below it (negative).
    pub ascender: f32,
    pub descender: f32,
    pub advance: f32,
    pub shape: GlyphShape,
}

/// What a glyph draws.
#[derive(Debug, Clone, PartialEq)]
pub enum GlyphShape {
    /// Its outline, filled nonzero.
    Outline(Vec<PathCmd>),
    /// A coverage mask, for a face whose outline is out of reach: `width ×
    /// height` bytes, rows top first, its bottom left at `origin` and each
    /// pixel `unit` font units wide.
    Mask {
        width: u32,
        height: u32,
        coverage: Vec<u8>,
        origin: [f32; 2],
        unit: f32,
    },
    /// Nothing: a space.
    Empty,
}

/// A raster of primitives: `width × height` straight-alpha sRGB RGBA8
/// pixels, rows top first.
#[derive(Debug, Clone, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Where a page's glyphs and rasters come from.
pub trait PageSource {
    /// Glyph `glyph` of face `face`; `None` where the face has no such
    /// glyph.
    fn glyph(&mut self, face: u32, glyph: u16) -> Option<Glyph>;

    /// `draws` drawn over transparency into a raster of the box `rect` of
    /// content pixels at `scale` raster pixels per content pixel; `None`
    /// where they cannot be.
    fn raster(&mut self, draws: &[Primitive], rect: Rect, scale: f32) -> Option<Raster>;
}

/// The captured pages, and the source of their glyphs and rasters while the
/// window that laid them out is at hand.
pub struct Capture<'a> {
    pages: &'a [Page<'a>],
    source: &'a mut dyn PageSource,
}

impl<'a> Capture<'a> {
    /// Every page, in order.
    pub fn pages(&self) -> &'a [Page<'a>] {
        self.pages
    }
}

impl PageSource for Capture<'_> {
    fn glyph(&mut self, face: u32, glyph: u16) -> Option<Glyph> {
        self.source.glyph(face, glyph)
    }

    fn raster(&mut self, draws: &[Primitive], rect: Rect, scale: f32) -> Option<Raster> {
        self.source.raster(draws, rect, scale)
    }
}

/// Captures `paged`, answering with what `take` makes of the [`Capture`]
/// once its view is laid out with its text and images: a frame or a few
/// after the call returns. `take` runs in the frame, the window's device at
/// hand. Callable from anywhere on the app's thread. It is built in the
/// app's first window; with none open it fails with
/// [`ServiceError::Unsupported`].
pub fn capture<T: 'static>(
    paged: Paged,
    take: impl FnOnce(&mut Capture<'_>) -> T + 'static,
) -> Reply<T> {
    let (done, answer) = reply();
    let take: Take = Box::new(move |captured| {
        done.complete(captured.map(take));
    });
    ASKED.with(|asked| {
        let mut asked = asked.borrow_mut();
        asked.jobs.push(Box::new(Running::new(paged, take)));
        if let Some(waker) = &asked.waker {
            waker.wake();
        }
    });
    answer
}

/// Breaks the page before what `cx` declares next, in a [`Paged`] view.
pub fn page_break(cx: &mut BuildCx<'_>) {
    let mark = cx.leaf(LeafStyle {
        size: Sizing::fixed(0.0, 0.0),
        style: BoxStyle::NONE,
    });
    MARKS.with(|marks| marks.borrow_mut().breaks.push(mark.id()));
}

/// Keeps `node`'s box on one page where it fits on one, in a [`Paged`]
/// view.
pub fn keep_together(node: Handle) {
    MARKS.with(|marks| marks.borrow_mut().keeps.push(node.id()));
}

/// What runs on a capture once it is made, or on why it is not.
type Take = Box<dyn FnOnce(Result<&mut Capture<'_>, ServiceError>)>;

/// A capture being made, as the loop moves it on. Only [`capture`] makes
/// one, so an app that captures nothing links none of the capture's code.
pub(crate) trait Job {
    /// Moves it on by a frame in `ws`; `true` once it is finished.
    fn step(&mut self, ws: &mut WindowState) -> bool;
    /// Gives it up for `error`.
    fn fail(self: Box<Self>, error: ServiceError);
}

/// The captures asked for that the loop has not taken.
#[derive(Default)]
struct Asked {
    jobs: Vec<Box<dyn Job>>,
    waker: Option<LoopWaker>,
}

/// The breaks and kept boxes a view's build marks.
#[derive(Default)]
struct Marks {
    breaks: Vec<NodeId>,
    keeps: Vec<NodeId>,
}

thread_local! {
    static ASKED: RefCell<Asked> = RefCell::new(Asked::default());
    static MARKS: RefCell<Marks> = RefCell::new(Marks::default());
}

/// Tells calls on this thread how to kick the loop.
pub(crate) fn launch(waker: LoopWaker) {
    ASKED.with(|asked| asked.borrow_mut().waker = Some(waker));
}

/// The captures asked for since the last call.
pub(crate) fn take_asked() -> Vec<Box<dyn Job>> {
    ASKED.with(|asked| std::mem::take(&mut asked.borrow_mut().jobs))
}

/// How long a capture waits for its text and images before it is made with
/// what it has.
const PATIENCE: Duration = Duration::from_secs(10);

/// Content laid out for a capture: a detached subtree of the window's store.
struct Laid {
    root: NodeId,
    /// The box it lays out in, in pixels; `None` tall for content that
    /// takes its own height.
    width: f32,
    height: Option<f32>,
    marks: Marks,
}

/// A capture being made.
struct Running {
    paged: Paged,
    take: Option<Take>,
    body: Option<Laid>,
    /// Where each page starts and the last ends, in body pixels, once cut.
    cuts: Vec<f32>,
    /// Each page's header and footer, once built.
    bands: Vec<(Option<Laid>, Option<Laid>)>,
    frames: u32,
    since: Instant,
}

impl Job for Running {
    fn step(&mut self, ws: &mut WindowState) -> bool {
        Running::step(self, ws)
    }

    fn fail(self: Box<Self>, error: ServiceError) {
        Running::fail(*self, error);
    }
}

impl Running {
    fn new(paged: Paged, take: Take) -> Running {
        Running {
            paged,
            take: Some(take),
            body: None,
            cuts: Vec::new(),
            bands: Vec::new(),
            frames: 0,
            since: Instant::now(),
        }
    }

    /// Fails it, when no window is there to build it in.
    fn fail(mut self, error: ServiceError) {
        if let Some(take) = self.take.take() {
            take(Err(error));
        }
    }

    /// The content width in pixels.
    fn width(&self) -> f32 {
        let p = &self.paged;
        ((p.size.width - p.margins.left - p.margins.right) / p.scale).max(1.0)
    }

    /// The page height a body page shows, in pixels.
    fn page_height(&self) -> f32 {
        let p = &self.paged;
        ((p.size.height - p.margins.top - p.margins.bottom) / p.scale).max(1.0)
    }

    /// Moves it on by one frame in `ws`: builds, lays out, waits for its
    /// text and images, cuts its pages and hands them over. Returns whether
    /// it is done.
    fn step(&mut self, ws: &mut WindowState) -> bool {
        if self.body.is_none() {
            let view = std::mem::replace(&mut self.paged.view, Box::new(|_| {}));
            let width = self.width();
            self.body = Some(build(ws, width, None, view));
        }
        for laid in self.laid() {
            lay(ws, laid);
        }
        let settled = self.frames >= 2
            && !ws.text_work_pending()
            && !ws.store.has_text_requests()
            && !ws.store.has_text_reflows()
            && !ws.store.assets().is_loading();
        self.frames += 1;
        if !settled && self.since.elapsed() < PATIENCE {
            return false;
        }
        if self.cuts.is_empty() {
            self.cuts = self.cut(ws);
            if self.paged.header.is_some() || self.paged.footer.is_some() {
                self.build_bands(ws);
                self.frames = 0;
                return false;
            }
        }
        if let Some(take) = self.take.take() {
            self.hand_over(ws, take);
        }
        self.free(ws);
        true
    }

    fn laid(&self) -> impl Iterator<Item = &Laid> {
        self.body.iter().chain(
            self.bands
                .iter()
                .flat_map(|(header, footer)| header.iter().chain(footer.iter())),
        )
    }

    fn build_bands(&mut self, ws: &mut WindowState) {
        let count = self.cuts.len().saturating_sub(1);
        let width = self.width();
        let scale = self.paged.scale;
        let (top, bottom) = (
            self.paged.margins.top / scale,
            self.paged.margins.bottom / scale,
        );
        for number in 1..=count {
            let page = PageNumber { number, count };
            let band = |ws: &mut WindowState, band: &Option<Band>, height: f32| {
                band.clone().map(|band| {
                    build(
                        ws,
                        width,
                        Some(height),
                        Box::new(move |cx: &mut BuildCx<'_>| band(cx, page)),
                    )
                })
            };
            let header = band(ws, &self.paged.header, top);
            let footer = band(ws, &self.paged.footer, bottom);
            self.bands.push((header, footer));
        }
    }

    /// Where the pages start, and the last ends: as far down as a page
    /// reaches, short of a line or a leaf it would cut, at a break.
    fn cut(&self, ws: &WindowState) -> Vec<f32> {
        let Some(body) = &self.body else {
            return vec![0.0];
        };
        let store = &ws.store;
        let page = self.page_height();
        let origin = store.world(body.root);
        // The root takes the box it lays out in; its content ends where its
        // last child does.
        let height = store
            .arena()
            .children(body.root)
            .map(|child| {
                let world = store.world(child);
                world.y + world.h - origin.y
            })
            .fold(1.0f32, f32::max);
        let mut keeps = Vec::new();
        let mut stack = vec![body.root];
        while let Some(node) = stack.pop() {
            let world = store.world(node);
            let y = world.y - origin.y;
            let rows = ws
                .text
                .as_ref()
                .and_then(|text| text.line_rows(ParagraphSlot::from(node)))
                .filter(|_| matches!(store.content_payload(node), Some(Content::Text { .. })));
            let mut leaf = true;
            for child in store.arena().children(node) {
                leaf = false;
                stack.push(child);
            }
            match rows {
                Some(rows) => keeps.extend(rows.windows(2).map(|r| (y + r[0], y + r[1]))),
                None if leaf && world.h < page => keeps.push((y, y + world.h)),
                None => {}
            }
        }
        for &node in &body.marks.keeps {
            if store.arena().is_live(node) {
                let world = store.world(node);
                if world.h < page {
                    keeps.push((world.y - origin.y, world.y - origin.y + world.h));
                }
            }
        }
        let mut breaks: Vec<f32> = body
            .marks
            .breaks
            .iter()
            .filter(|&&node| store.arena().is_live(node))
            .map(|&node| store.world(node).y - origin.y)
            .collect();
        breaks.sort_by(f32::total_cmp);
        paginate(height, page, &keeps, &breaks)
    }

    /// Hands the pages over to `take`, with the window as their source.
    fn hand_over(&mut self, ws: &mut WindowState, take: Take) {
        ws.resolve_style();
        let painted_body = self.body.as_ref().map(|laid| Painted::of(ws, laid));
        let bands: Vec<(Option<Painted>, Option<Painted>)> = self
            .bands
            .iter()
            .map(|(h, f)| {
                (
                    h.as_ref().map(|laid| Painted::of(ws, laid)),
                    f.as_ref().map(|laid| Painted::of(ws, laid)),
                )
            })
            .collect();
        let body_draws = painted_body.as_ref().map(Painted::draws);
        let band_draws: Vec<[Option<Vec<Draw<'_>>>; 2]> = bands
            .iter()
            .map(|(h, f)| {
                [
                    h.as_ref().map(Painted::draws),
                    f.as_ref().map(Painted::draws),
                ]
            })
            .collect();
        let p = &self.paged;
        let size = [p.size.width, p.size.height];
        let width = p.size.width - p.margins.left - p.margins.right;
        let count = self.cuts.windows(2).count().max(1);
        let mut pages = Vec::with_capacity(count);
        for page in 0..count {
            let (top, bottom) = match self.cuts.get(page..page + 2) {
                Some(&[top, bottom]) => (top, bottom),
                _ => (0.0, 0.0),
            };
            let mut parts = Vec::with_capacity(3);
            if let (Some(body), Some(draws)) = (&painted_body, &body_draws) {
                parts.push(PagePart {
                    area: Rect::new(
                        p.margins.left,
                        p.margins.top,
                        width,
                        (bottom - top) * p.scale,
                    ),
                    scale: p.scale,
                    origin: [body.origin[0], body.origin[1] + top],
                    draws,
                    places: &body.places,
                });
            }
            let (header, footer) = match bands.get(page) {
                Some((h, f)) => (h.as_ref(), f.as_ref()),
                None => (None, None),
            };
            let (header_draws, footer_draws) = match band_draws.get(page) {
                Some([h, f]) => (h.as_ref(), f.as_ref()),
                None => (None, None),
            };
            for (band, draws, y, h) in [
                (header, header_draws, 0.0, p.margins.top),
                (
                    footer,
                    footer_draws,
                    p.size.height - p.margins.bottom,
                    p.margins.bottom,
                ),
            ] {
                if let (Some(band), Some(draws)) = (band, draws) {
                    parts.push(PagePart {
                        area: Rect::new(p.margins.left, y, width, h),
                        scale: p.scale,
                        origin: band.origin,
                        draws,
                        places: &band.places,
                    });
                }
            }
            pages.push(Page { size, parts });
        }
        let mut source = PrintSource {
            text: ws.text.as_ref(),
            gpu: ws.gpu.as_mut(),
            drew: false,
        };
        take(Ok(&mut Capture {
            pages: &pages,
            source: &mut source,
        }));
        if source.drew {
            restore(ws);
        }
    }

    /// Frees its content from the window's store.
    pub(crate) fn free(&mut self, ws: &mut WindowState) {
        let mut scratch = Vec::new();
        let body = self.body.take();
        let bands = std::mem::take(&mut self.bands);
        let roots = body.into_iter().chain(
            bands
                .into_iter()
                .flat_map(|(header, footer)| header.into_iter().chain(footer)),
        );
        for laid in roots {
            ws.store.free_tree(laid.root, &mut ws.effects, &mut scratch);
        }
    }
}

/// Builds `view` as a detached subtree of `ws`'s store in a column `width`
/// pixels wide and `height` tall, or as tall as it is.
fn build(ws: &mut WindowState, width: f32, height: Option<f32>, view: View) -> Laid {
    MARKS.with(|marks| *marks.borrow_mut() = Marks::default());
    let mut cx = BuildCx::with_reactive(
        &mut ws.store,
        &mut ws.states,
        &mut ws.bindings,
        &mut ws.virtual_lists,
        &mut ws.text_edits,
        &mut ws.projectors,
    );
    let root = cx
        .flex(
            FlexStyle {
                axis: Axis::Column,
                align: Align::Stretch,
                justify: if height.is_some() {
                    Justify::Center
                } else {
                    Justify::Start
                },
                size: Sizing {
                    width: Length::Fixed(width),
                    height: height.map_or(Length::Fit, Length::Fixed),
                },
                ..FlexStyle::default()
            },
            view,
        )
        .id();
    let marks = MARKS.with(|marks| std::mem::take(&mut *marks.borrow_mut()));
    Laid {
        root,
        width,
        height,
        marks,
    }
}

/// Lays `laid` out in its box.
fn lay(ws: &mut WindowState, laid: &Laid) {
    let height = laid.height.unwrap_or(1.0e7);
    ws.store.layout(
        laid.root,
        Rect::new(0.0, 0.0, laid.width, height),
        &mut ws.scratch,
    );
}

/// Cuts content `height` tall into pages `page` tall: each as long as fits,
/// moved up off any of `keeps` it would cut unless that loses most of the
/// page, and short at each of `breaks`.
fn paginate(height: f32, page: f32, keeps: &[(f32, f32)], breaks: &[f32]) -> Vec<f32> {
    let mut cuts = vec![0.0];
    let mut top = 0.0f32;
    while top < height - 0.5 {
        let mut cut = top + page;
        if let Some(&forced) = breaks.iter().find(|&&b| b > top + 0.5 && b < cut) {
            cut = forced;
        } else if cut < height {
            loop {
                let straddled = keeps
                    .iter()
                    .filter(|&&(s, e)| s > top + 0.5 && s < cut - 0.01 && e > cut + 0.01)
                    .map(|&(s, _)| s)
                    .reduce(f32::min);
                match straddled {
                    Some(start) if start > top + page * 0.25 => cut = start,
                    _ => break,
                }
            }
        }
        cut = cut.min(height);
        cuts.push(cut);
        top = cut;
    }
    if cuts.len() == 1 {
        cuts.push(height);
    }
    cuts
}

/// A laid-out subtree's primitives with its text as runs, ready to draw.
struct Painted {
    prims: Vec<Primitive>,
    runs: Vec<TextRun>,
    /// The order to draw them in: a primitive's index, or a run's.
    order: Vec<Item>,
    places: Vec<Affine2>,
    /// The subtree's top left, in its pixels.
    origin: [f32; 2],
}

#[derive(Clone, Copy)]
enum Item {
    Prim(usize),
    Run(usize),
}

impl Painted {
    fn of(ws: &WindowState, laid: &Laid) -> Painted {
        let store = &ws.store;
        let mut prims = Vec::new();
        let mut owned = Vec::new();
        let mut table = TransformTable::default();
        viso_ui::paint_owned(store, laid.root, &mut prims, &mut table, &mut owned);
        table.update(store);
        let places = table.places().to_vec();
        let mut runs = Vec::new();
        // For each primitive index: replaced by the runs listed, or dropped.
        let mut replaced: Vec<Option<Vec<usize>>> = vec![None; prims.len()];
        for (node, range) in owned {
            let Some(Content::Text { atlas, glyphs, .. }) = store.content_payload(node) else {
                continue;
            };
            let Some(text) = &ws.text else { continue };
            let Some((source, placed)) = text.print_glyphs(ParagraphSlot::from(node)) else {
                continue;
            };
            let color = ws
                .text_sources
                .get(&node)
                .map_or(viso_render::Rgba::new(0.0, 0.0, 0.0, 1.0), |r| r.color);
            let mut at = None;
            let mut shift = None;
            for i in range.clone() {
                if let Primitive::GlyphRun(run) = &prims[i]
                    && run.atlas == *atlas
                {
                    at.get_or_insert(i);
                    if shift.is_none()
                        && let (Some(drawn), Some(own)) = (run.glyphs.first(), glyphs.first())
                    {
                        shift = Some([drawn.rect.x - own.rect.x, drawn.rect.y - own.rect.y]);
                    }
                    replaced[i] = Some(Vec::new());
                }
            }
            let shift = shift.unwrap_or_else(|| {
                let world = store.world(node);
                [world.x, world.y]
            });
            let first = runs.len();
            to_runs(&source, &placed, color, store.palette(), shift, &mut runs);
            let made: Vec<usize> = (first..runs.len()).collect();
            match at {
                Some(i) => replaced[i] = Some(made),
                None => {
                    // Nothing of it drew yet: its runs go after its own
                    // drawables.
                    let end = range.end.saturating_sub(1).max(range.start);
                    if end < prims.len() && range.end > range.start {
                        let mut kept = replaced[end].take().unwrap_or_default();
                        if kept.is_empty() {
                            // Keep the primitive itself, then the runs.
                            kept.push(usize::MAX);
                        }
                        kept.extend(made);
                        replaced[end] = Some(kept);
                    }
                }
            }
        }
        let mut order = Vec::with_capacity(prims.len());
        for (i, entry) in replaced.into_iter().enumerate() {
            match entry {
                None => order.push(Item::Prim(i)),
                Some(list) => {
                    for item in list {
                        if item == usize::MAX {
                            order.push(Item::Prim(i));
                        } else {
                            order.push(Item::Run(item));
                        }
                    }
                }
            }
        }
        let world = store.world(laid.root);
        Painted {
            prims,
            runs,
            order,
            places,
            origin: [world.x, world.y],
        }
    }

    fn draws(&self) -> Vec<Draw<'_>> {
        self.order
            .iter()
            .map(|&item| match item {
                Item::Prim(i) => Draw::Primitive(&self.prims[i]),
                Item::Run(i) => Draw::Text(&self.runs[i]),
            })
            .collect()
    }
}

/// `placed`'s glyphs of `source` as runs of one face, size and color, a
/// span's theme color as `palette` paints it, shifted by `shift`; emoji
/// left to the images that draw them.
fn to_runs(
    source: &str,
    placed: &[crate::text_content::PrintGlyph],
    color: viso_render::Rgba,
    palette: &viso_ui::Palette,
    shift: [f32; 2],
    out: &mut Vec<TextRun>,
) {
    let mut run: Option<TextRun> = None;
    for g in placed.iter().filter(|g| !g.emoji) {
        let color = g.color.map_or(color, |c| palette.resolve(c));
        let face = g.face.0;
        let same = run
            .as_ref()
            .is_some_and(|r| r.face == face && r.size == g.size && r.color == color);
        if !same && let Some(done) = run.take() {
            out.push(done);
        }
        let current = run.get_or_insert_with(|| TextRun {
            face,
            size: g.size,
            color,
            glyphs: Vec::new(),
        });
        current.glyphs.push(TextGlyph {
            glyph: g.glyph.0,
            origin: [g.origin[0] + shift[0], g.origin[1] + shift[1]],
            text: source.get(g.text.clone()).unwrap_or("").into(),
        });
    }
    out.extend(run);
}

/// Where a capture of a window gets its glyphs and rasters.
struct PrintSource<'a> {
    text: Option<&'a TextShaper>,
    gpu: Option<&'a mut GpuState>,
    /// Whether a raster drew through the window's renderer, which then
    /// takes the window's frame again.
    drew: bool,
}

/// The size a glyph without an outline the parser reads is rasterized at.
const MASK_PPEM: u16 = 512;

impl PageSource for PrintSource<'_> {
    fn glyph(&mut self, face: u32, glyph: u16) -> Option<Glyph> {
        let text = self.text?;
        let id = FontFaceId(face);
        let data = text.face_data(id)?;
        let mut parsed = ttf_parser::Face::parse(&data.bytes, data.index).ok()?;
        data.variations.apply(&mut parsed);
        let units_per_em = f32::from(parsed.units_per_em());
        let gid = ttf_parser::GlyphId(glyph);
        let advance = f32::from(parsed.glyph_hor_advance(gid).unwrap_or(0));
        let (ascender, descender) = (f32::from(parsed.ascender()), f32::from(parsed.descender()));
        let mut outline = Outline(Vec::new());
        // A CFF2 face reassembled from the platform's tables draws its
        // default master from the bytes, not the instance shown.
        let read =
            parsed.tables().cff2.is_none() && parsed.outline_glyph(gid, &mut outline).is_some();
        let shape = if read {
            GlyphShape::Outline(outline.0)
        } else if let Some(mask) = text.platform_mask(id, viso_text::GlyphId(glyph), MASK_PPEM) {
            let unit = units_per_em / f32::from(MASK_PPEM);
            GlyphShape::Mask {
                width: mask.width,
                height: mask.height,
                origin: [mask.left * unit, (mask.top - mask.height as f32) * unit],
                unit,
                coverage: mask.coverage,
            }
        } else {
            GlyphShape::Empty
        };
        Some(Glyph {
            units_per_em,
            ascender,
            descender,
            advance,
            shape,
        })
    }

    fn raster(&mut self, draws: &[Primitive], rect: Rect, scale: f32) -> Option<Raster> {
        let gpu = self.gpu.as_deref_mut()?;
        let (width, height) = (
            ((rect.w * scale).ceil() as u32).max(1),
            ((rect.h * scale).ceil() as u32).max(1),
        );
        let target = gpu.backend.create_texture(&TextureDesc {
            width,
            height,
            format: TextureFormat::Bgra8UnormSrgb,
            render_target: true,
            label: "page raster",
        });
        let logical = [width as f32 / scale, height as f32 / scale];
        gpu.renderer
            .set_surface_size([width as f32, height as f32], scale);
        let mut wrapped = Vec::with_capacity(draws.len() + 2);
        wrapped.push(Primitive::Transform(TransformSlot(0)));
        wrapped.extend_from_slice(draws);
        wrapped.push(Primitive::TransformEnd);
        gpu.renderer.upload_with_transforms(
            &mut gpu.backend,
            &wrapped,
            &[Affine2::translate(-rect.x, -rect.y)],
        );
        gpu.renderer
            .render_to_texture(&mut gpu.backend, target, [0.0; 4], logical);
        self.drew = true;
        let read = read_back(&mut gpu.backend, target);
        gpu.backend.destroy_texture(target);
        let bgra = read?;
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for px in bgra
            .as_chunks::<4>()
            .0
            .iter()
            .take((width * height) as usize)
        {
            pixels.extend_from_slice(&straight([px[2], px[1], px[0], px[3]]));
        }
        Some(Raster {
            width,
            height,
            pixels,
        })
    }
}

/// A premultiplied sRGB pixel's straight-alpha form, unpremultiplied in
/// linear light as it was blended.
fn straight([r, g, b, a]: [u8; 4]) -> [u8; 4] {
    match a {
        0 => [0; 4],
        255 => [r, g, b, 255],
        a => {
            let alpha = f32::from(a) / 255.0;
            let channel = |v: u8| {
                let linear = srgb_to_linear(f32::from(v) / 255.0) / alpha;
                (linear_to_srgb(linear.min(1.0)) * 255.0 + 0.5) as u8
            };
            [channel(r), channel(g), channel(b), a]
        }
    }
}

/// The texels of render target `id`, where this device reads them back at
/// once.
fn read_back(backend: &mut viso_gpu::Backend, id: viso_gpu::TextureId) -> Option<Vec<u8>> {
    let _ = &backend;
    viso_gpu::__per_backend!(
        msl: Some(backend.read_texture(id)),
        hlsl: Some(backend.read_texture(id)),
        spirv: Some(backend.read_texture(id)),
        glsl: { let _ = id; None },
        wgsl: { let _ = id; None },
        none: Some(backend.read_texture(id)),
    )
}

/// Gives the window's renderer its own frame back after rasters drew
/// through it.
fn restore(ws: &mut WindowState) {
    let dpi = ws.dpi.max(1.0);
    let Some(gpu) = ws.gpu.as_mut() else { return };
    let (w, h) = gpu.size;
    gpu.renderer.set_surface_size([w as f32, h as f32], dpi);
    gpu.renderer.upload_with_transforms(
        &mut gpu.backend,
        &ws.primitives,
        ws.store.transforms().places(),
    );
}

/// A glyph's outline as path commands, in font units.
struct Outline(Vec<PathCmd>);

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(PathCmd::MoveTo(Point { x, y }));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(PathCmd::LineTo(Point { x, y }));
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0
            .push(PathCmd::QuadTo(Point { x: x1, y: y1 }, Point { x, y }));
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.push(PathCmd::CubicTo(
            Point { x: x1, y: y1 },
            Point { x: x2, y: y2 },
            Point { x, y },
        ));
    }

    fn close(&mut self) {
        self.0.push(PathCmd::Close);
    }
}

#[cfg(test)]
mod tests {
    use super::paginate;

    #[test]
    fn pages_break_between_lines_and_at_breaks() {
        // Lines 30 tall; a page of 100 would cut the fourth line.
        let lines: Vec<(f32, f32)> = (0..10)
            .map(|i| (i as f32 * 30.0, i as f32 * 30.0 + 30.0))
            .collect();
        assert_eq!(
            paginate(300.0, 100.0, &lines, &[]),
            [0.0, 90.0, 180.0, 270.0, 300.0]
        );
        assert_eq!(
            paginate(300.0, 100.0, &lines, &[60.0]),
            [0.0, 60.0, 150.0, 240.0, 300.0]
        );
        // A box taller than most of a page is cut where the page ends.
        assert_eq!(
            paginate(150.0, 100.0, &[(10.0, 140.0)], &[]),
            [0.0, 100.0, 150.0]
        );
        assert_eq!(paginate(0.0, 100.0, &[], &[]), [0.0, 0.0]);
    }
}
