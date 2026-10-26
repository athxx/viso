use viso_render::{
    AnalyticEllipse, AnalyticRRect, Border, Corners, ExtendMode, Gradient, GradientKind,
    GradientStop, InterpolationSpace, LayerClip, Path, Quad, Stroke,
};

use super::*;

/// Square glyphs a fifth of an em inset, and rasters of solid red.
struct Fake {
    rasters: usize,
}

impl Source for Fake {
    fn glyph(&mut self, _: u32, glyph: u16) -> Option<Glyph> {
        if glyph == 0 {
            return Some(Glyph {
                units_per_em: 1000.0,
                ascender: 800.0,
                descender: -200.0,
                advance: 250.0,
                shape: GlyphShape::Empty,
            });
        }
        let p = |x, y| Point { x, y };
        Some(Glyph {
            units_per_em: 1000.0,
            ascender: 800.0,
            descender: -200.0,
            advance: 600.0,
            shape: GlyphShape::Outline(vec![
                PathCmd::MoveTo(p(100.0, 0.0)),
                PathCmd::LineTo(p(500.0, 0.0)),
                PathCmd::LineTo(p(500.0, 700.0)),
                PathCmd::LineTo(p(100.0, 700.0)),
                PathCmd::Close,
            ]),
        })
    }

    fn raster(&mut self, _: &[Primitive], rect: Rect, scale: f32) -> Option<Raster> {
        self.rasters += 1;
        let (width, height) = (
            (rect.w * scale).ceil() as u32,
            (rect.h * scale).ceil() as u32,
        );
        Some(Raster {
            width,
            height,
            pixels: [255, 0, 0, 128].repeat((width * height) as usize),
        })
    }
}

const RED: Rgba = Rgba::new(1.0, 0.0, 0.0, 1.0);
const BLUE: Rgba = Rgba::new(0.0, 0.0, 1.0, 1.0);

fn sample() -> Vec<Primitive> {
    let stops = vec![
        GradientStop {
            offset: 0.0,
            color: RED,
        },
        GradientStop {
            offset: 1.0,
            color: Rgba::new(0.0, 0.0, 1.0, 0.2),
        },
    ];
    let gradient = |kind, p1| {
        Primitive::Gradient(Gradient {
            rect: Rect::new(20.0, 140.0, 120.0, 80.0),
            kind,
            extend: ExtendMode::Repeat,
            p0: Point { x: 80.0, y: 180.0 },
            p1,
            stops: stops.clone(),
            interp: InterpolationSpace::Srgb,
        })
    };
    vec![
        Primitive::Quad(Quad {
            rect: Rect::new(10.0, 10.0, 200.0, 100.0),
            color: Rgba::new(0.9, 0.9, 0.9, 1.0),
            radius: 12.0,
            border: Border {
                width: 2.0,
                color: BLUE,
            },
        }),
        Primitive::AnalyticRRect(AnalyticRRect {
            rect: Rect::new(230.0, 10.0, 100.0, 100.0),
            color: Rgba::new(0.0, 0.5, 0.0, 0.5),
            radius: Corners {
                left_top: 30.0,
                right_top: 0.0,
                right_bottom: 10.0,
                left_bottom: 0.0,
            },
            border: Border::NONE,
        }),
        Primitive::AnalyticEllipse(AnalyticEllipse {
            rect: Rect::new(350.0, 10.0, 80.0, 60.0),
            color: RED,
            border: Border {
                width: 4.0,
                color: BLUE,
            },
        }),
        gradient(GradientKind::Linear, Point { x: 140.0, y: 180.0 }),
        Primitive::Layer(LayerClip {
            clip: Rect::new(60.0, 150.0, 140.0, 100.0),
            opacity: 0.5,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }),
        Primitive::ClipCorners(Corners::uniform(20.0)),
        gradient(GradientKind::Radial, Point { x: 60.0, y: 0.0 }),
        Primitive::LayerEnd,
        Primitive::Gradient(Gradient {
            rect: Rect::new(320.0, 140.0, 120.0, 80.0),
            kind: GradientKind::Sweep,
            extend: ExtendMode::Clamp,
            p0: Point { x: 380.0, y: 180.0 },
            p1: Point { x: 0.0, y: 0.0 },
            stops: vec![
                GradientStop {
                    offset: 0.0,
                    color: RED,
                },
                GradientStop {
                    offset: 0.5,
                    color: Rgba::new(0.0, 1.0, 0.0, 1.0),
                },
                GradientStop {
                    offset: 1.0,
                    color: BLUE,
                },
            ],
            interp: InterpolationSpace::Srgb,
        }),
        Primitive::Path(Box::new(Path {
            cmds: vec![
                PathCmd::MoveTo(Point { x: 20.0, y: 260.0 }),
                PathCmd::QuadTo(Point { x: 80.0, y: 230.0 }, Point { x: 140.0, y: 260.0 }),
                PathCmd::LineTo(Point { x: 80.0, y: 320.0 }),
                PathCmd::Close,
            ],
            fill: Some(BLUE),
            stroke: Some(Stroke::new(3.0, RED)),
            shadow: None,
        })),
        Primitive::Image(viso_render::ImageDraw::new(
            Rect::new(200.0, 260.0, 60.0, 40.0),
            viso_render::TextureId::new(7),
        )),
    ]
}

