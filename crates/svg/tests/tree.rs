use viso_svg::tree::*;

fn tree(s: &str) -> Tree {
    Tree::parse(s).unwrap_or_else(|e| panic!("{e}"))
}

/// All paths with their absolute transforms.
fn paths(t: &Tree) -> Vec<(Path, Transform)> {
    let mut out = Vec::new();
    t.root.for_each_path(&Transform::IDENTITY, &mut |p, ts| {
        out.push((p.clone(), *ts))
    });
    out
}

fn abs_points(p: &Path, ts: &Transform) -> Vec<(f32, f32)> {
    p.data
        .segments()
        .iter()
        .filter_map(|s| match *s {
            Segment::MoveTo(q) | Segment::LineTo(q) => Some(ts.apply(q)),
            _ => None,
        })
        .map(|q| ((q.x * 100.0).round() / 100.0, (q.y * 100.0).round() / 100.0))
        .collect()
}

fn fill_color(p: &Path) -> Color {
    match &p.fill.as_ref().expect("fill").paint {
        Paint::Color(c) => *c,
        other => panic!("not a color: {other:?}"),
    }
}

const NS: &str = r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""#;

#[test]
fn size_and_view_box() {
    let t = tree(&format!(r##"<svg {NS} width="200" viewBox="0 0 10 20"/>"##));
    assert_eq!(t.size, (200.0, 400.0));
    let t = tree(&format!(r##"<svg {NS}/>"##));
    assert_eq!(t.size, (100.0, 100.0));
    assert!(matches!(Tree::parse("<html/>"), Err(ParseError::NotSvg)));
    assert!(matches!(Tree::parse("<svg"), Err(ParseError::Xml(_))));
    assert!(matches!(
        Tree::parse(&format!(r##"<svg {NS} width="0"/>"##)),
        Err(ParseError::InvalidSize)
    ));
}

#[test]
fn view_box_maps_content() {
    let t = tree(&format!(
        r##"<svg {NS} width="100" height="100" viewBox="0 0 10 10"><rect x="1" y="2" width="3" height="4"/></svg>"##
    ));
    let ps = paths(&t);
    assert_eq!(ps.len(), 1);
    assert_eq!(abs_points(&ps[0].0, &ps[0].1)[0], (10.0, 20.0));
}

#[test]
fn css_cascade_and_inheritance() {
    let t = tree(&format!(
        r##"<svg {NS}><style>.a {{ fill: blue }} #r {{ fill: lime !important }}</style>
        <g fill="red"><rect width="1" height="1"/><rect class="a" width="1" height="1"/>
        <rect id="r" class="a" style="fill: yellow" width="1" height="1"/></g></svg>"##
    ));
    let ps = paths(&t);
    assert_eq!(fill_color(&ps[0].0), Color::new(255, 0, 0, 255));
    assert_eq!(fill_color(&ps[1].0), Color::new(0, 0, 255, 255));
    assert_eq!(fill_color(&ps[2].0), Color::new(0, 255, 0, 255));
}

#[test]
fn use_expands_and_guards_cycles() {
    let t = tree(&format!(
        r##"<svg {NS}><defs><rect id="r" width="2" height="2"/></defs>
        <use href="#r" x="5" y="6"/><use xlink:href="#r"/>
        <g id="loop"><use href="#loop"/></g></svg>"##
    ));
    let ps = paths(&t);
    assert_eq!(ps.len(), 2);
    assert_eq!(abs_points(&ps[0].0, &ps[0].1)[0], (5.0, 6.0));
}

#[test]
fn percentages_resolve_against_viewport() {
    let t = tree(&format!(
        r##"<svg {NS} width="200" height="100"><rect width="50%" height="50%"/></svg>"##
    ));
    let ps = paths(&t);
    let b = ps[0].0.data.bounds().unwrap();
    assert_eq!((b.w, b.h), (100.0, 50.0));
}

#[test]
fn gradients_resolve_to_user_space() {
    let t = tree(&format!(
        r##"<svg {NS}><linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient>
        <linearGradient id="h" href="#g" x2="0" y2="1"/>
        <linearGradient id="one"><stop stop-color="lime"/></linearGradient>
        <rect x="10" width="20" height="40" fill="url(#h)"/><rect width="1" height="1" fill="url(#one)"/>
        <rect width="1" height="1" fill="url(#missing) green"/></svg>"##
    ));
    let ps = paths(&t);
    let Some(Fill {
        paint: Paint::LinearGradient(g),
        ..
    }) = &ps[0].0.fill
    else {
        panic!()
    };
    assert_eq!(g.stops.len(), 2);
    // Unit square → bbox.
    assert_eq!(
        g.transform.apply(Point::new(g.x2, g.y2)),
        Point::new(10.0, 40.0)
    );
    assert_eq!(fill_color(&ps[1].0), Color::new(0, 255, 0, 255));
    assert_eq!(fill_color(&ps[2].0), Color::new(0, 128, 0, 255));
}

#[test]
fn clip_and_mask_references() {
    let t = tree(&format!(
        r##"<svg {NS}><clipPath id="c"><circle r="5"/></clipPath><mask id="m"><rect width="1" height="1" fill="white"/></mask>
        <rect width="10" height="10" clip-path="url(#c)" mask="url(#m)"/>
        <rect width="10" height="10" clip-path="url(#nope)"/>
        <clipPath id="obb" clipPathUnits="objectBoundingBox"><rect width=".5" height=".5"/></clipPath>
        <g clip-path="url(#obb)"/></svg>"##
    ));
    let Node::Group(g) = &t.root.children[0] else {
        panic!("{:?}", t.root.children[0])
    };
    assert!(g.clip_path.is_some() && g.mask.is_some());
    // Missing ref renders unclipped; empty obb group is dropped.
    assert_eq!(t.root.children.len(), 2);
    assert!(matches!(t.root.children[1], Node::Path(_)));
}

#[test]
fn stroke_properties() {
    let t = tree(&format!(
        r##"<svg {NS}><path d="M0 0 L10 0" stroke="red" stroke-width="2" stroke-dasharray="1 2 3" stroke-linecap="round"/>
        <path d="M0 0 L10 0" stroke="red" stroke-dasharray="0 0"/></svg>"##
    ));
    let ps = paths(&t);
    let s = ps[0].0.stroke.as_ref().unwrap();
    assert_eq!(s.width, 2.0);
    assert_eq!(s.cap, LineCap::Round);
    assert_eq!(
        s.dasharray.as_deref(),
        Some(&[1.0, 2.0, 3.0, 1.0, 2.0, 3.0][..])
    );
    assert_eq!(ps[1].0.stroke.as_ref().unwrap().dasharray, None);
}

#[test]
fn markers_are_instanced_per_vertex() {
    let t = tree(&format!(
        r##"<svg {NS}><marker id="m" markerWidth="4" markerHeight="4" orient="auto"><rect width="1" height="1"/></marker>
        <polyline points="0 0 10 0 10 10" stroke="black" marker-start="url(#m)" marker-mid="url(#m)" marker-end="url(#m)" fill="none"/></svg>"##
    ));
    // Stroke path + 3 marker rects.
    assert_eq!(paths(&t).len(), 4);
}

#[test]
fn opacity_and_display() {
    let t = tree(&format!(
        r##"<svg {NS}><rect width="1" height="1" opacity=".5"/><rect width="1" height="1" display="none"/>
        <rect width="1" height="1" opacity="0"/><rect width="1" height="1" visibility="hidden"/></svg>"##
    ));
    let ps = paths(&t);
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0].0.fill.as_ref().unwrap().opacity, 0.5);
}

#[test]
fn switch_picks_first_match() {
    let t = tree(&format!(
        r##"<svg {NS}><switch><rect systemLanguage="fr" width="1" height="1" fill="red"/>
        <rect width="1" height="1" fill="lime"/><rect width="1" height="1"/></switch></svg>"##
    ));
    let ps = paths(&t);
    assert_eq!(ps.len(), 1);
    assert_eq!(fill_color(&ps[0].0), Color::new(0, 255, 0, 255));
}

#[test]
fn filters_parse() {
    let t = tree(&format!(
        r##"<svg {NS}><filter id="f"><feGaussianBlur stdDeviation="2" result="b"/><feOffset in="b" dx="3"/>
        <feMerge><feMergeNode in="b"/><feMergeNode in="SourceGraphic"/></feMerge></filter>
        <rect width="10" height="10" filter="url(#f)"/><rect width="10" height="10" filter="blur(1px) grayscale()"/></svg>"##
    ));
    let Node::Group(g) = &t.root.children[0] else {
        panic!()
    };
    let f = &g.filters[0];
    assert_eq!(f.region, Rect::new(-1.0, -1.0, 12.0, 12.0));
    assert_eq!(f.primitives.len(), 3);
    assert!(matches!(
        f.primitives[1].kind,
        FilterKind::Offset {
            input: FilterInput::Reference(_),
            dx: 3.0,
            ..
        }
    ));
    let Node::Group(g) = &t.root.children[1] else {
        panic!()
    };
    assert_eq!(g.filters.len(), 2);
}

#[test]
fn data_url_images() {
    let inner = "data:image/svg+xml;utf8,%3Csvg xmlns='http://www.w3.org/2000/svg' width='4' height='4'%3E%3Crect width='4' height='4'/%3E%3C/svg%3E";
    let t = tree(&format!(
        r##"<svg {NS}><image href="{inner}" width="8" height="8"/></svg>"##
    ));
    let ps = paths(&t);
    assert_eq!(ps.len(), 1);
    assert_eq!(abs_points(&ps[0].0, &ps[0].1)[2], (8.0, 8.0));
}

#[test]
fn arcs_and_shapes_become_paths() {
    let t = tree(&format!(
        r##"<svg {NS}><circle cx="5" cy="5" r="5"/><ellipse rx="2" ry="1"/><rect width="4" height="4" rx="1"/>
        <path d="M0 0 A5 5 0 0 1 10 0"/><polygon points="0 0 1 0 1 1"/><line x2="1" stroke="black"/></svg>"##
    ));
    assert_eq!(paths(&t).len(), 6);
    let b = paths(&t)[0].0.data.bounds().unwrap();
    assert!((b.w - 10.0).abs() < 1e-3);
}
