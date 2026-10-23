//! Open a URL with the app its scheme is registered to, through the shell.

use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

use super::owner;
use crate::launcher::{Launcher, launch};
use crate::reply::{Reply, ServiceError};

/// `ShellExecute`'s answer when no app is registered for the URL's scheme.
const SE_ERR_NOASSOC: isize = 31;

pub(super) struct ShellOpen;

impl Launcher for ShellOpen {
    fn open_url(&self, url: &str) -> Reply<()> {
        launch(url, |url| {
            let url = HSTRING::from(url);
            // SAFETY: the verb and the URL are nul-ended wide strings that
            // outlive the call; the owner is a window handle or null. Only
            // an absolute URL of a scheme that hands it to an app reaches
            // here, never a path the shell would run.
            let done = unsafe {
                ShellExecuteW(
                    Some(owner()),
                    w!("open"),
                    &url,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    SW_SHOWNORMAL,
                )
            };
            // Above 32 is success; below, an error code.
            Reply::ready(match done.0 as isize {
                code if code > 32 => Ok(()),
                SE_ERR_NOASSOC => Err(ServiceError::Unsupported),
                code => Err(ServiceError::Failed(format!(
                    "the shell could not open the URL ({code})"
                ))),
            })
        })
    }
}
