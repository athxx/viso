//! Calendar dates, the week and the clock a locale keeps, and the marks
//! under a week's days.
//!
//! A [`Date`] is a day of the proleptic Gregorian calendar. It counts as a
//! number of days from 1970-01-01 ([`Date::days`]), which is how a reactive
//! cell holds one ([`Date::to_state`]): it sorts, subtracts and steps without
//! a calendar, and stepping by months clamps to the end of a short month.
//!
//! [`first_weekday`] and [`hour_cycle`] read a BCP 47 locale for the day its
//! weeks start on and whether its clock runs to twelve or to twenty-four,
//! after the region the tag names — or the one its language is most spoken
//! in — and the tag's `-u-fw-` and `-u-hc-` keywords, which win.
//!
//! [`Days`] is a week row's content ([`Content::Days`]): under the row's
//! days it draws a disc on the selected one, a ring round today, and a faint
//! disc under the one the pointer is over. The row holds no dates: whoever
//! drives it — the `Calendar` widget — tells it which of its days to mark
//! ([`NodeStore::set_days`]).

use std::fmt;

use viso_render::{AnalyticEllipse, Border, Primitive, Rect, Rgba};

use crate::component::NodeStore;
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::node::NodeId;
use crate::state::StateValue;

/// A day of the proleptic Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

/// What a date cell holds when it holds no date.
pub const NO_DATE: i32 = i32::MIN;

impl Date {
    /// The `day` of `month` (`1` to `12`) of `year`, if there is one.
    pub fn new(year: i32, month: u32, day: u32) -> Option<Date> {
        if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date {
            year,
            month: month as u8,
            day: day as u8,
        })
    }

    /// The date `days` days after 1970-01-01, or before it when negative.
    pub fn from_days(days: i32) -> Date {
        // Howard Hinnant's `civil_from_days`, exact across the calendar.
        let z = i64::from(days) + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Date {
            year: year.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            month: month as u8,
            day: day as u8,
        }
    }

    /// The days from 1970-01-01 to it, negative before.
    pub fn days(self) -> i32 {
        let (m, d) = (i64::from(self.month), i64::from(self.day));
        let y = i64::from(self.year) - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        (era * 146_097 + doe - 719_468).clamp(i64::from(i32::MIN) + 1, i64::from(i32::MAX)) as i32
    }

    pub fn year(self) -> i32 {
        self.year
    }

    /// Its month, `1` to `12`.
    pub fn month(self) -> u32 {
        u32::from(self.month)
    }

    /// Its day of the month, from `1`.
    pub fn day(self) -> u32 {
        u32::from(self.day)
    }

    pub fn weekday(self) -> Weekday {
        // 1970-01-01 was a Thursday.
        Weekday::from_index((self.days().rem_euclid(7) + 3) as u32)
    }

    /// The date `n` days on, or back when negative.
    pub fn add_days(self, n: i32) -> Date {
        Date::from_days(self.days().saturating_add(n))
    }

    /// The same day `n` months on, or back, held to the last day of a
    /// shorter month: a month after 31 January is 28 or 29 February.
    pub fn add_months(self, n: i32) -> Date {
        let total = i64::from(self.year) * 12 + i64::from(self.month) - 1 + i64::from(n);
        let year = total
            .div_euclid(12)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
        let month = total.rem_euclid(12) as u32 + 1;
        let day = self.day().min(days_in_month(year, month));
        Date {
            year,
            month: month as u8,
            day: day as u8,
        }
    }

    /// The same day `n` years on, or back; 29 February falls to the 28th.
    pub fn add_years(self, n: i32) -> Date {
        self.add_months(n.saturating_mul(12))
    }

    /// The first of its month.
    pub fn first_of_month(self) -> Date {
        Date { day: 1, ..self }
    }

    /// Its month as one count, `year * 12 + month - 1`.
    pub fn month_index(self) -> i32 {
        self.year
            .saturating_mul(12)
            .saturating_add(i32::from(self.month) - 1)
    }

    /// The first of the month counted `index` ([`month_index`](Self::month_index)).
    pub fn of_month_index(index: i32) -> Date {
        Date {
            year: index.div_euclid(12),
            month: (index.rem_euclid(12) + 1) as u8,
            day: 1,
        }
    }

    /// Its ISO 8601 week number, `1` to `53`.
    pub fn iso_week(self) -> u32 {
        let thursday = self.add_days(3 - self.weekday().index() as i32);
        let jan1 = Date {
            year: thursday.year,
            month: 1,
            day: 1,
        };
        ((thursday.days() - jan1.days()) / 7 + 1) as u32
    }

    /// The date `text` writes as `YYYY-MM-DD`.
    pub fn parse_iso(text: &str) -> Option<Date> {
        let text = text.trim();
        let (body, sign) = match text.strip_prefix('-') {
            Some(rest) => (rest, -1),
            None => (text, 1),
        };
        let mut parts = body.split('-');
        let year: i32 = parts.next()?.parse().ok()?;
        let month: u32 = parts.next()?.parse().ok()?;
        let day: u32 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Date::new(year * sign, month, day)
    }

    /// Today, by the system clock in universal time.
    pub fn today() -> Date {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        Date::from_days((secs / 86_400).min(i32::MAX as u64) as i32)
    }

    /// It as a cell holds it.
    pub fn to_state(self) -> StateValue {
        StateValue::Int(self.days())
    }

    /// The date a cell holds, `None` for [`NO_DATE`] or any other value.
    pub fn of_state(value: Option<StateValue>) -> Option<Date> {
        match value {
            Some(StateValue::Int(days)) if days != NO_DATE => Some(Date::from_days(days)),
            _ => None,
        }
    }

    /// The first day of the six weeks a month grid starting its weeks on
    /// `first` shows for its month.
    pub fn grid_start(self, first: Weekday) -> Date {
        let start = self.first_of_month();
        let back = (start.weekday().index() + 7 - first.index()) % 7;
        start.add_days(-(back as i32))
    }
}

