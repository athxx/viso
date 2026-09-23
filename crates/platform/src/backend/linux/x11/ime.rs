//! Input methods through XIM. Key presses go to the input method server,
//! which answers with committed text, preedit updates, or the key itself
//! when it does not want it. One input context per window, created once the
//! server has told us which input styles it supports.

use std::collections::VecDeque;
use std::rc::Rc;

use x11rb::protocol::Event;
use x11rb::protocol::xproto::{KeyPressEvent, Window};
use x11rb::xcb_ffi::XCBConnection;
use xim::{
    AHashMap, AttributeName, Client, ClientError, ClientHandler, ForwardEventFlag, InputStyle,
    InputStyleList, Point, PreeditDrawStatus, x11rb::X11rbClient,
};

type XimClient = X11rbClient<Rc<XCBConnection>>;

/// What the input method asked the app to do.
#[derive(Debug)]
pub(super) enum ImeOut {
    Commit {
        window: Window,
        text: String,
    },
    /// The server did not want this key: handle it as a plain key event.
    Forward {
        window: Window,
        event: KeyPressEvent,
    },
    /// The composition changed; the caret is in characters.
    Preedit {
        window: Window,
        text: String,
        caret: usize,
    },
}

pub(super) struct Ime {
    client: XimClient,
    state: ImeState,
}

#[derive(Default)]
struct ImeState {
    im: Option<u16>,
    style: Option<InputStyle>,
    /// Windows that want an input context, in creation order.
    windows: Vec<Window>,
    /// Windows whose `CreateIc` is in flight, answered in order.
    creating: VecDeque<Window>,
    contexts: Vec<Context>,
    focused: Option<Window>,
    out: Vec<ImeOut>,
    lost: bool,
}

struct Context {
    window: Window,
    ic: u16,
    preedit: String,
    /// The spot (physical pixels, window-relative) the server places its
    /// candidate window at; `None` while no text field has focus.
    spot: Option<(i16, i16)>,
}

impl Ime {
    /// Connect to the server `XMODIFIERS` names; `None` without one.
    pub(super) fn connect(conn: Rc<XCBConnection>, screen: usize) -> Option<Self> {
        let client = X11rbClient::init(conn, screen, None).ok()?;
        Some(Self {
            client,
            state: ImeState::default(),
        })
    }

    /// Offer `event` to the XIM transport; `true` when it was XIM traffic.
    /// Afterwards [`take`](Self::take) holds what the server asked for.
    pub(super) fn filter(&mut self, event: &Event) -> bool {
        match self.client.filter_event(event, &mut self.state) {
            Ok(consumed) => consumed,
            Err(_) => {
                self.state.lost = true;
                false
            }
        }
    }

    /// The server went away; the caller falls back to local composition.
    pub(super) fn is_lost(&self) -> bool {
        self.state.lost
    }

    /// Ready to take keys for `window`.
    pub(super) fn active_for(&self, window: Window) -> bool {
        !self.state.lost && self.state.context(window).is_some()
    }

    pub(super) fn take(&mut self) -> Vec<ImeOut> {
        std::mem::take(&mut self.state.out)
    }

    pub(super) fn add_window(&mut self, window: Window) {
        self.state.windows.push(window);
        if let Some(im) = self.state.im
            && self.state.style.is_some()
        {
            let _ = self.state.create_ic(&mut self.client, im, window);
        }
    }

    pub(super) fn remove_window(&mut self, window: Window) {
        self.state.windows.retain(|w| *w != window);
        if let Some(i) = self.state.contexts.iter().position(|c| c.window == window)
            && let Some(im) = self.state.im
        {
            let ic = self.state.contexts.swap_remove(i).ic;
            let _ = self.client.destroy_ic(im, ic);
        }
    }

    /// Hand a key press or release to the server.
    pub(super) fn forward(&mut self, window: Window, event: &KeyPressEvent) -> bool {
        let (Some(im), Some(ctx)) = (self.state.im, self.state.context(window)) else {
            return false;
        };
        let ic = ctx.ic;
        self.client
            .forward_event(im, ic, ForwardEventFlag::empty(), event)
            .is_ok()
    }

    pub(super) fn focus(&mut self, window: Window, focused: bool) {
        self.state.focused = if focused {
            Some(window)
        } else {
            self.state.focused.filter(|w| *w != window)
        };
        self.state.sync_focus(&mut self.client, window);
    }

