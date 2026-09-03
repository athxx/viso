//! Basic 3D spatial primitives: [`Ray`], [`Plane`], [`Aabb`].
//!
//! These back picking, culling, and camera math. They are `f32`, `#[repr(C)]`,
//! and allocation-free like the rest of the crate. Intersection queries return
//! `Option` rather than a sentinel so a miss is unambiguous.

use crate::vec::Vec3;

/// A half-line: an `origin` plus a `direction` (not required to be unit; the
/// returned `t` from [`Ray::intersect_plane`] is in units of `direction`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ray {
    /// Starting point.
    pub origin: Vec3,
    /// Travel direction.
    pub direction: Vec3,
}

/// An infinite plane in the form `dot(normal, p) == distance` — `normal` is the
/// (assumed unit) plane normal and `distance` is its signed offset from the
/// origin along that normal.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Plane {
    /// Plane normal, assumed unit length.
    pub normal: Vec3,
    /// Signed distance from the origin along `normal`.
    pub distance: f32,
}

/// An axis-aligned bounding box, stored as its `min` and `max` corners.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// The corner with the smallest coordinates.
    pub min: Vec3,
    /// The corner with the largest coordinates.
    pub max: Vec3,
}

impl Ray {
    /// Builds a ray.
    #[inline]
    pub const fn new(origin: Vec3, direction: Vec3) -> Ray {
        Ray { origin, direction }
    }

    /// The point at parameter `t` along the ray: `origin + t * direction`.
    #[inline]
    pub fn at(self, t: f32) -> Vec3 {
        self.origin + self.direction * t
    }

    /// The parameter `t` at which the ray crosses `plane`, or `None` when the
    /// ray is parallel to the plane (or points away — a negative `t` still
    /// returns, meaning the crossing is behind the origin; callers filter on
    /// sign). Use [`Ray::at`] to recover the point.
    pub fn intersect_plane(self, plane: Plane) -> Option<f32> {
        let denom = plane.normal.dot(self.direction);
        if denom.abs() < f32::EPSILON {
            // Parallel: either no hit, or the ray lies in the plane.
            return None;
        }
        let t = (plane.distance - plane.normal.dot(self.origin)) / denom;
        Some(t)
    }
}

impl Plane {
    /// A plane through `point` with the given (assumed unit) `normal`.
    #[inline]
    pub fn from_point_normal(point: Vec3, normal: Vec3) -> Plane {
        Plane {
            normal,
            distance: normal.dot(point),
        }
    }

    /// The signed distance from `point` to the plane (positive on the side the
    /// normal points toward).
    #[inline]
    pub fn signed_distance(self, point: Vec3) -> f32 {
        self.normal.dot(point) - self.distance
    }
}

impl Aabb {
    /// Builds a box from its two corners (assumes `min <= max` componentwise).
    #[inline]
    pub const fn new(min: Vec3, max: Vec3) -> Aabb {
        Aabb { min, max }
    }

    /// The box that contains a single point (`min == max == p`), the identity
    /// for [`Aabb::union`].
    #[inline]
    pub const fn from_point(p: Vec3) -> Aabb {
        Aabb { min: p, max: p }
    }

    /// The center point.
    #[inline]
    pub fn center(self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// Whether the point lies inside (inclusive on all faces).
    #[inline]
    pub fn contains(self, p: Vec3) -> bool {
        p.x >= self.min.x
            && p.x <= self.max.x
            && p.y >= self.min.y
            && p.y <= self.max.y
            && p.z >= self.min.z
            && p.z <= self.max.z
    }

    /// Whether the two boxes overlap (inclusive: boxes touching on a face count
    /// as intersecting, matching the inclusive [`contains`](Self::contains)).
    #[inline]
    pub fn intersects(self, other: Aabb) -> bool {
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
            && self.min.z <= other.max.z
            && self.max.z >= other.min.z
    }

    /// The smallest box containing both.
    #[inline]
    pub fn union(self, other: Aabb) -> Aabb {
        Aabb {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    /// The box grown to include `p`.
    #[inline]
    pub fn extend(self, p: Vec3) -> Aabb {
        Aabb {
            min: self.min.min(p),
            max: self.max.max(p),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec::vec3;

    #[test]
    fn ray_hits_plane_in_front() {
        // Plane z = 5, ray from origin along +z.
        let plane = Plane::from_point_normal(vec3(0.0, 0.0, 5.0), Vec3::Z);
        let ray = Ray::new(vec3(0.0, 0.0, 0.0), vec3(0.0, 0.0, 1.0));
        let t = ray.intersect_plane(plane).unwrap();
        assert!((t - 5.0).abs() < 1e-6);
        assert_eq!(ray.at(t), vec3(0.0, 0.0, 5.0));
    }

    #[test]
    fn ray_parallel_to_plane_misses() {
        let plane = Plane::from_point_normal(vec3(0.0, 0.0, 5.0), Vec3::Z);
        let ray = Ray::new(vec3(0.0, 0.0, 0.0), vec3(1.0, 0.0, 0.0));
        assert!(ray.intersect_plane(plane).is_none());
    }

    #[test]
    fn plane_signed_distance_has_sign() {
        let plane = Plane::from_point_normal(vec3(0.0, 0.0, 0.0), Vec3::Y);
        assert!(plane.signed_distance(vec3(0.0, 3.0, 0.0)) > 0.0);
        assert!(plane.signed_distance(vec3(0.0, -3.0, 0.0)) < 0.0);
    }

    #[test]
    fn aabb_contains_inclusive() {
        let b = Aabb::new(vec3(0.0, 0.0, 0.0), vec3(10.0, 10.0, 10.0));
        assert!(b.contains(vec3(5.0, 5.0, 5.0)));
        assert!(b.contains(vec3(0.0, 0.0, 0.0))); // face inclusive
        assert!(b.contains(vec3(10.0, 10.0, 10.0)));
        assert!(!b.contains(vec3(10.001, 5.0, 5.0)));
    }

    #[test]
    fn aabb_intersects_and_disjoint() {
        let a = Aabb::new(vec3(0.0, 0.0, 0.0), vec3(5.0, 5.0, 5.0));
        let b = Aabb::new(vec3(4.0, 4.0, 4.0), vec3(9.0, 9.0, 9.0));
        let c = Aabb::new(vec3(20.0, 20.0, 20.0), vec3(25.0, 25.0, 25.0));
        assert!(a.intersects(b));
        assert!(!a.intersects(c));
    }

    #[test]
    fn aabb_union_grows_to_cover_both() {
        let a = Aabb::new(vec3(0.0, 0.0, 0.0), vec3(1.0, 1.0, 1.0));
        let b = Aabb::new(vec3(5.0, -2.0, 3.0), vec3(6.0, 0.0, 4.0));
        let u = a.union(b);
        assert_eq!(u.min, vec3(0.0, -2.0, 0.0));
        assert_eq!(u.max, vec3(6.0, 1.0, 4.0));
    }

    #[test]
    fn aabb_extend_absorbs_point() {
        let b = Aabb::from_point(vec3(1.0, 1.0, 1.0)).extend(vec3(-1.0, 3.0, 2.0));
        assert_eq!(b.min, vec3(-1.0, 1.0, 1.0));
        assert_eq!(b.max, vec3(1.0, 3.0, 2.0));
    }
}
