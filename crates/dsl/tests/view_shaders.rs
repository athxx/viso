//! A view's shaders: the view names each shader its shader views paint with
//! by its index in its table, the release package carries each packed, a
//! node paints with the loaded program and its value's parameters, the nodes
//! of one shader share a slot, and a reload that changes a shader's code
//! repaints its nodes with the new program.

mod support;

use std::sync::Arc;

use support::Rt;
use viso_dsl::frontend::compile_file;
use viso_dsl::view_behavior::view_behavior;
use viso_ui::shader::{BrushProgram, ProgramSlot};
use viso_ui::{Content, NodeId, Srgb};

/// A brush named `name` with `members` beside the reserved ones, filling its
/// box with `fill`.
fn brush(name: &str, members: &str, fill: &str) -> String {
    format!(
        "shader {name} {{
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
    {members}
    vertex(vertex_id: U32) -> VertexOutput {{
        let unit = quad_vertex(vertex_id);
        return VertexOutput {{
            clip_position: to_clip(rect_pos + unit * rect_size, viewport_size),
        }};
    }}
    fragment() -> Vec4F32 {{
        return {fill};
    }}
}}"
    )
}

/// A card painting two ripples and a glow, the first ripple's rings a state
/// a click adds to; the ripple fills with `fill`. The glow comes first, so an
/// edit of the ripple leaves its bytes as they were.
fn card(fill: &str) -> String {
    let ripple = brush(
        "Ripple",
        "instance tint: ColorLinear;
    instance rings: F32;
    instance center: Vec2F32;
    instance seed: U32;",
        fill,
    );
    let glow = brush(
        "Glow",
        "instance glow: F32;",
        "Vec4F32(glow, glow, glow, 1.0)",
    );
    format!(
        "import viso::math::Vec2F32;
{glow}
{ripple}
export component Card {{
    state rings: F32 = 8.0;
    view {{
        Column {{
            width: 400dp;
            height: 300dp;
            Text {{ width: 20dp; height: 20dp; on click {{ rings = rings + 1.0; }} }}
            ShaderView {{
                shader: Ripple {{ tint: #3366ff, rings: rings, center: Vec2F32::new(0.5, 0.25), seed: 7 }};
                width: 120dp;
                height: 80dp;
            }}
            ShaderView {{
                shader: Ripple {{ tint: #ff0000, rings: 2.0, center: Vec2F32::new(0.0, 0.0), seed: 1 }};
                width: 10dp;
                height: 10dp;
            }}
            ShaderView {{ shader: Glow {{ glow: 0.5 }}; width: 10dp; height: 10dp; }}
        }}
    }}
}}"
    )
}

const FILL: &str = "Vec4F32(tint.r, tint.g, tint.b, 1.0)";
const DIMMER: &str = "Vec4F32(tint.r * 0.5, tint.g * 0.5, tint.b * 0.5, 1.0)";

/// The slot node `node` paints with and its encoded parameters.
fn painted(rt: &Rt, node: NodeId) -> (ProgramSlot, Arc<[u8]>) {
    match rt.store.content_payload(node) {
        Some(Content::Shader {
            program, params, ..
        }) => (*program, Arc::clone(params)),
        other => panic!("{other:?}"),
    }
}

/// The program in `slot`.
fn program(rt: &Rt, slot: ProgramSlot) -> Arc<BrushProgram> {
    Arc::clone(rt.store.shaders().get(slot).expect("an occupied slot"))
}

/// The lanes of parameter `name` in `params`, encoded by `program`.
fn lanes(program: &BrushProgram, params: &[u8], name: &str, count: usize) -> Vec<u32> {
    let param = &program.layout.params[program.layout.param(name).expect("a parameter")];
    let at = param.offset as usize;
    (0..count)
        .map(|lane| {
            let bytes = &params[at + 4 * lane..at + 4 * lane + 4];
            u32::from_le_bytes(bytes.try_into().unwrap())
        })
        .collect()
}

fn floats(program: &BrushProgram, params: &[u8], name: &str, count: usize) -> Vec<f32> {
    lanes(program, params, name, count)
        .into_iter()
        .map(f32::from_bits)
        .collect()
}

#[test]
fn a_view_names_each_shader_it_paints_with_once_in_its_table() {
    let compiled = compile_file(&card(FILL), &support::origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let view = view_behavior(&compiled).unwrap().unwrap();
    let names: Vec<&str> = view
        .shaders
        .iter()
        .map(|brush| &*brush.program.name)
        .collect();
    assert_eq!(names, ["Ripple", "Glow"]);
    let shaders: Vec<Option<u32>> = view
        .controls()
        .map(|(_, control)| control.shader)
        .filter(Option::is_some)
        .collect();
    assert_eq!(shaders, [Some(0), Some(0), Some(1)]);
}

#[test]
fn a_packaged_shader_view_paints_with_its_program_and_parameters() {
    let mut rt = Rt::packaged(&card(FILL));
    let nodes = rt.descendants();
    let (first, second, glow) = (nodes[2], nodes[3], nodes[4]);
    let (slot, params) = painted(&rt, first);
    assert_eq!(painted(&rt, second).0, slot, "one shader, one slot");
    assert_ne!(painted(&rt, glow).0, slot);
    assert_eq!(rt.store.shaders().iter().count(), 2);

    let ripple = program(&rt, slot);
    assert_eq!(&*ripple.name, "Ripple");
    assert_eq!(params.len(), ripple.layout.stride as usize);
    let tint = Srgb::from_rgba32(0x3366_ffff).into_linear_straight();
    assert_eq!(
        floats(&ripple, &params, "tint", 4),
        [tint.r, tint.g, tint.b, tint.a]
    );
    assert_eq!(floats(&ripple, &params, "rings", 1), [8.0]);
    assert_eq!(floats(&ripple, &params, "center", 2), [0.5, 0.25]);
    assert_eq!(lanes(&ripple, &params, "seed", 1), [7]);

    rt.click(0);
    let (after, params) = painted(&rt, first);
    assert_eq!(after, slot, "a parameter change keeps the program");
    assert_eq!(floats(&ripple, &params, "rings", 1), [9.0]);
    assert_eq!(floats(&ripple, &painted(&rt, second).1, "rings", 1), [2.0]);
}

#[test]
fn a_reload_changing_a_shaders_code_repaints_its_nodes_with_the_new_program() {
    let mut rt = Rt::mount(&card(FILL));
    let nodes = rt.descendants();
    let (first, glow) = (nodes[2], nodes[4]);
    let ripple = program(&rt, painted(&rt, first).0);
    let glowing = program(&rt, painted(&rt, glow).0);

    rt.reload(&card(DIMMER));
    let nodes = rt.descendants();
    let (first, second, glow) = (nodes[2], nodes[3], nodes[4]);
    let dimmer = program(&rt, painted(&rt, first).0);
    assert!(!Arc::ptr_eq(&dimmer, &ripple), "the new code");
    assert_eq!(painted(&rt, second).0, painted(&rt, first).0);
    assert!(
        Arc::ptr_eq(&program(&rt, painted(&rt, glow).0), &glowing),
        "an unchanged shader keeps its program"
    );
    assert_eq!(
        rt.store.shaders().iter().count(),
        2,
        "the old program left with its nodes"
    );
}