    /// Place the candidate window at `spot`, or disable composition for
    /// `window` with `None`.
    pub(super) fn set_spot(&mut self, window: Window, spot: Option<(i16, i16)>) {
        let Some(im) = self.state.im else { return };
        let Some(ctx) = self.state.context_mut(window) else {
            return;
        };
        let was = ctx.spot;
        ctx.spot = spot;
        let ic = ctx.ic;
        if let Some((x, y)) = spot {
            let attrs = self
                .client
                .build_ic_attributes()
                .nested_list(AttributeName::PreeditAttributes, |b| {
                    b.push(AttributeName::SpotLocation, Point { x, y });
                })
                .build();
            let _ = self.client.set_ic_values(im, ic, attrs);
        }
        if was.is_some() != spot.is_some() {
            self.state.sync_focus(&mut self.client, window);
        }
    }
}

impl ImeState {
    fn context(&self, window: Window) -> Option<&Context> {
        self.contexts.iter().find(|c| c.window == window)
    }

    fn context_mut(&mut self, window: Window) -> Option<&mut Context> {
        self.contexts.iter_mut().find(|c| c.window == window)
    }

    fn window_of(&self, ic: u16) -> Option<Window> {
        self.contexts.iter().find(|c| c.ic == ic).map(|c| c.window)
    }

    /// The server holds focus for a window's context while the window has
    /// keyboard focus and a text field wants input.
    fn sync_focus(&mut self, client: &mut XimClient, window: Window) {
        let Some(im) = self.im else { return };
        let focused = self.focused == Some(window);
        let Some(ctx) = self.context_mut(window) else {
            return;
        };
        let ic = ctx.ic;
        if focused && ctx.spot.is_some() {
            let _ = client.set_focus(im, ic);
        } else {
            let _ = client.unset_focus(im, ic);
            if !ctx.preedit.is_empty() {
                ctx.preedit.clear();
                self.out.push(ImeOut::Preedit {
                    window,
                    text: String::new(),
                    caret: 0,
                });
            }
        }
    }

    fn create_ic(
        &mut self,
        client: &mut XimClient,
        im: u16,
        window: Window,
    ) -> Result<(), ClientError> {
        let style = self
            .style
            .unwrap_or(InputStyle::PREEDIT_NOTHING | InputStyle::STATUS_NOTHING);
        let attrs = client
            .build_ic_attributes()
            .push(AttributeName::InputStyle, style)
            .push(AttributeName::ClientWindow, window)
            .push(AttributeName::FocusWindow, window)
            .nested_list(AttributeName::PreeditAttributes, |b| {
                b.push(AttributeName::SpotLocation, Point { x: 0, y: 0 });
            })
            .build();
        client.create_ic(im, attrs)?;
        self.creating.push_back(window);
        Ok(())
    }
}

/// The richest input style the server supports: preedit drawn by the app,
/// else by the server at the caret, else in the server's own window.
fn pick_style(supported: &[InputStyle]) -> InputStyle {
    let nothing = InputStyle::STATUS_NOTHING;
    [
        InputStyle::PREEDIT_CALLBACKS | nothing,
        InputStyle::PREEDIT_POSITION | nothing,
        InputStyle::PREEDIT_NOTHING | nothing,
    ]
    .into_iter()
    .find(|s| supported.contains(s))
    .unwrap_or(InputStyle::PREEDIT_NOTHING | nothing)
}

/// The locale to open the input method with: the environment's, without
/// its encoding suffix.
fn locale() -> String {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX")
        .and_then(|v| v.split(['.', '@']).next().map(str::to_owned))
        .unwrap_or_else(|| "en_US".to_owned())
}

impl ClientHandler<XimClient> for ImeState {
    fn handle_connect(&mut self, client: &mut XimClient) -> Result<(), ClientError> {
        client.open(&locale())
    }

    fn handle_disconnect(&mut self) {
        self.lost = true;
    }

    fn handle_open(&mut self, client: &mut XimClient, im: u16) -> Result<(), ClientError> {
        self.im = Some(im);
        client.get_im_values(im, &[AttributeName::QueryInputStyle])
    }

    fn handle_get_im_values(
        &mut self,
        client: &mut XimClient,
        im: u16,
        mut attributes: AHashMap<AttributeName, Vec<u8>>,
    ) -> Result<(), ClientError> {
        let supported = attributes
            .remove(&AttributeName::QueryInputStyle)
            .and_then(|v| xim::read::<InputStyleList>(&v).ok())
            .map(|l| l.styles)
            .unwrap_or_default();
        self.style = Some(pick_style(&supported));
        for window in self.windows.clone() {
            self.create_ic(client, im, window)?;
        }
        Ok(())
    }

