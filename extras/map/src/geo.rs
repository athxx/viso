//! Geographic math: longitude and latitude, the Web Mercator world they
//! project into, bounds, and distances on the sphere.
//!
//! The *world* is the Web Mercator square scaled to `[0, 1]` on both axes:
//! `x` runs east from the antimeridian, `y` south from the latitude where
//! the square ends ([`MAX_LATITUDE`]). A tile `(z, x, y)` covers the world
//! square `[x, x + 1] / 2^z × [y, y + 1] / 2^z`. World math is `f64`: at
//! zoom 22 a point of screen is a ten-billionth of the world.

use std::f64::consts::PI;

use crate::source::TileId;

/// The latitude, north and south, where the Web Mercator square ends.
pub const MAX_LATITUDE: f64 = 85.051_128_779_806_59;

/// The Earth's radius the Web Mercator projection assumes, in meters.
pub const EARTH_RADIUS: f64 = 6_378_137.0;

/// A place on the Earth: degrees of longitude east and latitude north.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LngLat {
    pub lon: f64,
    pub lat: f64,
}

impl LngLat {
    /// The place `lon` degrees east and `lat` degrees north.
    pub const fn new(lon: f64, lat: f64) -> Self {
        Self { lon, lat }
    }

    /// The place's point in the world square, its latitude clamped to the
    /// square.
    pub fn to_world(self) -> WorldPoint {
        let lat = self.lat.clamp(-MAX_LATITUDE, MAX_LATITUDE).to_radians();
        WorldPoint {
            x: (self.lon + 180.0) / 360.0,
            y: 0.5 - (PI / 4.0 + lat / 2.0).tan().ln() / (2.0 * PI),
        }
    }

    /// The same place with its longitude wrapped into `[-180, 180)`.
    pub fn wrapped(self) -> Self {
        Self {
            lon: (self.lon + 180.0).rem_euclid(360.0) - 180.0,
            lat: self.lat,
        }
    }

    /// The great-circle distance to `other`, in meters.
    pub fn distance(self, other: Self) -> f64 {
        let (p1, p2) = (self.lat.to_radians(), other.lat.to_radians());
        let dp = p2 - p1;
        let dl = (other.lon - self.lon).to_radians();
        let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
        2.0 * EARTH_RADIUS * a.sqrt().min(1.0).asin()
    }

    /// The place `meters` away along the initial `bearing` (degrees
    /// clockwise from north), on the sphere.
    pub fn offset(self, bearing: f64, meters: f64) -> Self {
        let d = meters / EARTH_RADIUS;
        let b = bearing.to_radians();
        let (p1, l1) = (self.lat.to_radians(), self.lon.to_radians());
        let p2 = (p1.sin() * d.cos() + p1.cos() * d.sin() * b.cos()).asin();
        let l2 = l1 + (b.sin() * d.sin() * p1.cos()).atan2(d.cos() - p1.sin() * p2.sin());
        Self::new(l2.to_degrees(), p2.to_degrees())
    }
}

/// A point of the world square (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WorldPoint {
    pub x: f64,
    pub y: f64,
}

impl WorldPoint {
    /// The point `(x, y)` of the world square.
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// The place this point projects from.
    pub fn to_lng_lat(self) -> LngLat {
        let n = PI - 2.0 * PI * self.y;
        LngLat {
            lon: self.x * 360.0 - 180.0,
            lat: n.sinh().atan().to_degrees(),
        }
    }

    /// The tile at zoom `z` holding this point, its column wrapped round
    /// the world and its row clamped to it.
    pub fn tile(self, z: u8) -> TileId {
        let span = TileId::span(z) as f64;
        let x = (self.x.rem_euclid(1.0) * span).floor() as u32;
        let y = (self.y.clamp(0.0, 1.0) * span).floor().min(span - 1.0) as u32;
        TileId::new(z, x.min(span as u32 - 1), y)
    }

    /// Meters on the ground per world unit at this point's latitude.
    pub fn meters_per_unit(self) -> f64 {
        let lat = self.to_lng_lat().lat.to_radians();
        2.0 * PI * EARTH_RADIUS * lat.cos()
    }
}

impl std::ops::Add for WorldPoint {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.x + o.x, self.y + o.y)
    }
}

impl std::ops::Sub for WorldPoint {
    type Output = Self;
    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y)
    }
}

impl std::ops::Mul<f64> for WorldPoint {
    type Output = Self;
    fn mul(self, k: f64) -> Self {
        Self::new(self.x * k, self.y * k)
    }
}

/// The world square a tile covers: its north-west corner and its side.
pub fn tile_square(tile: TileId) -> (WorldPoint, f64) {
    let side = 1.0 / TileId::span(tile.z) as f64;
    (
        WorldPoint::new(tile.x as f64 * side, tile.y as f64 * side),
        side,
    )
}

