//! A drive simulated along a route, and the words a navigator shows: where
//! the car is a given number of seconds into the route, which way it faces,
//! and distances and times written for a banner.

use viso_map_nav::{LonLat, ManeuverKind, Route};

/// Metres along `route` the car has come `seconds` into it, at the pace
/// the route was planned for.
pub fn distance_at(route: &Route, seconds: f64) -> f64 {
    let times = &route.times;
    if times.is_empty() {
        return 0.0;
    }
    let i = times.partition_point(|&t| t <= seconds);
    if i == 0 {
        return route.distances[0];
    }
    if i >= times.len() {
        return route.distance();
    }
    let (t0, t1) = (times[i - 1], times[i]);
    let (d0, d1) = (route.distances[i - 1], route.distances[i]);
    if t1 > t0 {
        d0 + (d1 - d0) * (seconds - t0) / (t1 - t0)
    } else {
        d1
    }
}

/// Degrees from north, from `a` toward `b`.
pub fn bearing(a: LonLat, b: LonLat) -> f64 {
    let (la, lb) = (a.lat.to_radians(), b.lat.to_radians());
    let dl = (b.lon - a.lon).to_radians();
    let y = dl.sin() * lb.cos();
    let x = la.cos() * lb.sin() - la.sin() * lb.cos() * dl.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// Where the car is `seconds` into `route`, and which way it faces.
pub fn pose(route: &Route, seconds: f64) -> (LonLat, f64) {
    let d = distance_at(route, seconds);
    let (here, _) = route.point_at(d);
    let ahead = if d + 8.0 <= route.distance() {
        route.point_at(d + 8.0).0
    } else {
        route.points.last().copied().unwrap_or(here)
    };
    let behind = route.point_at((d - 8.0).max(0.0)).0;
    let heading = if ahead != here {
        bearing(here, ahead)
    } else {
        bearing(behind, here)
    };
    (here, heading)
}

/// A distance for a banner: tens of metres up to a kilometre, then
/// kilometres to a tenth.
pub fn distance_text(m: f64) -> String {
    if m < 995.0 {
        format!("{} m", ((m / 10.0).round() * 10.0).max(0.0) as i64)
    } else {
        format!("{:.1} km", m / 1000.0)
    }
}

/// A duration for a banner, in minutes or hours and minutes.
pub fn duration_text(s: f64) -> String {
    let min = (s / 60.0).round().max(1.0) as i64;
    if min < 60 {
        format!("{min} min")
    } else {
        format!("{} h {:02} min", min / 60, min % 60)
    }
}

/// An arrow for a maneuver.
pub fn arrow(kind: ManeuverKind) -> &'static str {
    use ManeuverKind as K;
    match kind {
        K::Depart | K::Continue | K::Merge | K::Ferry => "↑",
        K::SlightLeft | K::KeepLeft | K::RampLeft => "↖",
        K::SlightRight | K::KeepRight | K::RampRight => "↗",
        K::Left | K::SharpLeft => "←",
        K::Right | K::SharpRight => "→",
        K::UTurn => "↶",
        K::Roundabout { .. } => "↻",
        K::Arrive => "◉",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_for_the_banner() {
        assert_eq!(distance_text(4.0), "0 m");
        assert_eq!(distance_text(347.0), "350 m");
        assert_eq!(distance_text(1260.0), "1.3 km");
        assert_eq!(duration_text(30.0), "1 min");
        assert_eq!(duration_text(3900.0), "1 h 05 min");
        assert_eq!(arrow(ManeuverKind::SharpLeft), "←");
        let east = bearing(LonLat::new(6.0, 49.6), LonLat::new(6.1, 49.6));
        assert!((east - 90.0).abs() < 0.1, "{east}");
    }
}