/// `YYYY-MM-DD`.
impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// How many days `month` (`1` to `12`) of `year` has.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Weekday {
    pub const ALL: [Weekday; 7] = [
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
    ];

    /// Its place from Monday, `0` to `6`.
    pub fn index(self) -> u32 {
        self as u32
    }

    /// The day `index` places from Monday, round the week.
    pub fn from_index(index: u32) -> Weekday {
        Weekday::ALL[(index % 7) as usize]
    }

    /// The day `n` days on.
    pub fn after(self, n: u32) -> Weekday {
        Weekday::from_index(self.index() + n % 7)
    }

    fn of_keyword(key: &str) -> Option<Weekday> {
        Some(match key {
            "mon" => Weekday::Monday,
            "tue" => Weekday::Tuesday,
            "wed" => Weekday::Wednesday,
            "thu" => Weekday::Thursday,
            "fri" => Weekday::Friday,
            "sat" => Weekday::Saturday,
            "sun" => Weekday::Sunday,
            _ => return None,
        })
    }
}

/// Whether a clock runs to twelve, twice a day, or to twenty-four.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HourCycle {
    H12,
    H24,
}

/// The regions whose weeks start on Sunday, Saturday and Friday; every
/// other's starts on Monday (CLDR week data).
const SUNDAY: &[&str] = &[
    "AG", "AS", "BD", "BR", "BS", "BT", "BW", "BZ", "CA", "CN", "CO", "DM", "DO", "ET", "GT", "GU",
    "HK", "HN", "ID", "IL", "IN", "JM", "JP", "KE", "KH", "KR", "LA", "MH", "MM", "MO", "MT", "MX",
    "MZ", "NI", "NP", "PA", "PE", "PH", "PK", "PR", "PT", "PY", "SA", "SG", "SV", "TH", "TT", "TW",
    "UM", "US", "VE", "VI", "WS", "YE", "ZA", "ZW",
];
const SATURDAY: &[&str] = &[
    "AE", "AF", "BH", "DJ", "DZ", "EG", "IQ", "IR", "JO", "KW", "LY", "OM", "QA", "SD", "SY",
];
const FRIDAY: &[&str] = &["MV"];

