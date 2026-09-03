//! `viso-math` microbenchmarks — the hot-path contract's measurement side.
//!
//! Six categories, matching the architecture doc's math bench list, each named
//! for the work it does and the count it does it in:
//!
//! - `math_vec2_ops_10m`               — 10M fused vector ops (add/scale/dot);
//! - `math_mat4_mul_1m`                — 1M `Mat4 * Mat4` (the SIMD-backed path);
//! - `math_affine2_transform_points_10m` — 10M `Affine2::transform_point`;
//! - `math_rect_hit_test_10m`          — 10M `Rect::contains` hit tests;
//! - `math_aabb_intersection_1m`       — 1M `Aabb::intersects`;
//! - `math_transform_chain_100k`       — 100k `Transform3::then` compositions.
//!
//! These are the throughput baselines a later optimization must not regress.
//! Run release (`cargo bench -p viso-math`); criterion defaults to a release
//! profile, and debug timing is not a performance result (the benchmark rule in
//! the engineering guide).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_math::{Aabb, Affine2, Mat4, Point, Quat, Rect, Transform3, Vec2, Vec3, vec2, vec3};

const N_10M: u64 = 10_000_000;
const N_1M: u64 = 1_000_000;
const N_100K: u64 = 100_000;

fn vec2_ops(c: &mut Criterion) {
    c.bench_function("math_vec2_ops_10m", |bencher| {
        bencher.iter(|| {
            // A small register-resident accumulation so the whole loop stays in
            // the vector unit: add, scale, dot, fold the dot back in.
            let mut acc = Vec2::ZERO;
            let step = vec2(0.5, -0.25);
            let mut i = 0u64;
            while i < N_10M {
                acc += step;
                acc *= 1.0000001;
                let d = acc.dot(step);
                acc += vec2(d * 1e-9, -d * 1e-9);
                i += 1;
            }
            black_box(acc)
        })
    });
}

fn mat4_mul(c: &mut Criterion) {
    c.bench_function("math_mat4_mul_1m", |bencher| {
        let a = Mat4::perspective(1.2, 1.6, 0.1, 100.0);
        let b = Mat4::look_at(vec3(3.0, 4.0, 5.0), Vec3::ZERO, Vec3::Y);
        bencher.iter(|| {
            // Chain the product so each iteration depends on the last, defeating
            // the optimizer's urge to hoist a loop-invariant multiply.
            let mut m = black_box(a);
            let mut i = 0u64;
            while i < N_1M {
                m = m.mul(black_box(&b));
                i += 1;
            }
            black_box(m)
        })
    });
}

fn affine2_transform_points(c: &mut Criterion) {
    c.bench_function("math_affine2_transform_points_10m", |bencher| {
        let t = Affine2::from_rotation(0.6).then(&Affine2::from_translation(vec2(12.0, -7.0)));
        bencher.iter(|| {
            let mut p = vec2(1.0, 1.0);
            let mut i = 0u64;
            while i < N_10M {
                // Feed the output back in with a tiny nudge so points do not
                // collapse to a fixed point and the transform stays live.
                p = t.transform_point(p) * 0.5 + vec2(0.001, 0.001);
                i += 1;
            }
            black_box(p)
        })
    });
}

fn rect_hit_test(c: &mut Criterion) {
    c.bench_function("math_rect_hit_test_10m", |bencher| {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        bencher.iter(|| {
            let mut hits = 0u64;
            let mut i = 0u64;
            while i < N_10M {
                // Sweep a point across and past the rect so both the inside and
                // outside branches are exercised.
                let x = (i % 200) as f32 * 0.6;
                let y = (i % 137) as f32 * 0.8;
                if r.contains(Point::new(x, y)) {
                    hits += 1;
                }
                i += 1;
            }
            black_box(hits)
        })
    });
}

fn aabb_intersection(c: &mut Criterion) {
    c.bench_function("math_aabb_intersection_1m", |bencher| {
        let a = Aabb::new(vec3(0.0, 0.0, 0.0), vec3(10.0, 10.0, 10.0));
        bencher.iter(|| {
            let mut overlaps = 0u64;
            let mut i = 0u64;
            while i < N_1M {
                let o = (i % 30) as f32;
                let b = Aabb::new(vec3(o, o, o), vec3(o + 5.0, o + 5.0, o + 5.0));
                if a.intersects(b) {
                    overlaps += 1;
                }
                i += 1;
            }
            black_box(overlaps)
        })
    });
}

fn transform_chain(c: &mut Criterion) {
    c.bench_function("math_transform_chain_100k", |bencher| {
        let step = Transform3 {
            rotation: Quat::from_axis_angle(Vec3::Y, 0.01),
            translation: vec3(0.1, 0.0, -0.05),
        };
        bencher.iter(|| {
            let mut acc = Transform3::IDENTITY;
            let mut i = 0u64;
            while i < N_100K {
                acc = acc.then(black_box(&step));
                i += 1;
            }
            black_box(acc)
        })
    });
}

criterion_group!(
    benches,
    vec2_ops,
    mat4_mul,
    affine2_transform_points,
    rect_hit_test,
    aabb_intersection,
    transform_chain
);
criterion_main!(benches);
