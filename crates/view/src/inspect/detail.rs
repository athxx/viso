//! The runtime's half of a node's detail: the node as the store holds it,
//! with the bindings into it and its span of the window's paint stream.

use viso_dev_protocol::inspect::{
    ChainLink, NodeBinding, NodeDetail, PaintSpan, PropertyGroup, RouteStop, spell_value,
};
use viso_ui::{
    Align, AlignItems, Axis, BindingTable, Content, Interaction, Justify, LayoutInput, Length,
    NodeId, NodeStore, PaintRanges, Primitive, Rgba, StateStore,
};

/// The detail of node `id` of a window whose paint stream is `primitives`,
/// spanned by `ranges`, `draws` the batch of each draw the renderer made of
/// it by stream index, in stream order; `None` for a node not live.
pub fn node_detail(
    store: &NodeStore,
    bindings: &BindingTable,
    states: &StateStore,
    ranges: &PaintRanges,
    primitives: &[Primitive],
    draws: &[(u32, u32)],
    id: NodeId,
) -> Option<NodeDetail> {
    if !store.arena().is_live(id) {
        return None;
    }
    let mut ancestry = vec![id];
    while let Some(parent) = store.parent(*ancestry.last().expect("a node")) {
        ancestry.push(parent);
    }
    ancestry.reverse();
    let chain = ancestry
        .iter()
        .filter_map(|&node| {
            let translate = store.translate(node);
            let scroll = store.scroll(node);
            let moved = translate.x != 0.0 || translate.y != 0.0;
            let scrolled = scroll.x != 0.0 || scroll.y != 0.0;
            (moved || scrolled).then_some(ChainLink {
                index: node.index(),
                generation: node.generation(),
                translate: [translate.x, translate.y],
                scroll: [scroll.x, scroll.y],
            })
        })
        .collect();
    let route = ancestry
        .iter()
        .filter_map(|&node| {
            let stop = RouteStop {
                index: node.index(),
                generation: node.generation(),
                pointer: store.has_handler(node),
                key: store.has_key_handler(node),
                focusable: store.focusable(node),
            };
            (stop.pointer || stop.key || stop.focusable).then_some(stop)
        })
        .collect();
    let mut edges = Vec::new();
    bindings.edges_into(store, id, &mut edges);
    let bindings = edges
        .into_iter()
        .filter_map(|(cell, class, dynamic)| {
            let cell = states.live_id(cell)?;
            Some(NodeBinding {
                cell: (cell.index(), cell.generation()),
                key: states.key_of(cell).map(|key| (key.hi, key.lo)),
                class: class.bits(),
                dynamic,
                value: states.get(cell).map(spell_value).unwrap_or_default(),
            })
        })
        .collect();
    let states = Interaction::ALL
        .iter()
        .filter(|&&state| store.in_state(id, state))
        .fold(0u8, |bits, state| bits | 1 << state.tag());
    let paint = ranges.get(id).map_or_else(PaintSpan::default, |range| {
        let end = (range.start + range.len).min(primitives.len());
        let start = range.start.min(end);
        let own = own_spans(store, ranges, id, start, end);
        PaintSpan {
            start: range.start as u32,
            len: range.len as u32,
            own: kinds(&primitives[own[0].0..own[0].1]),
            batches: batches(draws, &own),
        }
    });
    Some(NodeDetail {
        index: id.index(),
        generation: id.generation(),
        groups: groups(store, id),
        world: store.world(id),
        bounds: store.bounds(id),
        chain,
        dirty: store.dirty(id).bits(),
        states,
        route,
        bindings,
        paint,
    })
}

/// The stream spans node `id` paints itself, `start..end` being its span:
/// the one before its first painted child, then the one after its last,
/// where it closes what it opened. Either may be empty.
fn own_spans(
    store: &NodeStore,
    ranges: &PaintRanges,
    id: NodeId,
    start: usize,
    end: usize,
) -> [(usize, usize); 2] {
    let mut head = end;
    let mut tail = end;
    for at in store.arena().children(id) {
        if let Some(range) = ranges.get(at) {
            head = head.min(range.start.clamp(start, end));
            tail = (range.start + range.len).clamp(start, end);
        }
    }
    [(start, head), (tail.max(head), end)]
}