    fn handle_create_ic(
        &mut self,
        client: &mut XimClient,
        _im: u16,
        ic: u16,
    ) -> Result<(), ClientError> {
        let Some(window) = self.creating.pop_front() else {
            return Ok(());
        };
        if !self.windows.contains(&window) {
            // Closed while the context was being created.
            if let Some(im) = self.im {
                client.destroy_ic(im, ic)?;
            }
            return Ok(());
        }
        self.contexts.push(Context {
            window,
            ic,
            preedit: String::new(),
            spot: None,
        });
        self.sync_focus(client, window);
        Ok(())
    }

    fn handle_commit(
        &mut self,
        _client: &mut XimClient,
        _im: u16,
        ic: u16,
        text: &str,
    ) -> Result<(), ClientError> {
        let Some(window) = self.window_of(ic) else {
            return Ok(());
        };
        if let Some(ctx) = self.context_mut(window)
            && !ctx.preedit.is_empty()
        {
            ctx.preedit.clear();
            self.out.push(ImeOut::Preedit {
                window,
                text: String::new(),
                caret: 0,
            });
        }
        self.out.push(ImeOut::Commit {
            window,
            text: text.to_owned(),
        });
        Ok(())
    }

    fn handle_forward_event(
        &mut self,
        _client: &mut XimClient,
        _im: u16,
        ic: u16,
        _flag: ForwardEventFlag,
        event: KeyPressEvent,
    ) -> Result<(), ClientError> {
        if let Some(window) = self.window_of(ic) {
            self.out.push(ImeOut::Forward { window, event });
        }
        Ok(())
    }

    fn handle_preedit_draw(
        &mut self,
        _client: &mut XimClient,
        _im: u16,
        ic: u16,
        caret: i32,
        chg_first: i32,
        chg_len: i32,
        status: PreeditDrawStatus,
        preedit_string: &str,
        _feedbacks: Vec<xim::Feedback>,
    ) -> Result<(), ClientError> {
        let Some(window) = self.window_of(ic) else {
            return Ok(());
        };
        let Some(ctx) = self.context_mut(window) else {
            return Ok(());
        };
        let inserted = if status.contains(PreeditDrawStatus::NO_STRING) {
            ""
        } else {
            preedit_string
        };
        ctx.preedit = splice_chars(&ctx.preedit, chg_first, chg_len, inserted);
        let len = ctx.preedit.chars().count();
        let caret = usize::try_from(caret).unwrap_or(0).min(len);
        let text = ctx.preedit.clone();
        self.out.push(ImeOut::Preedit {
            window,
            text,
            caret,
        });
        Ok(())
    }

    fn handle_preedit_done(
        &mut self,
        _client: &mut XimClient,
        _im: u16,
        ic: u16,
    ) -> Result<(), ClientError> {
        let Some(window) = self.window_of(ic) else {
            return Ok(());
        };
        if let Some(ctx) = self.context_mut(window)
            && !ctx.preedit.is_empty()
        {
            ctx.preedit.clear();
            self.out.push(ImeOut::Preedit {
                window,
                text: String::new(),
                caret: 0,
            });
        }
        Ok(())
    }
}

/// `text` with the `len` characters at `first` replaced by `insert`; out of
/// range positions clamp to the text.
fn splice_chars(text: &str, first: i32, len: i32, insert: &str) -> String {
    let count = text.chars().count();
    let first = usize::try_from(first).unwrap_or(0).min(count);
    let len = usize::try_from(len).unwrap_or(0).min(count - first);
    let mut out = String::with_capacity(text.len() + insert.len());
    out.extend(text.chars().take(first));
    out.push_str(insert);
    out.extend(text.chars().skip(first + len));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preedit_draws_replace_character_ranges() {
        assert_eq!(splice_chars("", 0, 0, "ni"), "ni");
        assert_eq!(splice_chars("ni", 0, 2, "你"), "你");
        assert_eq!(splice_chars("你好", 1, 1, "们"), "你们");
        assert_eq!(splice_chars("你好", 2, 5, "!"), "你好!");
        assert_eq!(splice_chars("abc", -1, 1, ""), "bc");
    }

    #[test]
    fn style_prefers_app_drawn_preedit() {
        let n = InputStyle::STATUS_NOTHING;
        assert_eq!(
            pick_style(&[
                InputStyle::PREEDIT_POSITION | n,
                InputStyle::PREEDIT_CALLBACKS | n
            ]),
            InputStyle::PREEDIT_CALLBACKS | n
        );
        assert_eq!(
            pick_style(&[InputStyle::PREEDIT_POSITION | n]),
            InputStyle::PREEDIT_POSITION | n
        );
        assert_eq!(pick_style(&[]), InputStyle::PREEDIT_NOTHING | n);
    }
}
