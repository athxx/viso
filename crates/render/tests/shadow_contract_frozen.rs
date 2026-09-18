//! Frozen public parameter surface of the E0 analytic-shadow fast lane (§15/§20).
//!
//! Three artifacts pin the E0 shadow contract together and must move together:
//!
//! - `instance_abi_frozen.rs` pins the GPU half — the `ShadowInstance` bytes that
//!   land in the device buffer.
//! - The shader-crate MSL oracle pins the codegen half — the fragment coverage and
//!   the `shape`/`inner` dispatch the headless reader mirrors bit-for-bit.
//! - This test pins the *authoring* half — the public `AnalyticShadow` /
//!   `PathShadow` parameter sets (§15.2) and the `to_instance` lowering that bridges
//!   authoring to the ABI. Adding, removing, or re-typing a public parameter, or
//!   changing how a parameter lowers, is a deliberate contract break: update the
//!   shader schema, the headless reader, `instance_abi_frozen.rs`, and this snapshot
//!   together, and re-freeze.
//!
//! The path-shadow coverage-key reuse locality (the {geometry, sigma, spread} key
//! that excludes color/offset, §15.4/§20.2) is machine-frozen by the steady-state
//! bench gate `assert_path_shadow_reuse_is_local`, which runs at bench `--test`
//! startup; this file pins only the reachable public parameter surface.

use viso_render::{AnalyticShadow, Corners, PathShadow, Rect, Rgba, ShadowInstance, ShadowShape};

/// The three §15 silhouette discriminants map to the exact `u32` the shader and the
/// headless `shadow_sdf` dispatch read (0=rounded box, 1=ellipse, 2=capsule). These
/// values are baked into the frozen MSL; renumbering them silently repaints every
/// shadow.
#[test]
fn shadow_shape_discriminants_are_frozen() {
    // `as_u32` is private, so pin the mapping through the public `to_instance` seam.
    let base = AnalyticShadow {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        },
        color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        radius: Corners::uniform(0.0),
        offset: [0.0, 0.0],
        sigma: 1.0,
        spread: 0.0,
        shape: ShadowShape::RoundedBox,
        inner: false,
    };
    assert_eq!(base.to_instance().shape, 0, "RoundedBox");
    assert_eq!(
        AnalyticShadow {
            shape: ShadowShape::Ellipse,
            ..base
        }
        .to_instance()
        .shape,
        1,
        "Ellipse"
    );
    assert_eq!(
        AnalyticShadow {
            shape: ShadowShape::Capsule,
            ..base
        }
        .to_instance()
        .shape,
        2,
        "Capsule"
    );
}

/// `AnalyticShadow::to_instance` lowers every authored parameter to the pinned ABI
/// slot: rect → pos/size, per-corner radii normalized to the rect (§11.2) in
/// left-top / right-top / right-bottom / left-bottom order, straight linear color,
/// and offset/sigma/spread/inner passed through unchanged.
#[test]
fn analytic_shadow_lowering_is_frozen() {
    let shadow = AnalyticShadow {
        rect: Rect {
            x: 3.0,
            y: 5.0,
            w: 40.0,
            h: 20.0,
        },
        color: Rgba::new(0.1, 0.2, 0.3, 0.4),
        radius: Corners {
            left_top: 4.0,
            right_top: 6.0,
            right_bottom: 8.0,
            left_bottom: 2.0,
        },
        offset: [1.5, -2.5],
        sigma: 3.0,
        spread: 1.25,
        shape: ShadowShape::RoundedBox,
        inner: true,
    };
    let inst = shadow.to_instance();
    assert_eq!(inst.rect_pos, [3.0, 5.0]);
    assert_eq!(inst.rect_size, [40.0, 20.0]);
    assert_eq!(inst.color, [0.1, 0.2, 0.3, 0.4]);
    // Radii fit inside the rect, so normalization is identity here; the order is the
    // frozen part.
    assert_eq!(inst.radius, [4.0, 6.0, 8.0, 2.0]);
    assert_eq!(inst.offset, [1.5, -2.5]);
    assert_eq!(inst.sigma, 3.0);
    assert_eq!(inst.spread, 1.25);
    assert_eq!(inst.shape, 0);
    assert_eq!(inst.inner, 1, "inner shadow lowers to 1");
}

/// Oversized authored corner radii scale down proportionally to the smaller
/// half-extent (§11.2), identical to `AnalyticRRect`; the shadow silhouette never
/// self-intersects. Freezing this keeps authored radii forgiving without a separate
/// clamp rule per family.
#[test]
fn analytic_shadow_radius_normalization_is_frozen() {
    let shadow = AnalyticShadow {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        },
        color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        // 8 + 8 = 16 > 10 on each axis: must scale to fit.
        radius: Corners::uniform(8.0),
        offset: [0.0, 0.0],
        sigma: 1.0,
        spread: 0.0,
        shape: ShadowShape::RoundedBox,
        inner: false,
    };
    let r = shadow.to_instance().radius;
    for corner in r {
        assert!(
            corner <= 5.0 + 1e-4,
            "radius {corner} clamps to half-extent"
        );
    }
}

/// The outer-shadow default lowers `inner` to 0; the analytic §20.3 inner lane is
/// opt-in per instance. Pins the two-state routing at the parameter boundary.
#[test]
fn analytic_shadow_inner_defaults_to_outer() {
    let outer = AnalyticShadow {
        rect: Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        },
        color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        radius: Corners::uniform(0.0),
        offset: [0.0, 0.0],
        sigma: 1.0,
        spread: 0.0,
        shape: ShadowShape::RoundedBox,
        inner: false,
    };
    assert_eq!(outer.to_instance().inner, 0);
}

/// `PathShadow` is the general-outline carrier (§15.4): a distinct parameter set
/// from `AnalyticShadow` (no `rect`/`radius`/`shape` — the geometry is the `Path`
/// itself), routing through the tight-coverage-mask fallback. Constructing it with
/// every documented field pins the surface; a field add/remove fails to compile.
#[test]
fn path_shadow_parameter_surface_is_frozen() {
    let shadow = PathShadow {
        color: Rgba::new(0.0, 0.0, 0.0, 0.5),
        offset: [2.0, 3.0],
        sigma: 4.0,
        spread: 1.0,
        inner: false,
    };
    assert_eq!(shadow.color.a, 0.5);
    assert_eq!(shadow.offset, [2.0, 3.0]);
    assert_eq!(shadow.sigma, 4.0);
    assert_eq!(shadow.spread, 1.0);
    assert!(!shadow.inner);
    // Copy semantics: the carrier is a cheap value, not a heap handle.
    let _copy = shadow;
    assert_eq!(shadow, _copy);
}

/// The `ShadowInstance` size is the E0 stride the pool diffs by slot; duplicated
/// here (independently of `instance_abi_frozen.rs`) so a stride change trips both a
/// parameter-surface test and an ABI test, never just one.
#[test]
fn shadow_instance_stride_is_frozen() {
    assert_eq!(std::mem::size_of::<ShadowInstance>(), 72);
}
