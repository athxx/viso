//! PDF documents of views: a view laid out at the width of a page and cut
//! into pages, drawn as vectors with its text as text.
//!
//! [`export`] takes a [`Pdf`] — the view, built like any other with a
//! [`BuildCx`], and the paper it goes on — and answers with the file's bytes
//! once the view's text and images are ready, a frame or a few later. The
//! window it is built in lends it its fonts, theme and images; nothing of it
//! shows there.
//!
//! ```no_run
//! use viso::pdf::{self, Margins, PageSize, Pdf};
//! use viso::ui::{BuildCx, FlexStyle};
//!
//! fn invoice(cx: &mut BuildCx<'_>) {
//!     cx.flex(FlexStyle::default(), |_cx| {
//!         // The lines of the invoice.
//!     });
//! }
//!
//! let reply = pdf::export(
//!     Pdf::new(invoice)
//!         .size(PageSize::A4)
//!         .margins(Margins::all(48.0))
//!         .title("Invoice 42")
//!         .footer(|cx, page| {
//!             let _ = (cx, page.number, page.count);
//!         }),
//! );
//! // `reply` resolves to the bytes: write them to a file, share them, or
//! // hand them to the system's print dialog.
//! # drop(reply);
//! ```
//!
//! The page holds the view at [`Pdf::scale`] points per pixel, 0.75 by
//! default: a 16 px body text prints at 12 pt, as a browser prints a page.
//! Pages break between lines of text and around images and other leaves,
//! never through one shorter than a page; [`page_break`] forces a break and
//! [`keep_together`] keeps a box on one page. A header and a footer, built
//! once for each page with its number, sit in the top and bottom margins.
//!
//! What draws as vectors: boxes and their borders, paths, gradients, meshes,
//! clips, translucent and blended groups, color effects; text as glyph
//! outlines a reader selects, searches and copies. Images, shadows, shader
//! brushes and 3D scenes draw as rasters at [`Pdf::raster_dpi`], 300 by
//! default; a frosted surface shows its border alone, having no backdrop to
//! blur. On the web, and on a GLES device, the rasters are left out.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use viso_gpu::{GpuBackend, TextureDesc, TextureFormat};
use viso_math::{Affine2, linear_to_srgb, srgb_to_linear};
use viso_pdf::{Document, Draw, Glyph, GlyphShape, Info, Part, Raster, Source, TextGlyph, TextRun};
use viso_platform::LoopWaker;
use viso_render::{PathCmd, Point, Primitive, Rect, TransformSlot};
use viso_services::{Completer, Reply, ServiceError, reply};
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

/// A document to write: a view and the paper it goes on.
pub struct Pdf {
    view: View,
    size: PageSize,
    margins: Margins,
    scale: f32,
    raster_dpi: f32,
    info: Info,
    header: Option<Band>,
    footer: Option<Band>,
}

