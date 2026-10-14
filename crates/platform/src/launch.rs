//! The variables an app was launched with, by name: what the launcher
//! passes an app the way the platform passes it.
//!
//! - The desktop and iOS: the process environment (`simctl launch` sets the
//!   app's environment on the simulator).
//! - Android: the string extras of the intent that started the activity.
//! - The web: the members of the page's `visoLaunch` object, which the page
//!   sets before the app loads.

/// The launch variable `name`, if the app was launched with it.
pub fn launch_var(name: &str) -> Option<String> {
    #[cfg(target_os = "android")]
    {
        crate::backend::android::launch_var(name)
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        crate::backend::web::launch_var(name)
    }
    #[cfg(not(any(
        target_os = "android",
        all(target_arch = "wasm32", target_os = "unknown")
    )))]
    {
        std::env::var(name).ok()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_desktop_launch_variable_is_the_environment() {
        assert_eq!(super::launch_var("VISO_NO_SUCH_LAUNCH_VARIABLE"), None);
        assert_eq!(
            super::launch_var("PATH"),
            std::env::var("PATH").ok(),
            "the process environment"
        );
    }
}