/// The regions whose clocks run to twelve (CLDR time data).
const TWELVE: &[&str] = &[
    "AE", "AS", "AU", "BD", "BH", "BN", "BT", "CA", "CO", "DJ", "DO", "DZ", "EG", "ER", "ET", "FJ",
    "FM", "GH", "GU", "HK", "HN", "IN", "IQ", "JO", "KI", "KR", "KW", "LB", "LY", "MH", "MO", "MP",
    "MW", "MY", "NI", "NP", "NZ", "OM", "PA", "PG", "PH", "PK", "PR", "PW", "QA", "SA", "SB", "SD",
    "SO", "SS", "SV", "SY", "TO", "TW", "UM", "US", "VE", "VI", "VU", "WS", "YE", "ZM",
];

/// The region a language is most spoken in, for a tag naming none: the
/// languages whose region keeps another week or clock than most.
const LIKELY: &[(&str, &str)] = &[
    ("am", "ET"),
    ("ar", "EG"),
    ("bn", "BD"),
    ("dv", "MV"),
    ("en", "US"),
    ("fa", "IR"),
    ("fil", "PH"),
    ("gu", "IN"),
    ("he", "IL"),
    ("hi", "IN"),
    ("id", "ID"),
    ("iw", "IL"),
    ("ja", "JP"),
    ("km", "KH"),
    ("kn", "IN"),
    ("ko", "KR"),
    ("lo", "LA"),
    ("ml", "IN"),
    ("mr", "IN"),
    ("ms", "MY"),
    ("my", "MM"),
    ("ne", "NP"),
    ("pa", "IN"),
    ("ps", "AF"),
    ("pt", "BR"),
    ("ta", "IN"),
    ("te", "IN"),
    ("th", "TH"),
    ("tl", "PH"),
    ("ur", "PK"),
    ("zh", "CN"),
];

/// What a BCP 47 tag says: its region, or its language's, and the value of
/// its Unicode extension keyword `key`.
fn read_tag<'a>(locale: &'a str, key: &str) -> (Option<String>, Option<&'a str>) {
    let mut parts = locale.split(['-', '_']);
    let language = parts.next().unwrap_or("").to_ascii_lowercase();
    let mut region = None;
    let mut keyword = None;
    let mut in_unicode = false;
    let mut want = false;
    for part in parts {
        if in_unicode {
            if want {
                keyword = Some(part);
                want = false;
            } else if part.len() == 1 {
                in_unicode = part.eq_ignore_ascii_case("u");
            } else {
                want = part.eq_ignore_ascii_case(key);
            }
            continue;
        }
        if part.len() == 1 {
            in_unicode = part.eq_ignore_ascii_case("u");
            continue;
        }
        let is_region = (part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()))
            || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()));
        if region.is_none() && is_region {
            region = Some(part.to_ascii_uppercase());
        }
    }
    let region = region.or_else(|| {
        LIKELY
            .iter()
            .find(|(l, _)| *l == language)
            .map(|(_, r)| (*r).to_owned())
    });
    (region, keyword)
}

/// The day the weeks of `locale` start on.
pub fn first_weekday(locale: &str) -> Weekday {
    let (region, fw) = read_tag(locale, "fw");
    if let Some(day) = fw.and_then(|k| Weekday::of_keyword(&k.to_ascii_lowercase())) {
        return day;
    }
    match region.as_deref() {
        Some(r) if SUNDAY.contains(&r) => Weekday::Sunday,
        Some(r) if SATURDAY.contains(&r) => Weekday::Saturday,
        Some(r) if FRIDAY.contains(&r) => Weekday::Friday,
        _ => Weekday::Monday,
    }
}

/// Whether the clock of `locale` runs to twelve or to twenty-four.
pub fn hour_cycle(locale: &str) -> HourCycle {
    let (region, hc) = read_tag(locale, "hc");
    match hc.map(|k| k.to_ascii_lowercase()).as_deref() {
        Some("h11" | "h12") => return HourCycle::H12,
        Some("h23" | "h24") => return HourCycle::H24,
        _ => {}
    }
    match region.as_deref() {
        Some(r) if TWELVE.contains(&r) => HourCycle::H12,
        _ => HourCycle::H24,
    }
}

