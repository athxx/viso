//! What a plate shows: a badge's count, an avatar's initials and the color
//! a name gives its avatar — the one meaning the runtime draws and the web
//! export writes.

use alloc::format;
use alloc::string::{String, ToString};

/// The fills an avatar takes from its name, as sRGB `0xrrggbb`: deep enough
/// under white initials.
pub const AVATAR_FILLS: [u32; 10] = [
    0xc62828, 0xad1457, 0x6a1b9a, 0x283593, 0x1565c0, 0x00695c, 0x2e7d32, 0xd84315, 0x4e342e,
    0x37474f,
];

/// What a badge counting `count` shows: the count, or `max` and a plus past
/// it; a negative count shows as none, and a `max` below one as one.
pub fn badge_count(count: i64, max: i64) -> String {
    let count = count.max(0);
    let max = max.max(1);
    if count > max {
        format!("{max}+")
    } else {
        count.to_string()
    }
}

/// The initials of `name`: the first letters of its first and last words,
/// capitalized; a name whose first letter has no case (an ideograph, a
/// syllable of a script without capitals) shows that letter alone. A name
/// with no letter or digit has none.
pub fn initials(name: &str) -> String {
    let lead = |word: &str| word.chars().find(|c| c.is_alphanumeric());
    let mut words = name.split_whitespace().filter(|w| lead(w).is_some());
    let Some(first) = words.next().and_then(lead) else {
        return String::new();
    };
    let mut out: String = first.to_uppercase().collect();
    let cased = first.is_lowercase() || first.is_uppercase();
    if cased && let Some(last) = words.next_back().and_then(lead) {
        out.extend(last.to_uppercase());
    }
    out
}

/// The fill an avatar of `name` takes, as sRGB `0xrrggbb`: the same for the
/// same name, the FNV-1a hash of its trimmed UTF-8 picking among
/// [`AVATAR_FILLS`].
pub fn avatar_rgb(name: &str) -> u32 {
    let hash = name.trim().bytes().fold(0x811c_9dc5_u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    AVATAR_FILLS[hash as usize % AVATAR_FILLS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_past_its_most_shows_the_most_and_a_plus() {
        assert_eq!(badge_count(7, 99), "7");
        assert_eq!(badge_count(99, 99), "99");
        assert_eq!(badge_count(100, 99), "99+");
        assert_eq!(badge_count(-3, 99), "0");
    }

    #[test]
    fn initials_take_the_first_and_last_words_or_one_uncased_letter() {
        assert_eq!(initials("ada lovelace"), "AL");
        assert_eq!(initials("Grace Brewster Murray Hopper"), "GH");
        assert_eq!(initials("  cher "), "C");
        assert_eq!(initials("张 三"), "张");
        assert_eq!(initials("(Ann) Lee"), "AL");
        assert_eq!(initials("— —"), "");
    }

    #[test]
    fn a_name_always_gives_its_avatar_the_same_fill() {
        assert_eq!(avatar_rgb("Ada"), avatar_rgb(" Ada "));
        // Pinned: the web export's script hashes the same way.
        assert_eq!(avatar_rgb("Ada Lovelace"), AVATAR_FILLS[0xfe5e_7cfe % 10]);
    }
}
