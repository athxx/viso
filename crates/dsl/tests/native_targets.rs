//! A native limited to some platforms: a call to it compiled for another is
//! `E6104` at the call, naming where it exists; for one of its platforms, or
//! with no platform to check, it compiles.

use std::sync::Arc;

use viso_behavior::native::{NativeLibrary, Natives, STANDARD};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;

static HAPTICS: NativeLibrary = NativeLibrary {
    path: "haptics",
    version: 1,
    functions: &[
        viso_behavior::native!(action "vibrate" |_cx, ms: i64| -> () {
            let _ = ms;
            Ok(())
        })
        .only_on(&["ios", "android"]),
        viso_behavior::native!(fn "strength" |_cx| -> i64 { Ok(3) }),
    ],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[],
};

const BELL: &str = r#"
import haptics;

export component Bell {
    state n = 0;
    action ring() {
        n = haptics::strength();
        haptics::vibrate(30);
    }
    view { Text("ring") { on click { ring(); } } }
}
"#;

fn errors(platform: Option<&'static str>) -> Vec<(String, String, Vec<String>)> {
    let mut natives = Natives::new();
    natives.extend(STANDARD).expect("standard");
    natives.register(&HAPTICS).expect("haptics");
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        ..Default::default()
    };
    let profile = TargetProfile {
        platform,
        ..TargetProfile::default()
    };
    compile_file_for(BELL, &origin, Arc::new(natives), profile)
        .errors()
        .map(|d| {
            let at = d.primary.start().to_u32() as usize..d.primary.end().to_u32() as usize;
            let at = BELL[at].to_owned();
            (d.code.to_string(), at, d.notes.clone())
        })
        .collect()
}

#[test]
fn a_call_to_a_native_missing_on_the_target_is_an_error_at_the_call() {
    let found = errors(Some("web"));
    assert_eq!(found.len(), 1, "{found:#?}");
    let (code, at, notes) = &found[0];
    assert_eq!(code, "E6104");
    assert!(at.starts_with("haptics::vibrate"), "{at}");
    assert!(notes[0].contains("ios, android"), "{notes:?}");
}

#[test]
fn it_compiles_for_its_platforms_and_without_one() {
    for platform in [Some("ios"), Some("android"), None] {
        assert_eq!(errors(platform), [], "{platform:?}");
    }
}
