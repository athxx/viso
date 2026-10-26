//! The camera: where the map looks, how close and which way up, and the
//! math between the world and the view it fills.
//!
//! At zoom `z` the world square is `TILE_SIZE · 2^z` points across. The
//! camera's center sits at the middle of the view's padded area; the
//! bearing turns the map so that the compass direction it names points up.
//! A pitch tilts the ground away about that point: the eye then looks at it
//! from above and behind, through a perspective whose field of view spans
//! the view's height, and the point keeps the scale its zoom gives it.

use crate::geo::{LngLat, LngLatBounds, WorldPoint};

/// The side of a tile at a whole zoom, in points.
pub const TILE_SIZE: f64 = 512.0;

/// The deepest zoom a camera reaches.
pub const MAX_ZOOM: f64 = 24.0;

/// The steepest a camera tilts, in degrees from looking straight down.
pub const MAX_PITCH: f64 = 60.0;

/// The vertical field of view of a tilted camera, in radians: the view's
/// height seen from three halves of it away.
pub const FIELD_OF_VIEW: f64 = 0.643_501_108_793_284_4;

/// How far past its focus a tilted view reaches over the ground at most, in
/// eye distances: the ground beyond fades into the sky.
pub const REACH: f64 = 3.0;

/// The nearest a ground point comes to the eye in perspective, in eye
/// distances; nearer ones are pushed off the view.
const NEAREST: f64 = 0.05;

/// Where the map looks: its center, zoom, bearing (degrees clockwise from
/// north that point up) and pitch (degrees the ground tilts away from
/// looking straight down, `0` to [`MAX_PITCH`]).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Camera {
    pub center: LngLat,
    pub zoom: f64,
    pub bearing: f64,
    pub pitch: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self::new(LngLat::default(), 0.0)
    }
}

impl Camera {
    /// A camera over `center` at `zoom`, north up.
    pub const fn new(center: LngLat, zoom: f64) -> Self {
        Self {
            center,
            zoom,
            bearing: 0.0,
            pitch: 0.0,
        }
    }

    /// The camera tilted to `pitch` degrees.
    pub const fn with_pitch(mut self, pitch: f64) -> Self {
        self.pitch = pitch;
        self
    }

    /// The camera turned to `bearing`.
    pub const fn with_bearing(mut self, bearing: f64) -> Self {
        self.bearing = bearing;
        self
    }

    /// The camera at `zoom`.
    pub const fn with_zoom(mut self, zoom: f64) -> Self {
        self.zoom = zoom;
        self
    }

    /// The camera over `center`.
    pub const fn with_center(mut self, center: LngLat) -> Self {
        self.center = center;
        self
    }

    /// Points per world unit at this camera's zoom.
    pub fn scale(&self) -> f64 {
        TILE_SIZE * self.zoom.exp2()
    }
}

/// Space kept clear at a view's edges, in points: the camera centers in
/// what is left, and fitting keeps what it fits out of it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Insets {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Insets {
    /// The same inset on every edge.
    pub const fn all(v: f64) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

/// Wrap `degrees` into `(-180, 180]`.
pub fn wrap_bearing(degrees: f64) -> f64 {
    let b = (degrees + 180.0).rem_euclid(360.0) - 180.0;
    if b == -180.0 { 180.0 } else { b }
}

/// A camera in a view of `size` points, with `padding` kept clear: the
/// map between world and view points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub camera: Camera,
    pub size: [f64; 2],
    pub padding: Insets,
}

impl View {
    /// `camera` filling a view of `size` points, unpadded.
    pub fn new(camera: Camera, size: [f64; 2]) -> Self {
        Self {
            camera,
            size,
            padding: Insets::default(),
        }
    }

    /// The view point the camera's center sits at: the middle of the padded
    /// area.
    pub fn focus(&self) -> [f64; 2] {
        let p = self.padding;
        [
            p.left + (self.size[0] - p.left - p.right) / 2.0,
            p.top + (self.size[1] - p.top - p.bottom) / 2.0,
        ]
    }

    fn turn(&self) -> (f64, f64) {
        let r = (-self.camera.bearing).to_radians();
        (r.cos(), r.sin())
    }

    /// How far the eye of a tilted camera is from the focus, in view points.
    pub fn eye_distance(&self) -> f64 {
        self.size[1].max(1.0) / 2.0 / (FIELD_OF_VIEW / 2.0).tan()
    }