/// What a week row draws under its days.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Days {
    /// How many of the row's first children are not days: a week number.
    pub skip: u8,
    /// The day, of the row's days, that is selected.
    pub selected: Option<u8>,
    /// The day that is today.
    pub today: Option<u8>,
    /// The day the pointer is over.
    pub hover: Option<u8>,
    /// The selected day's disc and today's ring.
    pub accent: Rgba,
    /// The disc under the day the pointer is over.
    pub hover_fill: Rgba,
}

impl Days {
    /// A row of days marking none, its marks in `accent` and `hover_fill`.
    pub fn new(skip: u8, accent: Rgba, hover_fill: Rgba) -> Days {
        Days {
            skip,
            selected: None,
            today: None,
            hover: None,
            accent,
            hover_fill,
        }
    }

    /// Draws the marks under the days of row `node`, at `world`.
    pub(crate) fn paint(
        &self,
        store: &NodeStore,
        node: NodeId,
        world: Rect,
        out: &mut Vec<Primitive>,
    ) {
        let origin = store.bounds(node);
        let day = |i: u8| {
            let child = store
                .arena()
                .children(node)
                .nth(usize::from(self.skip) + usize::from(i))?;
            let b = store.bounds(child);
            let side = (b.w.min(b.h) - 2.0).max(0.0);
            let (cx, cy) = (
                b.x - origin.x + world.x + b.w * 0.5,
                b.y - origin.y + world.y + b.h * 0.5,
            );
            Some(Rect::new(cx - side * 0.5, cy - side * 0.5, side, side))
        };
        let disc = |rect: Rect, color: Rgba, border: Border| {
            Primitive::AnalyticEllipse(AnalyticEllipse {
                rect,
                color,
                border,
            })
        };
        if let Some(rect) = self
            .hover
            .filter(|h| Some(*h) != self.selected)
            .and_then(day)
        {
            out.push(disc(rect, self.hover_fill, Border::NONE));
        }
        if let Some(rect) = self
            .today
            .filter(|t| Some(*t) != self.selected)
            .and_then(day)
        {
            out.push(disc(
                rect,
                Rgba::TRANSPARENT,
                Border {
                    width: 1.5,
                    color: self.accent,
                },
            ));
        }
        if let Some(rect) = self.selected.and_then(day) {
            out.push(disc(rect, self.accent, Border::NONE));
        }
    }
}

impl NodeStore {
    /// Node `id`'s week of days, if it draws one.
    pub fn days(&self, id: NodeId) -> Option<&Days> {
        match self.content_payload(id) {
            Some(Content::Days(days)) => Some(days),
            _ => None,
        }
    }

    /// Make node `id` draw `days` under its days; one drawing days already
    /// keeps the day the pointer is over.
    pub fn set_days(&mut self, id: NodeId, days: Days) {
        if !self.arena().is_live(id) {
            return;
        }
        let next = match self.days(id) {
            Some(was) => Days {
                hover: was.hover,
                ..days
            },
            None => days,
        };
        if self.days(id) == Some(&next) {
            return;
        }
        self.set_content_payload(id, Content::Days(next));
        self.mark_dirty(id, DirtyClass::PAINT);
    }