/// The kinds of `span`'s primitives, consecutive ones of a kind counted
/// together.
fn kinds(span: &[Primitive]) -> Vec<(String, u32)> {
    let mut kinds: Vec<(String, u32)> = Vec::new();
    for primitive in span {
        let kind = primitive.label();
        match kinds.last_mut() {
            Some((last, n)) if last == kind => *n += 1,
            _ => kinds.push((kind.to_owned(), 1)),
        }
    }
    kinds
}

/// The batches the draws of `spans` land in, consecutive draws in one batch
/// counted together.
fn batches(draws: &[(u32, u32)], spans: &[(usize, usize)]) -> Vec<(u32, u32)> {
    let mut batches: Vec<(u32, u32)> = Vec::new();
    for &(lo, hi) in spans {
        let first = draws.partition_point(|&(source, _)| (source as usize) < lo);
        for &(_, batch) in draws[first..]
            .iter()
            .take_while(|&&(source, _)| (source as usize) < hi)
        {
            match batches.last_mut() {
                Some((last, n)) if *last == batch => *n += 1,
                _ => batches.push((batch, 1)),
            }
        }
    }
    batches
}

/// The node's layout, style and content properties as it resolved them.
fn groups(store: &NodeStore, id: NodeId) -> Vec<PropertyGroup> {
    let mut layout = Vec::new();
    let add = |out: &mut Vec<(String, String)>, name: &str, value: String| {
        out.push((name.to_owned(), value));
    };
    let input = store.layout_input(id);
    let size = input.size();
    match input {
        LayoutInput::Flex {
            axis,
            gap,
            padding,
            align,
            justify,
            ..
        } => {
            add(&mut layout, "kind", "flex".into());
            add(&mut layout, "axis", axis_name(axis).into());
            add(&mut layout, "gap", number(gap));
            add(
                &mut layout,
                "padding",
                inset([padding.top, padding.right, padding.bottom, padding.left]),
            );
            add(&mut layout, "align", align_name(align).into());
            add(&mut layout, "justify", justify_name(justify).into());
        }
        LayoutInput::Leaf { .. } => add(&mut layout, "kind", "leaf".into()),
        LayoutInput::Scroll { axis, .. } => {
            add(&mut layout, "kind", "scroll".into());
            add(&mut layout, "axis", axis_name(axis).into());
            add(&mut layout, "range", number(store.scroll_range(id, axis)));
        }
        LayoutInput::AbsoluteRows { axis, .. } => {
            add(&mut layout, "kind", "rows".into());
            add(&mut layout, "axis", axis_name(axis).into());
        }
        LayoutInput::Grid {
            column_count,
            row_count,
            column_gap,
            row_gap,
            padding,
            align_items,
            ..
        } => {
            add(&mut layout, "kind", "grid".into());
            add(&mut layout, "columns", column_count.to_string());
            add(&mut layout, "rows", row_count.to_string());
            add(
                &mut layout,
                "gap",
                format!("{} {}", number(row_gap), number(column_gap)),
            );
            add(
                &mut layout,
                "padding",
                inset([padding.top, padding.right, padding.bottom, padding.left]),
            );
            add(
                &mut layout,
                "align_items",
                align_items_name(align_items).into(),
            );
        }
    }
    add(&mut layout, "width", length(size.width));
    add(&mut layout, "height", length(size.height));
    let translate = store.translate(id);
    if translate.x != 0.0 || translate.y != 0.0 {
        add(
            &mut layout,
            "translate",
            format!("{} {}", number(translate.x), number(translate.y)),
        );
    }

    let mut style = Vec::new();
    let resolved = store.style(id);
    add(&mut style, "background", color(resolved.fill));
    if resolved.radius != 0.0 {
        add(&mut style, "radius", number(resolved.radius));
    }
    if resolved.border.width != 0.0 {
        add(
            &mut style,
            "border",
            format!(
                "{} {}",
                number(resolved.border.width),
                color(resolved.border.color)
            ),
        );
    }
    add(&mut style, "opacity", number(store.opacity(id)));
    if store.hidden(id) {
        add(&mut style, "hidden", "true".into());
    }
    if store.is_overlay(id) {
        add(&mut style, "overlay", "true".into());
    }
    add(&mut style, "hit_test", store.hittable(id).to_string());
    if let Some(style_id) = store.style_id(id) {
        add(&mut style, "style", format!("{style_id:?}"));
    }
    if store.interaction_style(id).is_some() {
        add(&mut style, "interaction", "styled".into());
    }

    let mut content = Vec::new();
    match store.content_payload(id) {
        None => {}
        Some(Content::Text {
            glyphs,
            color_glyphs,
            color: text_color,
            natural,
            baseline,
            shaped_at_width,
            soft_wrap,
            ..
        }) => {
            add(&mut content, "kind", "text".into());
            add(
                &mut content,
                "glyphs",
                (glyphs.len() + color_glyphs.len()).to_string(),
            );
            add(&mut content, "color", color(*text_color));
            if let Some(size) = store.resolved_font_size(id) {
                add(&mut content, "font_size", number(size));
            }
            add(
                &mut content,
                "natural",
                format!("{}x{}", number(natural.x), number(natural.y)),
            );
            add(&mut content, "baseline", number(*baseline));
            add(&mut content, "soft_wrap", soft_wrap.to_string());
            if let Some(width) = shaped_at_width {
                add(&mut content, "shaped_at", number(*width));
            }
        }
        Some(Content::Image { natural, tint, .. }) => {
            add(&mut content, "kind", "image".into());
            add(
                &mut content,
                "natural",
                format!("{}x{}", number(natural.x), number(natural.y)),
            );
            add(&mut content, "tint", color(*tint));
        }
        Some(Content::Path { .. }) => add(&mut content, "kind", "path".into()),
        Some(Content::Shader { .. }) => add(&mut content, "kind", "shader".into()),
        Some(Content::Asset { .. }) => add(&mut content, "kind", "asset".into()),
    }
    if let Some(request) = store.text_request(id) {
        add(&mut content, "pending_text", request.text.clone());
    }

    [("layout", layout), ("style", style), ("content", content)]
        .into_iter()
        .filter(|(_, properties)| !properties.is_empty())
        .map(|(name, properties)| PropertyGroup {
            name: name.to_owned(),
            properties,
        })
        .collect()
}