    /// The cosine and sine of the pitch, when the camera is tilted.
    pub fn tilt(&self) -> Option<(f64, f64)> {
        let p = self.camera.pitch.clamp(0.0, MAX_PITCH);
        (p > 0.0).then(|| (p.to_radians().cos(), p.to_radians().sin()))
    }

    /// The view point of world point `w`, unwrapped: a point a world away
    /// lands a world's width away. Tilted, a point behind the eye lands far
    /// below the view and one past the horizon at it.
    pub fn to_view(&self, w: WorldPoint) -> [f64; 2] {
        self.projected(w, 0.0).0
    }

    /// The view point of world point `w` raised `height` points of the
    /// camera's zoom over the ground the camera looks at: where a tilted
    /// view shows a place on relief. Looking straight down, heights do not
    /// move it.
    pub fn to_view_at(&self, w: WorldPoint, height: f64) -> [f64; 2] {
        self.projected(w, height).0
    }

    /// The view point of `w` raised `height` points, and how large things
    /// there show against things at the focus: `1` looking straight down,
    /// less beyond the focus of a tilted view, more before it.
    pub(crate) fn projected(&self, w: WorldPoint, height: f64) -> ([f64; 2], f64) {
        let c = self.camera.center.to_world();
        let s = self.camera.scale();
        let (dx, dy) = ((w.x - c.x) * s, (w.y - c.y) * s);
        let (cos, sin) = self.turn();
        let f = self.focus();
        let g = [dx * cos - dy * sin, dx * sin + dy * cos];
        match self.tilt() {
            None => ([f[0] + g[0], f[1] + g[1]], 1.0),
            Some((pc, ps)) => {
                let d = self.eye_distance();
                let depth = (d - g[1] * ps - height * pc).max(d * NEAREST);
                let k = d / depth;
                ([f[0] + g[0] * k, f[1] + (g[1] * pc - height * ps) * k], k)
            }
        }
    }

    /// The world point under view point `p`. Tilted, a point at or above
    /// the horizon finds the ground [`REACH`] eye distances away.
    pub fn to_world(&self, p: [f64; 2]) -> WorldPoint {
        let f = self.focus();
        let (mut dx, mut dy) = (p[0] - f[0], p[1] - f[1]);
        if let Some((pc, ps)) = self.tilt() {
            let d = self.eye_distance();
            let below = pc * d + dy * ps;
            let gy = if below > 0.0 {
                (dy * d / below).max(-REACH * d)
            } else {
                -REACH * d
            };
            dx *= (d - gy * ps) / d;
            dy = gy;
        }
        let (cos, sin) = self.turn();
        let s = self.camera.scale();
        let c = self.camera.center.to_world();
        WorldPoint::new(
            c.x + (dx * cos + dy * sin) / s,
            c.y + (-dx * sin + dy * cos) / s,
        )
    }

    /// The view point of `place`, on the copy of the world nearest the
    /// camera's center.
    pub fn project(&self, place: LngLat) -> [f64; 2] {
        let c = self.camera.center.to_world();
        let mut w = place.to_world();
        w.x += (c.x - w.x).round();
        self.to_view(w)
    }

    /// The place under view point `p`.
    pub fn unproject(&self, p: [f64; 2]) -> LngLat {
        self.to_world(p).to_lng_lat().wrapped()
    }

    /// The world points under the view's corners, clockwise from the top
    /// left.
    pub fn corners(&self) -> [WorldPoint; 4] {
        let [w, h] = self.size;
        [
            self.to_world([0.0, 0.0]),
            self.to_world([w, 0.0]),
            self.to_world([w, h]),
            self.to_world([0.0, h]),
        ]
    }

    /// The world box the view covers, unwrapped: its minimum and maximum
    /// corners.
    pub fn world_box(&self) -> (WorldPoint, WorldPoint) {
        let c = self.corners();
        let (mut lo, mut hi) = (c[0], c[0]);
        for p in &c[1..] {
            lo = WorldPoint::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = WorldPoint::new(hi.x.max(p.x), hi.y.max(p.y));
        }
        (lo, hi)
    }

