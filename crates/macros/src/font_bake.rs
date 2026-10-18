//! Build-time glyph baking for embedded targets (Text/Font Runtime §3.6).
//!
//! `viso build` for an embedded target whose `Viso.toml` declares the font
//! sizes its UI draws at sets two more variables for the compiler:
//!
//! - `VISO_FONT_BAKE`: the sizes, in pixels per em as the panel draws them,
//!   comma-separated; each packaged face's drawable glyphs are rasterized at
//!   each and embedded as compressed A8 pages (`viso_glyph_a8::baked`), which
//!   the runtime reads before it would rasterize;
//! - `VISO_FONT_RASTERIZER`: `off` when the target links no rasterizer. Every
//!   face must then bake, and its outline tables are dropped from the
//!   embedded font, which keeps what shaping reads.
//!
//! The bake set is exact without shaping anything at build time: a subset's
//! drawable glyphs are the closure the subsetter computes (the kept
//! characters' glyphs and everything `GSUB` reaches from them), so whatever
//! shaping the runtime does over the kept characters draws a baked glyph.

pub(crate) const BAKE_VAR: &str = "VISO_FONT_BAKE";
pub(crate) const RASTERIZER_VAR: &str = "VISO_FONT_RASTERIZER";

/// The largest size a build bakes.
const MAX_SIZE: u16 = 1024;

/// What a build asks the packaged faces to bake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Bake {
    /// Pixels per em, ascending, each once; empty when nothing is baked.
    pub sizes: Vec<u16>,
    /// Whether the build rasterizes at run time; `false` drops the outlines.
    pub rasterizer: bool,
}

impl Bake {
    /// The bake the build environment asks for.
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| match std::env::var(name) {
            Ok(value) if value.trim().is_empty() => Ok(None),
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} is not UTF-8")),
        };
        Self::new(var(BAKE_VAR)?.as_deref(), var(RASTERIZER_VAR)?.as_deref())
    }

    pub fn new(sizes: Option<&str>, rasterizer: Option<&str>) -> Result<Self, String> {
        let mut parsed = Vec::new();
        for item in sizes.unwrap_or_default().split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            match item.parse::<u16>() {
                Ok(size @ 1..=MAX_SIZE) => parsed.push(size),
                _ => {
                    return Err(format!(
                        "{BAKE_VAR}: `{item}` is not a size from 1 to {MAX_SIZE} pixels per em"
                    ));
                }
            }
        }
        parsed.sort_unstable();
        parsed.dedup();
        let rasterizer = match rasterizer.map(str::trim) {
            None | Some("on") => true,
            Some("off") => false,
            Some(other) => {
                return Err(format!(
                    "{RASTERIZER_VAR}: `{other}` is neither `on` nor `off`"
                ));
            }
        };
        if !rasterizer && parsed.is_empty() {
            return Err(format!(
                "{RASTERIZER_VAR}=off draws every glyph from baked pages, but {BAKE_VAR} \
                 names no size to bake"
            ));
        }
        Ok(Self {
            sizes: parsed,
            rasterizer,
        })
    }

    /// Whether anything is baked.
    pub fn is_on(&self) -> bool {
        !self.sizes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_sorted_once_each_and_the_rasterizer_stays_by_default() {
        let bake = Bake::new(Some("24, 14,,16,14"), None).unwrap();
        assert_eq!(bake.sizes, [14, 16, 24]);
        assert!(bake.rasterizer && bake.is_on());
        let none = Bake::new(None, Some("on")).unwrap();
        assert!(!none.is_on() && none.rasterizer);
    }

    #[test]
    fn a_bad_size_or_switch_is_an_error_naming_it() {
        let error = Bake::new(Some("14,0"), None).unwrap_err();
        assert!(error.contains("`0`") && error.contains(BAKE_VAR), "{error}");
        let error = Bake::new(Some("14.5"), None).unwrap_err();
        assert!(error.contains("`14.5`"), "{error}");
        let error = Bake::new(Some("14"), Some("no")).unwrap_err();
        assert!(
            error.contains("`no`") && error.contains(RASTERIZER_VAR),
            "{error}"
        );
    }

    #[test]
    fn no_rasterizer_needs_a_size_to_bake() {
        let error = Bake::new(None, Some("off")).unwrap_err();
        assert!(error.contains(BAKE_VAR), "{error}");
        assert!(!Bake::new(Some("16"), Some("off")).unwrap().rasterizer);
    }
}