    /// Mark day `hover` of row `id` as the one the pointer is over, or none.
    pub(crate) fn hover_day(&mut self, id: NodeId, hover: Option<u8>) {
        if !self.arena().is_live(id) {
            return;
        }
        if let Some(Content::Days(days)) = self.content_payload_mut(id)
            && days.hover != hover
        {
            days.hover = hover;
            self.mark_dirty(id, DirtyClass::PAINT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u32, d: u32) -> Date {
        Date::new(y, m, d).unwrap()
    }

    #[test]
    fn dates_count_days_both_ways_across_the_calendar() {
        assert_eq!(date(1970, 1, 1).days(), 0);
        assert_eq!(date(2000, 3, 1).days(), 11_017);
        assert_eq!(date(1969, 12, 31).days(), -1);
        for days in [-800_000, -1, 0, 59, 60, 11_016, 19_000, 2_932_896] {
            assert_eq!(Date::from_days(days).days(), days);
        }
        assert_eq!(Date::from_days(19_655), date(2023, 10, 25));
        assert_eq!(date(2026, 10, 24).weekday(), Weekday::Saturday);
        assert_eq!(date(1970, 1, 1).weekday(), Weekday::Thursday);
        assert_eq!(Date::new(2023, 2, 29), None);
        assert!(Date::new(2024, 2, 29).is_some());
        assert_eq!(Date::new(1900, 2, 29), None, "a century is not leap");
        assert!(Date::new(2000, 2, 29).is_some(), "but every fourth is");
    }

    #[test]
    fn months_step_held_to_their_ends_and_weeks_number_by_iso() {
        assert_eq!(date(2024, 1, 31).add_months(1), date(2024, 2, 29));
        assert_eq!(date(2023, 1, 31).add_months(1), date(2023, 2, 28));
        assert_eq!(date(2024, 3, 15).add_months(-3), date(2023, 12, 15));
        assert_eq!(date(2024, 2, 29).add_years(1), date(2025, 2, 28));
        assert_eq!(date(2026, 10, 24).month_index(), 2026 * 12 + 9);
        assert_eq!(Date::of_month_index(2026 * 12 + 9), date(2026, 10, 1));
        assert_eq!(
            date(2021, 1, 3).iso_week(),
            53,
            "early January in last year's week"
        );
        assert_eq!(date(2021, 1, 4).iso_week(), 1);
        assert_eq!(date(2026, 12, 31).iso_week(), 53);
        assert_eq!(date(2026, 10, 24).to_string(), "2026-10-24");
        assert_eq!(Date::parse_iso(" 2026-10-24 "), Some(date(2026, 10, 24)));
        assert_eq!(Date::parse_iso("2026-02-30"), None);
        assert_eq!(Date::of_state(Some(StateValue::Int(NO_DATE))), None);
        assert_eq!(
            Date::of_state(Some(date(2026, 1, 2).to_state())),
            Some(date(2026, 1, 2))
        );
    }

    #[test]
    fn a_month_grid_starts_on_its_weeks_first_day() {
        // October 2026 begins on a Thursday.
        let oct = date(2026, 10, 24);
        assert_eq!(oct.grid_start(Weekday::Monday), date(2026, 9, 28));
        assert_eq!(oct.grid_start(Weekday::Sunday), date(2026, 9, 27));
        assert_eq!(oct.grid_start(Weekday::Thursday), date(2026, 10, 1));
        assert_eq!(oct.grid_start(Weekday::Friday), date(2026, 9, 25));
    }

    #[test]
    fn a_locale_keeps_its_regions_week_and_clock() {
        assert_eq!(first_weekday("en-US"), Weekday::Sunday);
        assert_eq!(first_weekday("en"), Weekday::Sunday, "English as in the US");
        assert_eq!(first_weekday("en-GB"), Weekday::Monday);
        assert_eq!(first_weekday("de"), Weekday::Monday);
        assert_eq!(first_weekday("ar-EG"), Weekday::Saturday);
        assert_eq!(first_weekday("dv"), Weekday::Friday);
        assert_eq!(first_weekday("zh-Hant-TW"), Weekday::Sunday);
        assert_eq!(first_weekday("en_US"), Weekday::Sunday);
        assert_eq!(
            first_weekday("en-US-u-fw-mon"),
            Weekday::Monday,
            "the keyword wins"
        );
        assert_eq!(first_weekday("und"), Weekday::Monday);
        assert_eq!(hour_cycle("en-US"), HourCycle::H12);
        assert_eq!(hour_cycle("en-GB"), HourCycle::H24);
        assert_eq!(hour_cycle("ko"), HourCycle::H12);
        assert_eq!(hour_cycle("ja-JP"), HourCycle::H24);
        assert_eq!(hour_cycle("en-US-u-hc-h23"), HourCycle::H24);
        assert_eq!(hour_cycle("fr-u-hc-h12"), HourCycle::H12);
    }
}