fn text() -> TextRun {
    TextRun {
        face: 3,
        size: 20.0,
        color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        glyphs: "Hi there"
            .chars()
            .enumerate()
            .map(|(i, ch)| TextGlyph {
                glyph: if ch == ' ' { 0 } else { ch as u16 },
                origin: [20.0 + i as f32 * 12.0, 380.0],
                text: ch.to_string().into(),
            })
            .collect(),
    }
}

fn part<'a>(draws: &'a [Draw<'a>], origin_y: f32) -> Part<'a> {
    Part {
        area: Rect::new(36.0, 36.0, 523.0, 300.0),
        scale: 0.75,
        origin: [0.0, origin_y],
        draws,
        places: &[],
    }
}

fn document() -> (Vec<u8>, usize) {
    let prims = sample();
    let run = text();
    let mut draws: Vec<Draw<'_>> = prims.iter().map(Draw::Primitive).collect();
    draws.push(Draw::Text(&run));
    let mut source = Fake { rasters: 0 };
    let mut doc = Document::new(Info {
        title: Some("Viso – sample".into()),
        lang: Some("en".into()),
        ..Info::default()
    });
    let size = [595.0, 842.0];
    doc.page(size, &[part(&draws, 0.0)], &mut source);
    doc.page(size, &[part(&draws, 400.0)], &mut source);
    doc.page(size, &[part(&draws, 0.0)], &mut source);
    (doc.finish(), source.rasters)
}

#[test]
fn a_document_is_a_well_formed_file() {
    let (bytes, _) = document();
    if let Some(path) = std::env::var_os("VISO_PDF_OUT") {
        std::fs::write(path, &bytes).unwrap();
    }
    assert!(bytes.starts_with(b"%PDF-1.7"));
    assert!(bytes.ends_with(b"%%EOF\n"));
    // Every cross-reference entry points at its object's header.
    let find = |needle: &[u8]| {
        bytes
            .windows(needle.len())
            .rposition(|w| w == needle)
            .unwrap()
    };
    let xref = find(b"\nxref\n") + 1;
    let table = String::from_utf8(bytes[xref..].to_vec()).unwrap();
    let mut lines = table.lines().skip(1);
    let count: usize = lines.next().unwrap()[2..].parse().unwrap();
    lines.next();
    for n in 1..count {
        let offset: usize = lines.next().unwrap()[..10].parse().unwrap();
        assert!(
            bytes[offset..].starts_with(format!("{n} 0 obj").as_bytes()),
            "object {n} is where the table says"
        );
    }
    let startxref: usize = table[table.rfind("startxref\n").unwrap() + 10..]
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(startxref, xref);
}

#[test]
fn glyphs_and_rasters_are_written_once() {
    let (bytes, rasters) = document();
    let s = String::from_utf8_lossy(&bytes);
    assert_eq!(
        rasters, 1,
        "the image rasters once for both pages showing it"
    );
    assert_eq!(
        s.matches("/Subtype/Type3").count(),
        1,
        "one font for the face"
    );
    assert_eq!(s.matches("/Type/Page/").count(), 3);
}