    /// The view moved so that the world under view point `from` comes to
    /// lie under `to`.
    pub fn panned(&self, from: [f64; 2], to: [f64; 2]) -> Camera {
        let a = self.to_world(from);
        let b = self.to_world(to);
        let c = self.camera.center.to_world() + (a - b);
        self.camera.with_center(c.to_lng_lat())
    }

    /// The camera zoomed by `delta` with the world under view point
    /// `anchor` held still.
    pub fn zoomed_about(&self, delta: f64, anchor: [f64; 2]) -> Camera {
        let before = self.to_world(anchor);
        let mut next = *self;
        next.camera.zoom += delta;
        let after = next.to_world(anchor);
        let c = self.camera.center.to_world() + (before - after);
        next.camera.with_center(c.to_lng_lat())
    }

    /// The camera turned by `degrees` with the world under view point
    /// `anchor` held still.
    pub fn turned_about(&self, degrees: f64, anchor: [f64; 2]) -> Camera {
        let before = self.to_world(anchor);
        let mut next = *self;
        next.camera.bearing = wrap_bearing(self.camera.bearing + degrees);
        let after = next.to_world(anchor);
        let c = self.camera.center.to_world() + (before - after);
        next.camera.with_center(c.to_lng_lat())
    }

    /// The camera that shows `bounds` whole inside the padded view at
    /// `bearing`, as close as `max_zoom` allows.
    pub fn fit(&self, bounds: LngLatBounds, bearing: f64, max_zoom: f64) -> Camera {
        let (nw, se) = bounds.to_world();
        let r = (-bearing).to_radians();
        let (cos, sin) = (r.cos(), r.sin());
        let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for (x, y) in [(nw.x, nw.y), (se.x, nw.y), (se.x, se.y), (nw.x, se.y)] {
            let p = [x * cos - y * sin, x * sin + y * cos];
            for i in 0..2 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        let p = self.padding;
        let room = [
            (self.size[0] - p.left - p.right).max(1.0),
            (self.size[1] - p.top - p.bottom).max(1.0),
        ];
        let span = [(hi[0] - lo[0]).max(1e-12), (hi[1] - lo[1]).max(1e-12)];
        let scale = (room[0] / span[0]).min(room[1] / span[1]);
        let zoom = (scale / TILE_SIZE).log2().min(max_zoom);
        let center = WorldPoint::new((nw.x + se.x) / 2.0, (nw.y + se.y) / 2.0);
        Camera::new(center.to_lng_lat().wrapped(), zoom)
            .with_bearing(wrap_bearing(bearing))
            .with_pitch(self.camera.pitch)
    }
}

/// What the camera may show: a zoom range and, optionally, a region the
/// view stays inside.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub min_zoom: f64,
    pub max_zoom: f64,
    pub bounds: Option<LngLatBounds>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            min_zoom: 0.0,
            max_zoom: 22.0,
            bounds: None,
        }
    }
}

impl Limits {
    /// `view`'s camera brought within the limits: its zoom clamped, its
    /// bearing wrapped, its longitude wrapped round the world, and its center
    /// held so the view shows no space past the poles — or past the bounds,
    /// whose middle it centers on when they are smaller than the view.
    pub fn constrain(&self, view: &View) -> Camera {
        let mut camera = view.camera;
        camera.zoom = camera
            .zoom
            .clamp(self.min_zoom.max(0.0), self.max_zoom.min(MAX_ZOOM));
        camera.bearing = wrap_bearing(camera.bearing);
        camera.pitch = if camera.pitch.is_finite() {
            camera.pitch.clamp(0.0, MAX_PITCH)
        } else {
            0.0
        };
        if !camera.center.lon.is_finite() || !camera.center.lat.is_finite() {
            camera.center = LngLat::default();
        }
        let mut c = camera.center.to_world();
        // How far the view reaches from its center, in world units, turned.
        let s = camera.scale();
        let r = camera.bearing.to_radians();
        let (cos, sin) = (r.cos().abs(), r.sin().abs());
        let [w, h] = view.size;
        let half = [(w * cos + h * sin) / 2.0 / s, (w * sin + h * cos) / 2.0 / s];
        let (lo, hi) = match self.bounds {
            Some(b) => {
                let (nw, se) = b.to_world();
                ([nw.x, nw.y], [se.x, se.y])
            }
            None => ([f64::NEG_INFINITY, 0.0], [f64::INFINITY, 1.0]),
        };
        let clamp_axis = |v: &mut f64, i: usize| {
            if hi[i] - lo[i] <= 2.0 * half[i] {
                *v = (lo[i] + hi[i]) / 2.0;
            } else {
                *v = v.clamp(lo[i] + half[i], hi[i] - half[i]);
            }
        };
        if self.bounds.is_some() {
            clamp_axis(&mut c.x, 0);
        }
        clamp_axis(&mut c.y, 1);
        camera.center = c.to_lng_lat();
        if self.bounds.is_none() {
            camera.center = camera.center.wrapped();
        }
        camera
    }
}