/// A region between two meridians and two parallels. `west > east` crosses
/// the antimeridian.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LngLatBounds {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl LngLatBounds {
    /// The region from `south_west` to `north_east`.
    pub const fn new(south_west: LngLat, north_east: LngLat) -> Self {
        Self {
            west: south_west.lon,
            south: south_west.lat,
            east: north_east.lon,
            north: north_east.lat,
        }
    }

    /// The whole world the projection shows.
    pub const WORLD: Self = Self {
        west: -180.0,
        south: -MAX_LATITUDE,
        east: 180.0,
        north: MAX_LATITUDE,
    };

    /// The smallest region holding every place of `places`; `None` for no
    /// places. It never crosses the antimeridian.
    pub fn around(places: impl IntoIterator<Item = LngLat>) -> Option<Self> {
        let mut places = places.into_iter();
        let first = places.next()?;
        let mut b = Self::new(first, first);
        for p in places {
            b.west = b.west.min(p.lon);
            b.east = b.east.max(p.lon);
            b.south = b.south.min(p.lat);
            b.north = b.north.max(p.lat);
        }
        Some(b)
    }

    /// The region grown to hold `place`.
    pub fn extend(self, place: LngLat) -> Self {
        Self {
            west: self.west.min(place.lon),
            south: self.south.min(place.lat),
            east: self.east.max(place.lon),
            north: self.north.max(place.lat),
        }
    }

    /// Whether `place` lies in the region.
    pub fn contains(self, place: LngLat) -> bool {
        let lon = place.wrapped().lon;
        let in_lon = if self.west <= self.east {
            (self.west..=self.east).contains(&lon)
        } else {
            lon >= self.west || lon <= self.east
        };
        in_lon && (self.south..=self.north).contains(&place.lat)
    }

    /// The region's north-west and south-east corners in the world square;
    /// one that crosses the antimeridian ends east of `x = 1`.
    pub fn to_world(self) -> (WorldPoint, WorldPoint) {
        let nw = LngLat::new(self.west, self.north).to_world();
        let mut se = LngLat::new(self.east, self.south).to_world();
        if se.x < nw.x {
            se.x += 1.0;
        }
        (nw, se)
    }

    /// The point midway across the region in the world square.
    pub fn center(self) -> LngLat {
        let (nw, se) = self.to_world();
        WorldPoint::new((nw.x + se.x) / 2.0, (nw.y + se.y) / 2.0)
            .to_lng_lat()
            .wrapped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn places_round_trip_through_the_world() {
        for (lon, lat) in [(0.0, 0.0), (13.4, 52.52), (-122.42, 37.77), (151.2, -33.87)] {
            let w = LngLat::new(lon, lat).to_world();
            let back = w.to_lng_lat();
            assert!(close(back.lon, lon, 1e-9) && close(back.lat, lat, 1e-9));
        }
        assert_eq!(LngLat::new(0.0, 0.0).to_world(), WorldPoint::new(0.5, 0.5));
        let top = LngLat::new(-180.0, MAX_LATITUDE).to_world();
        assert!(close(top.x, 0.0, 1e-12) && close(top.y, 0.0, 1e-9));
        assert!(close(LngLat::new(0.0, 89.9).to_world().y, 0.0, 1e-9));
    }

    #[test]
    fn points_find_their_tiles() {
        let berlin = LngLat::new(13.4, 52.52).to_world();
        assert_eq!(berlin.tile(0), TileId::new(0, 0, 0));
        assert_eq!(berlin.tile(10), TileId::new(10, 550, 335));
        assert_eq!(WorldPoint::new(1.25, 1.0).tile(2), TileId::new(2, 1, 3));
        let (nw, side) = tile_square(TileId::new(10, 550, 335));
        assert!(berlin.x >= nw.x && berlin.x < nw.x + side);
    }

    #[test]
    fn distances_and_offsets_agree() {
        let paris = LngLat::new(2.3522, 48.8566);
        let london = LngLat::new(-0.1276, 51.5072);
        let d = paris.distance(london);
        assert!(close(d, 343_900.0, 1_500.0), "{d}");
        let east = paris.offset(90.0, 1000.0);
        assert!(close(paris.distance(east), 1000.0, 1e-6));
        let equator = WorldPoint::new(0.5, 0.5).meters_per_unit();
        assert!(close(equator, 40_075_016.7, 1.0));
    }

    #[test]
    fn bounds_hold_their_places_and_cross_the_antimeridian() {
        let b = LngLatBounds::around([LngLat::new(10.0, 50.0), LngLat::new(12.0, 48.0)]).unwrap();
        assert_eq!((b.west, b.south, b.east, b.north), (10.0, 48.0, 12.0, 50.0));
        assert!(b.contains(LngLat::new(11.0, 49.0)) && !b.contains(LngLat::new(13.0, 49.0)));
        let fiji = LngLatBounds::new(LngLat::new(177.0, -19.0), LngLat::new(-178.0, -16.0));
        assert!(fiji.contains(LngLat::new(179.5, -17.0)));
        assert!(fiji.contains(LngLat::new(-179.5, -17.0)));
        let (nw, se) = fiji.to_world();
        assert!(se.x > 1.0 && se.x > nw.x);
        assert!(close(fiji.center().lon, 179.5, 1e-9));
        assert!(LngLatBounds::around([]).is_none());
    }
}
