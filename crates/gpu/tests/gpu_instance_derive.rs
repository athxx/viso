//! Integration tests for `#[derive(GpuInstance)]`.
//!
//! These live in `viso-gpu` (not `viso-macros`) because the generated code
//! names `viso_gpu::...` types, and `viso-gpu` is the crate that depends on both
//! the trait/layout types and the derive — keeping `viso-macros` a pure leaf.

use viso_gpu::{AttrFormat, GpuInstance, InstanceSchema, LayoutError, SchemaAttr};

/// A realistic quad-ish instance: a scalar, a vec2, a vec3, a vec4, and a uint.
/// Exercises every float format plus an integer field, with the tight
/// `#[repr(C)]` offsets the derive must record via `offset_of!`.
#[repr(C)]
#[derive(Clone, Copy, GpuInstance)]
struct TestInstance {
    depth: f32,       // Float1 @ 0
    pos: [f32; 2],    // Float2 @ 4
    normal: [f32; 3], // Float3 @ 12  (tight: [f32;3] is 12 bytes, 4-byte align)
    color: [f32; 4],  // Float4 @ 24
    flags: u32,       // Uint1  @ 40
}

#[test]
fn stride_matches_size_of() {
    assert_eq!(TestInstance::STRIDE, core::mem::size_of::<TestInstance>());
    // 4 + 8 + 12 + 16 + 4 = 44, no trailing padding (max align is 4).
    assert_eq!(TestInstance::STRIDE, 44);
}

#[test]
fn layout_has_real_offsets_and_formats() {
    let f = TestInstance::LAYOUT.fields;
    assert_eq!(f.len(), 5);

    assert_eq!(f[0].name, "depth");
    assert_eq!(f[0].offset, 0);
    assert_eq!(f[0].format, AttrFormat::Float1);

    assert_eq!(f[1].name, "pos");
    assert_eq!(f[1].offset, 4);
    assert_eq!(f[1].format, AttrFormat::Float2);

    assert_eq!(f[2].name, "normal");
    assert_eq!(f[2].offset, 12);
    assert_eq!(f[2].format, AttrFormat::Float3);

    assert_eq!(f[3].name, "color");
    assert_eq!(f[3].offset, 24);
    assert_eq!(f[3].format, AttrFormat::Float4);

    assert_eq!(f[4].name, "flags");
    assert_eq!(f[4].offset, 40);
    assert_eq!(f[4].format, AttrFormat::Uint1);

    // The recorded offsets are the true struct offsets.
    assert_eq!(f[2].offset, core::mem::offset_of!(TestInstance, normal));
}

#[test]
fn validate_against_matching_schema_ok() {
    let schema = InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "depth",
                format: AttrFormat::Float1,
            },
            SchemaAttr {
                name: "pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "normal",
                format: AttrFormat::Float3,
            },
            SchemaAttr {
                name: "color",
                format: AttrFormat::Float4,
            },
            SchemaAttr {
                name: "flags",
                format: AttrFormat::Uint1,
            },
        ],
    };
    assert_eq!(TestInstance::validate_against(&schema), Ok(()));
}

#[test]
fn validate_against_detects_format_mismatch() {
    let schema = InstanceSchema {
        attributes: &[
            SchemaAttr {
                name: "depth",
                format: AttrFormat::Float1,
            },
            SchemaAttr {
                name: "pos",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "normal",
                format: AttrFormat::Float3,
            },
            // Wrong: color declared as Float2 by the shader.
            SchemaAttr {
                name: "color",
                format: AttrFormat::Float2,
            },
            SchemaAttr {
                name: "flags",
                format: AttrFormat::Uint1,
            },
        ],
    };
    assert_eq!(
        TestInstance::validate_against(&schema),
        Err(LayoutError::FormatMismatch {
            name: "color",
            layout: AttrFormat::Float4,
            schema: AttrFormat::Float2,
        })
    );
}

#[test]
fn validate_against_detects_count_mismatch() {
    let schema = InstanceSchema {
        attributes: &[SchemaAttr {
            name: "depth",
            format: AttrFormat::Float1,
        }],
    };
    assert_eq!(
        TestInstance::validate_against(&schema),
        Err(LayoutError::CountMismatch {
            layout: 5,
            schema: 1
        })
    );
}