/// A camera move over time: an ease along a straight path or a flight that
/// zooms out, travels and zooms back in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    from: Camera,
    to: Camera,
    /// The world point the move starts and ends at, the end unwrapped to
    /// the nearer copy of the world.
    a: WorldPoint,
    b: WorldPoint,
    kind: Path,
    duration: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Path {
    Ease,
    Fly {
        /// `w0`, `u1`, `r0` and the path length `S` of the zoom-pan curve.
        w0: f64,
        u1: f64,
        r0: f64,
        length: f64,
        /// The curve degenerates to a zoom in place.
        zoom_only: Option<f64>,
    },
}

/// How strongly a flight zooms out: the curvature of its path.
const FLY_CURVE: f64 = 1.42;
/// The pace of a flight, in path lengths a second.
const FLY_SPEED: f64 = 1.2;

fn ease_in_out(t: f64) -> f64 {
    if t <= 0.0 || t >= 1.0 {
        return t.clamp(0.0, 1.0);
    }
    // A cubic Bézier (0.25, 0.1, 0.25, 1) solved for x by bisection: the
    // gentle start and long settle of a camera move.
    let bez = |a: f64, b: f64, t: f64| {
        3.0 * a * t * (1.0 - t).powi(2) + 3.0 * b * t * t * (1.0 - t) + t.powi(3)
    };
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if bez(0.25, 0.25, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(0.1, 1.0, (lo + hi) / 2.0)
}

impl Motion {
    /// A move from `from` to `to` over `duration` seconds along a straight
    /// path, easing in and out.
    pub fn ease(from: Camera, to: Camera, duration: f64) -> Self {
        let (a, b) = Self::ends(&from, &to);
        Self {
            from,
            to,
            a,
            b,
            kind: Path::Ease,
            duration: duration.max(0.0),
        }
    }

    /// A flight from `from` to `to` in a view of `size` points: zooming out
    /// as far as the distance asks, across and back in, at a steady pace on
    /// screen. Its duration follows from the distance unless `duration`
    /// names one.
    pub fn fly(from: Camera, to: Camera, size: [f64; 2], duration: Option<f64>) -> Self {
        let (a, b) = Self::ends(&from, &to);
        let rho = FLY_CURVE;
        let rho2 = rho * rho;
        let w0 = size[0].max(size[1]).max(1.0);
        let w1 = w0 / (to.zoom - from.zoom).exp2();
        let u1 = ((b.x - a.x).hypot(b.y - a.y)) * from.scale();
        let r = |i: bool| {
            let (sign, w) = if i { (-1.0, w1) } else { (1.0, w0) };
            let k = (w1 * w1 - w0 * w0 + sign * rho2 * rho2 * u1 * u1) / (2.0 * w * rho2 * u1);
            ((k * k + 1.0).sqrt() - k).ln()
        };
        let r0 = r(false);
        let mut length = (r(true) - r0) / rho;
        let mut zoom_only = None;
        if u1.abs() < 1e-6 || !length.is_finite() {
            if (w0 - w1).abs() < 1e-6 {
                return Self::ease(from, to, duration.unwrap_or(0.0));
            }
            let k = if w1 < w0 { -1.0 } else { 1.0 };
            length = (w1 / w0).ln().abs() / rho;
            zoom_only = Some(k);
        }
        Self {
            from,
            to,
            a,
            b,
            kind: Path::Fly {
                w0,
                u1,
                r0,
                length,
                zoom_only,
            },
            duration: duration.unwrap_or(length / FLY_SPEED).max(0.0),
        }
    }

    fn ends(from: &Camera, to: &Camera) -> (WorldPoint, WorldPoint) {
        let a = from.center.to_world();
        let mut b = to.center.to_world();
        b.x += (a.x - b.x).round();
        (a, b)
    }

    /// How long the move takes, in seconds.
    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// Where the move ends.
    pub fn target(&self) -> Camera {
        self.to
    }

    /// The camera `t` seconds into the move; the target from its end on.
    pub fn at(&self, t: f64) -> Camera {
        if self.duration <= 0.0 || t >= self.duration {
            return self.to;
        }
        let k = ease_in_out((t / self.duration).clamp(0.0, 1.0));
        let turn = wrap_bearing(self.to.bearing - self.from.bearing);
        let bearing = wrap_bearing(self.from.bearing + turn * k);
        let pitch = self.from.pitch + (self.to.pitch - self.from.pitch) * k;
        let (zoom, along) = match self.kind {
            Path::Ease => (self.from.zoom + (self.to.zoom - self.from.zoom) * k, k),
            Path::Fly {
                w0,
                u1,
                r0,
                length,
                zoom_only,
            } => {
                let s = k * length;
                let rho = FLY_CURVE;
                let (w, u) = match zoom_only {
                    Some(sign) => ((sign * rho * s).exp(), 0.0),
                    None => (
                        r0.cosh() / (r0 + rho * s).cosh(),
                        w0 * ((r0.cosh() * (r0 + rho * s).tanh() - r0.sinh()) / (rho * rho)) / u1,
                    ),
                };
                (self.from.zoom + (1.0 / w).log2(), u)
            }
        };
        let c = self.a + (self.b - self.a) * along;
        Camera::new(c.to_lng_lat().wrapped(), zoom)
            .with_bearing(bearing)
            .with_pitch(pitch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    fn view(camera: Camera) -> View {
        View::new(camera, [800.0, 600.0])
    }

    #[test]
    fn view_and_world_round_trip_at_any_bearing() {
        for bearing in [0.0, 30.0, -135.0, 180.0] {
            let v = view(Camera::new(LngLat::new(13.4, 52.5), 12.3).with_bearing(bearing));
            let p = [123.0, 456.0];
            let back = v.to_view(v.to_world(p));
            assert!(close(back[0], p[0], 1e-6) && close(back[1], p[1], 1e-6));
            let center = v.project(v.camera.center);
            assert!(close(center[0], 400.0, 1e-6) && close(center[1], 300.0, 1e-6));
        }
    }

    #[test]
    fn a_tilted_view_keeps_its_focus_and_round_trips_below_the_horizon() {
        for (bearing, pitch) in [(0.0, 30.0), (-60.0, 45.0), (135.0, 60.0)] {
            let c = Camera::new(LngLat::new(13.4, 52.5), 14.0)
                .with_bearing(bearing)
                .with_pitch(pitch);
            let v = view(c);
            let center = v.project(c.center);
            assert!(close(center[0], 400.0, 1e-6) && close(center[1], 300.0, 1e-6));
            for p in [[123.0, 456.0], [700.0, 80.0], [400.0, 599.0]] {
                let back = v.to_view(v.to_world(p));
                assert!(
                    close(back[0], p[0], 1e-6) && close(back[1], p[1], 1e-6),
                    "{p:?} {back:?}"
                );
            }
            // The focus keeps its scale: a step across it is as long as flat.
            let flat = view(c.with_pitch(0.0));
            let a = v.to_world([401.0, 300.0]);
            let b = flat.to_world([401.0, 300.0]);
            assert!(close(a.x, b.x, 1e-12) && close(a.y, b.y, 1e-12));
            // The top of the view spans more ground than its bottom.
            let top = v.to_world([0.0, 0.0]);
            let top_r = v.to_world([800.0, 0.0]);
            let bottom = v.to_world([0.0, 600.0]);
            let bottom_r = v.to_world([800.0, 600.0]);
            let span = |a: WorldPoint, b: WorldPoint| (a.x - b.x).hypot(a.y - b.y);
            assert!(span(top, top_r) > span(bottom, bottom_r));
        }
        // Past the horizon the ground stops at the reach.
        let v = view(Camera::new(LngLat::new(0.0, 0.0), 10.0).with_pitch(60.0));
        let sky = v.to_world([400.0, -2000.0]);
        let reach = REACH * v.eye_distance() / v.camera.scale();
        assert!(close(-sky.y + v.camera.center.to_world().y, reach, 1e-9));
        // Panning, zooming and turning hold the anchor tilted too.
        let anchor = [150.0, 120.0];
        let under = v.to_world(anchor);
        for camera in [v.zoomed_about(1.3, anchor), v.turned_about(30.0, anchor)] {
            let now = View { camera, ..v }.to_world(anchor);
            assert!(close(now.x, under.x, 1e-12) && close(now.y, under.y, 1e-12));
        }
        let panned = View {
            camera: v.panned([150.0, 120.0], [300.0, 400.0]),
            ..v
        };
        let b = panned.to_world([300.0, 400.0]);
        assert!(close(under.x, b.x, 1e-12) && close(under.y, b.y, 1e-12));
    }

    #[test]
    fn a_raised_point_shows_higher_and_nearer() {
        let mut v = view(
            Camera::new(LngLat::new(8.0, 47.0), 14.0)
                .with_bearing(30.0)
                .with_pitch(50.0),
        );
        let w = v.to_world([300.0, 200.0]);
        let ground = v.to_view(w);
        assert_eq!(v.to_view_at(w, 0.0), ground);
        let up = v.to_view_at(w, 40.0);
        assert!(up[1] < ground[1] - 10.0, "{up:?} {ground:?}");
        // The focus raised stays above the focus, by its height foreshortened.
        let c = v.camera.center.to_world();
        let f = v.focus();
        let raised = v.to_view_at(c, 40.0);
        assert!((raised[0] - f[0]).abs() < 1e-9);
        let (_, k) = v.projected(c, 40.0);
        let ps = 50f64.to_radians().sin();
        assert!((f[1] - raised[1] - 40.0 * ps * k).abs() < 1e-9 && k > 1.0);
        // Looking straight down a height moves nothing.
        v.camera.pitch = 0.0;
        assert_eq!(v.to_view_at(w, 40.0), v.to_view(w));
    }

    #[test]
    fn the_bearing_points_up() {
        // Turned to the east, a place east of the center sits above it.
        let c = LngLat::new(0.0, 0.0);
        let v = view(Camera::new(c, 10.0).with_bearing(90.0));
        let p = v.project(LngLat::new(0.01, 0.0));
        assert!(close(p[0], 400.0, 1e-6) && p[1] < 300.0, "{p:?}");
        // North up, north is up and east is right.
        let v = view(Camera::new(c, 10.0));
        assert!(v.project(LngLat::new(0.0, 0.01))[1] < 300.0);
        assert!(v.project(LngLat::new(0.01, 0.0))[0] > 400.0);
    }

    #[test]
    fn padding_moves_the_focus() {
        let mut v = view(Camera::new(LngLat::new(5.0, 5.0), 8.0));
        v.padding = Insets {
            top: 100.0,
            ..Insets::default()
        };
        assert_eq!(v.focus(), [400.0, 350.0]);
        let p = v.project(v.camera.center);
        assert!(close(p[1], 350.0, 1e-6));
    }

    #[test]
    fn zooming_and_turning_hold_the_anchor() {
        let v = view(Camera::new(LngLat::new(2.35, 48.85), 11.0).with_bearing(20.0));
        let anchor = [100.0, 500.0];
        let under = v.to_world(anchor);
        for camera in [v.zoomed_about(1.7, anchor), v.turned_about(-45.0, anchor)] {
            let moved = View { camera, ..v };
            let now = moved.to_world(anchor);
            assert!(close(now.x, under.x, 1e-12) && close(now.y, under.y, 1e-12));
        }
        let panned = View {
            camera: v.panned([10.0, 10.0], [60.0, 30.0]),
            ..v
        };
        let a = v.to_world([10.0, 10.0]);
        let b = panned.to_world([60.0, 30.0]);
        assert!(close(a.x, b.x, 1e-12) && close(a.y, b.y, 1e-12));
    }

    #[test]
    fn fitting_shows_the_bounds_whole() {
        let b = LngLatBounds::new(LngLat::new(13.0, 52.3), LngLat::new(13.8, 52.7));
        let mut v = view(Camera::default());
        v.padding = Insets::all(20.0);
        for bearing in [0.0, 45.0] {
            let fitted = View {
                camera: v.fit(b, bearing, 22.0),
                ..v
            };
            for corner in [LngLat::new(13.0, 52.3), LngLat::new(13.8, 52.7)] {
                let p = fitted.project(corner);
                assert!(
                    p[0] >= 19.9 && p[0] <= 780.1 && p[1] >= 19.9 && p[1] <= 580.1,
                    "{p:?}"
                );
            }
        }
        assert!(v.fit(b, 0.0, 9.0).zoom <= 9.0);
    }

    #[test]
    fn limits_clamp_zoom_poles_and_bounds() {
        let limits = Limits {
            min_zoom: 2.0,
            max_zoom: 16.0,
            bounds: None,
        };
        let c = limits.constrain(&view(
            Camera::new(LngLat::new(200.0, 89.0), 30.0).with_bearing(270.0),
        ));
        assert_eq!(c.zoom, 16.0);
        assert_eq!(c.bearing, -90.0);
        assert!(close(c.center.lon, -160.0, 1e-9));
        // The top of the view stays at or below the north edge.
        let top = view(c).to_world([400.0, 0.0]);
        assert!(top.y >= -1e-9, "{top:?}");
        // A view larger than the bounds centers on them.
        let region = LngLatBounds::new(LngLat::new(10.0, 50.0), LngLat::new(10.01, 50.01));
        let boxed = Limits {
            bounds: Some(region),
            ..Limits::default()
        };
        let c = boxed.constrain(&view(Camera::new(LngLat::new(0.0, 0.0), 3.0)));
        assert!(close(c.center.lon, 10.005, 1e-6), "{c:?}");
        let c = boxed.constrain(&view(Camera::new(LngLat::new(10.0, 50.0), 22.0)));
        let corner = view(c).to_world([0.0, 0.0]).to_lng_lat();
        assert!(corner.lon >= 10.0 - 1e-9 && corner.lat <= 50.01 + 1e-9);
    }

    #[test]
    fn eases_and_flights_end_where_they_aim() {
        let from = Camera::new(LngLat::new(-122.4, 37.8), 12.0);
        let to = Camera::new(LngLat::new(-74.0, 40.7), 13.0).with_bearing(-30.0);
        let ease = Motion::ease(from, to, 0.5);
        let start = ease.at(0.0).center;
        assert!(close(start.lon, -122.4, 1e-9) && close(start.lat, 37.8, 1e-9));
        assert_eq!(ease.at(1.0), to);
        let mid = ease.at(0.25);
        assert!(mid.zoom > 12.0 && mid.zoom < 13.0 && mid.bearing < 0.0 && mid.bearing > -30.0);
        let tilt = Motion::ease(from, to.with_pitch(50.0), 0.5).at(0.25);
        assert!(tilt.pitch > 0.0 && tilt.pitch < 50.0, "{}", tilt.pitch);

        let fly = Motion::fly(from, to, [800.0, 600.0], None);
        assert!(fly.duration() > 1.0, "{}", fly.duration());
        let mid = fly.at(fly.duration() / 2.0);
        assert!(
            mid.zoom < 10.0,
            "a flight zooms out on the way: {}",
            mid.zoom
        );
        let start = fly.at(0.0);
        assert!(close(start.zoom, 12.0, 1e-9) && close(start.center.lon, -122.4, 1e-9));
        let end = fly.at(fly.duration() * 0.999);
        assert!(close(end.zoom, 13.0, 0.01) && close(end.center.lon, -74.0, 0.01));
        // Zooming in place flies too, without moving.
        let zoom = Motion::fly(from, from.with_zoom(16.0), [800.0, 600.0], None);
        let half = zoom.at(zoom.duration() / 2.0);
        assert!(close(half.center.lon, -122.4, 1e-9) && half.zoom > 12.0);
    }

    #[test]
    fn a_move_across_the_antimeridian_goes_the_short_way() {
        let from = Camera::new(LngLat::new(179.0, 0.0), 5.0);
        let to = Camera::new(LngLat::new(-179.0, 0.0), 5.0);
        let mid = Motion::ease(from, to, 1.0).at(0.5);
        assert!(mid.center.lon.abs() > 179.0, "{:?}", mid.center);
        assert_eq!(wrap_bearing(-180.0), 180.0);
        assert_eq!(wrap_bearing(370.0), 10.0);
    }
}