impl Pdf {
    /// A document of the view `view` builds, on A4 with half-inch margins.
    pub fn new(view: impl FnOnce(&mut BuildCx<'_>) + 'static) -> Pdf {
        Pdf {
            view: Box::new(view),
            size: PageSize::A4,
            margins: Margins::default(),
            scale: 0.75,
            raster_dpi: viso_pdf::RASTER_DPI,
            info: Info::default(),
            header: None,
            footer: None,
        }
    }

    pub fn size(mut self, size: PageSize) -> Pdf {
        self.size = size;
        self
    }

    pub fn margins(mut self, margins: Margins) -> Pdf {
        self.margins = margins;
        self
    }

    /// How many points a pixel of the view takes on paper; 0.75 by default.
    pub fn scale(mut self, points_per_pixel: f32) -> Pdf {
        if points_per_pixel.is_finite() && points_per_pixel > 0.0 {
            self.scale = points_per_pixel;
        }
        self
    }

    /// The resolution what has no vector form draws at, in pixels per inch.
    pub fn raster_dpi(mut self, dpi: f32) -> Pdf {
        self.raster_dpi = dpi;
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Pdf {
        self.info.title = Some(title.into());
        self
    }

    pub fn author(mut self, author: impl Into<String>) -> Pdf {
        self.info.author = Some(author.into());
        self
    }

    pub fn subject(mut self, subject: impl Into<String>) -> Pdf {
        self.info.subject = Some(subject.into());
        self
    }

    pub fn keywords(mut self, keywords: impl Into<String>) -> Pdf {
        self.info.keywords = Some(keywords.into());
        self
    }

    /// The language the document is written in, a BCP 47 tag.
    pub fn lang(mut self, lang: impl Into<String>) -> Pdf {
        self.info.lang = Some(lang.into());
        self
    }

    /// Builds a header in each page's top margin.
    pub fn header(mut self, header: impl Fn(&mut BuildCx<'_>, PageNumber) + 'static) -> Pdf {
        self.header = Some(Rc::new(header));
        self
    }

    /// Builds a footer in each page's bottom margin.
    pub fn footer(mut self, footer: impl Fn(&mut BuildCx<'_>, PageNumber) + 'static) -> Pdf {
        self.footer = Some(Rc::new(footer));
        self
    }
}

/// Writes `pdf`, answering with the file's bytes once its view is laid out
/// with its text and images: a frame or a few after the call returns.
/// Callable from anywhere on the app's thread. It is built in the app's
/// first window; with none open it fails with
/// [`ServiceError::Unsupported`].
pub fn export(pdf: Pdf) -> Reply<Vec<u8>> {
    let (done, answer) = reply();
    ASKED.with(|asked| {
        let mut asked = asked.borrow_mut();
        asked.jobs.push((pdf, done));
        if let Some(waker) = &asked.waker {
            waker.wake();
        }
    });
    answer
}

/// Breaks the page before what `cx` declares next, in a [`Pdf`]'s view.
pub fn page_break(cx: &mut BuildCx<'_>) {
    let mark = cx.leaf(LeafStyle {
        size: Sizing::fixed(0.0, 0.0),
        style: BoxStyle::NONE,
    });
    MARKS.with(|marks| marks.borrow_mut().breaks.push(mark.id()));
}

/// Keeps `node`'s box on one page where it fits on one, in a [`Pdf`]'s view.
pub fn keep_together(node: Handle) {
    MARKS.with(|marks| marks.borrow_mut().keeps.push(node.id()));
}

/// The documents asked for that the loop has not taken.
#[derive(Default)]
struct Asked {
    jobs: Vec<(Pdf, Completer<Vec<u8>>)>,
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

/// The documents asked for since the last call.
pub(crate) fn take_asked() -> Vec<(Pdf, Completer<Vec<u8>>)> {
    ASKED.with(|asked| std::mem::take(&mut asked.borrow_mut().jobs))
}

/// How long a document waits for its text and images before it is written
/// with what it has.
const PATIENCE: Duration = Duration::from_secs(10);

/// Content laid out for a document: a detached subtree of the window's store.
struct Laid {
    root: NodeId,
    /// The box it lays out in, in pixels; `None` tall for content that
    /// takes its own height.
    width: f32,
    height: Option<f32>,
    marks: Marks,
}

/// A document being written.
pub(crate) struct Running {
    pdf: Pdf,
    done: Option<Completer<Vec<u8>>>,
    body: Option<Laid>,
    /// Where each page starts and the last ends, in body pixels, once cut.
    cuts: Vec<f32>,
    /// Each page's header and footer, once built.
    bands: Vec<(Option<Laid>, Option<Laid>)>,
    frames: u32,
    since: Instant,
}

impl Running {
    pub(crate) fn new(pdf: Pdf, done: Completer<Vec<u8>>) -> Running {
        Running {
            pdf,
            done: Some(done),
            body: None,
            cuts: Vec::new(),
            bands: Vec::new(),
            frames: 0,
            since: Instant::now(),
        }
    }

    /// Fails it, when no window is there to build it in.
    pub(crate) fn fail(mut self, error: ServiceError) {
        if let Some(done) = self.done.take() {
            done.complete(Err(error));
        }
    }

    /// The content width in pixels.
    fn width(&self) -> f32 {
        let p = &self.pdf;
        ((p.size.width - p.margins.left - p.margins.right) / p.scale).max(1.0)
    }

    /// The page height a body page shows, in pixels.
    fn page_height(&self) -> f32 {
        let p = &self.pdf;
        ((p.size.height - p.margins.top - p.margins.bottom) / p.scale).max(1.0)
    }

    /// Moves it on by one frame in `ws`: builds, lays out, waits for its
    /// text and images, cuts its pages and writes it. Returns whether it is
    /// done.
    pub(crate) fn step(&mut self, ws: &mut WindowState) -> bool {
        if self.body.is_none() {
            let view = std::mem::replace(&mut self.pdf.view, Box::new(|_| {}));
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
            if self.pdf.header.is_some() || self.pdf.footer.is_some() {
                self.build_bands(ws);
                self.frames = 0;
                return false;
            }
        }
        let bytes = self.write(ws);
        self.free(ws);
        if let Some(done) = self.done.take() {
            done.complete(Ok(bytes));
        }
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
        let scale = self.pdf.scale;
        let (top, bottom) = (
            self.pdf.margins.top / scale,
            self.pdf.margins.bottom / scale,
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
            let header = band(ws, &self.pdf.header, top);
            let footer = band(ws, &self.pdf.footer, bottom);
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

    /// The document's bytes.
    fn write(&mut self, ws: &mut WindowState) -> Vec<u8> {
        ws.resolve_style();
        let mut doc = Document::new(self.pdf.info.clone());
        doc.set_raster_dpi(self.pdf.raster_dpi);
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
        let p = &self.pdf;
        let size = [p.size.width, p.size.height];
        let width = p.size.width - p.margins.left - p.margins.right;
        let mut source = PrintSource {
            text: ws.text.as_ref(),
            gpu: ws.gpu.as_mut(),
            drew: false,
        };
        let pages = self.cuts.windows(2).count().max(1);
        for page in 0..pages {
            let (top, bottom) = match self.cuts.get(page..page + 2) {
                Some(&[top, bottom]) => (top, bottom),
                _ => (0.0, 0.0),
            };
            let mut parts = Vec::with_capacity(3);
            let body_draws = painted_body.as_ref().map(Painted::draws);
            if let (Some(body), Some(draws)) = (&painted_body, &body_draws) {
                parts.push(Part {
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
            let header_draws = header.map(Painted::draws);
            let footer_draws = footer.map(Painted::draws);
            for (band, draws, y, h) in [
                (header, &header_draws, 0.0, p.margins.top),
                (
                    footer,
                    &footer_draws,
                    p.size.height - p.margins.bottom,
                    p.margins.bottom,
                ),
            ] {
                if let (Some(band), Some(draws)) = (band, draws) {
                    parts.push(Part {
                        area: Rect::new(p.margins.left, y, width, h),
                        scale: p.scale,
                        origin: band.origin,
                        draws,
                        places: &band.places,
                    });
                }
            }
            doc.page(size, &parts, &mut source);
        }
        if source.drew {
            restore(ws);
        }
        doc.finish()
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
            to_runs(&source, &placed, color, shift, &mut runs);
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

/// `placed`'s glyphs of `source` as runs of one face, size and color,
/// shifted by `shift`; emoji left to the images that draw them.
fn to_runs(
    source: &str,
    placed: &[crate::text_content::PrintGlyph],
    color: viso_render::Rgba,
    shift: [f32; 2],
    out: &mut Vec<TextRun>,
) {
    let mut run: Option<TextRun> = None;
    for g in placed.iter().filter(|g| !g.emoji) {
        let color = g.color.unwrap_or(color);
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
            glyph: g.glyph,
            origin: [g.origin[0] + shift[0], g.origin[1] + shift[1]],
            text: source.get(g.text.clone()).unwrap_or("").into(),
        });
    }
    out.extend(run);
}

/// Where a document of a window gets its glyphs and rasters.
struct PrintSource<'a> {
    text: Option<&'a TextShaper>,
    gpu: Option<&'a mut GpuState>,
    /// Whether a raster drew through the window's renderer, which then
    /// takes the window's frame again.
    drew: bool,
}

/// The size a glyph without an outline the parser reads is rasterized at.
const MASK_PPEM: u16 = 512;

impl Source for PrintSource<'_> {
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
        } else if let Some(mask) = text.platform_mask(id, glyph, MASK_PPEM) {
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
            label: "pdf raster",
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