fn axis_name(axis: Axis) -> &'static str {
    match axis {
        Axis::Row => "row",
        Axis::Column => "column",
    }
}

fn align_name(align: Align) -> &'static str {
    match align {
        Align::Start => "start",
        Align::Center => "center",
        Align::End => "end",
        Align::Stretch => "stretch",
    }
}

fn align_items_name(align: AlignItems) -> &'static str {
    match align {
        AlignItems::Stretch => "stretch",
        AlignItems::Start => "start",
        AlignItems::Center => "center",
        AlignItems::End => "end",
        AlignItems::Baseline => "baseline",
    }
}

fn justify_name(justify: Justify) -> &'static str {
    match justify {
        Justify::Start => "start",
        Justify::Center => "center",
        Justify::End => "end",
    }
}

/// A length as `.vs` spells it.
fn length(length: Length) -> String {
    match length {
        Length::Fixed(dp) => format!("{}dp", number(dp)),
        Length::Relative { fixed: 0.0, pct } => format!("{}%", number(pct * 100.0)),
        Length::Relative { fixed, pct } => {
            format!("{}dp + {}%", number(fixed), number(pct * 100.0))
        }
        Length::Fill { weight: 1.0 } => "fill".into(),
        Length::Fill { weight } => format!("fill({})", number(weight)),
        Length::Fit => "fit".into(),
    }
}

/// Four edges, top first, collapsed as CSS writes them.
fn inset([top, right, bottom, left]: [f32; 4]) -> String {
    if top == right && right == bottom && bottom == left {
        number(top)
    } else if top == bottom && right == left {
        format!("{} {}", number(top), number(right))
    } else {
        format!(
            "{} {} {} {}",
            number(top),
            number(right),
            number(bottom),
            number(left)
        )
    }
}

/// A number with no trailing zeros, to two decimals.
fn number(value: f32) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

/// A color as `#rrggbbaa`, `transparent` when fully so.
fn color(color: Rgba) -> String {
    if color.a == 0.0 {
        return "transparent".into();
    }
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let [r, g, b, a] = [byte(color.r), byte(color.g), byte(color.b), byte(color.a)];
    if a == 255 {
        format!("#{r:02x}{g:02x}{b:02x}")
    } else {
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }
}
