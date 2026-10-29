//! The frame phases the runtime drives the application through.

use std::cell::Cell;
use std::rc::Rc;

use viso_gpu::GpuBackend;
use viso_platform::{LogicalInsets, LogicalRect, RawWindowHandle, WindowId};
use viso_runtime::{FramePhase, RuntimeCx};
use viso_ui::context::UpdateCx;
use viso_ui::{
    BuildCx, DirtyCause, DirtyClass, ImeEvent, KeyEvent, KeyRouter, LinkPointer, Modifiers,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, ScrollEvent, ScrollRouter,
    focus_next, text_edit, virtual_list,
};
#[cfg(any(feature = "hot-reload", feature = "headless"))]
use viso_view::profile::Count;
use viso_view::profile::Span;

use super::access_keys::{access_key_of, alt_access};
use super::convert::{drag_out, lower_modifiers, outside_data, play};
use super::window_state::{
    drawn_menu_bar, native_handle, to_platform_config, wrap_root_in_safe_area,
    wrap_root_with_caption,
};
use super::{AppDriver, PANEL_ONLY, SessionFonts, WindowState};
use crate::{
    Application, Lifecycle, accessibility, audio, edit_menu, environment, footprint, persist,
    services, shell,
};

impl<A: Application> viso_runtime::FrameDriver for AppDriver<A> {
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
        shell::launch(cx);
        crate::paged::launch(cx.loop_waker());
        // Construct the user application now that the pump is live, then take the
        // faces it registered through `AppCx::load_font` so every window opened
        // this session loads them ahead of the system fallbacks.
        self.app = Some(A::new(&mut self.cx));
        self.accessibility = cx.capabilities().accessibility;
        self.fonts = SessionFonts {
            loaded: self.cx.__take_fonts(),
            packaged: self.cx.__packaged_fonts(),
        };
        #[cfg(feature = "hot-reload")]
        {
            self.hot_reload.set_dev_states(self.cx.__take_dev_states());
            self.hot_reload.set_packaged_fonts(self.fonts.packaged);
        }
        // Install the app's menu bar, if it declares one, before opening any
        // window. A `None` leaves the framework's standard app menu (with its
        // working Quit) in place from platform bring-up.
        if let Some(menu) = self.app.as_ref().and_then(Application::menu) {
            cx.set_menu(&menu);
            self.app_menu = Some(menu);
        }
        shell::apply(cx, &mut self.shell);
        // The OS services, created once on the loop thread now that the
        // platform is up; every window's handlers share them.
        if self.services.is_none() {
            let app = services::app_name();
            let system = services::Services::system(&app);
            self.services = Some(Rc::new(system));
            // The views' `@persist` states persist where the OS keeps the
            // app's data; a session handed its services (a headless test)
            // persists nothing unless it installs a store itself.
            persist::install(&app);
            // The games the views run play their audio on the default
            // output device. It links when a view mounts a game, and at once
            // in a development session, whose reloads may mount one.
            audio::allow();
            #[cfg(feature = "hot-reload")]
            audio::link();
        }
        // Open the initial window with the app's declared configuration
        // (`window_config`, defaulting to a self-drawn, captioned window). Later
        // phases let the app request further windows via `AppCx`.
        let cfg = self
            .app
            .as_ref()
            .map(Application::window_config)
            .unwrap_or_default();
        let mut config = to_platform_config(&cfg);
        let opening = self
            .places
            .open(cfg.persist.as_deref(), cx.framed_windows(), &mut config);
        #[cfg(feature = "hot-reload")]
        let config = crate::hot_reload::launch_window(config);
        let Ok(id) = cx.create_window(config) else {
            return;
        };
        if let Some(opening) = opening {
            self.places.opened(cx, id, opening);
        }
        // Open the launch window through the shared bring-up path, authoring its
        // tree with the application's own `build`, wrapped by default in a
        // self-drawn caption band (window-centered title, no authoring). A window
        // opened later through the `window()` seam runs the identical path with
        // the request's deferred build closure — the launch window is not a
        // special case.
        let ui_chrome = cfg.chrome;
        let title = cfg.title.clone();
        let framed = !PANEL_ONLY && cx.framed_windows();
        let caption = cfg.caption && framed;
        let bar = drawn_menu_bar(self.app_menu.as_ref(), &self.menu_picks, cx, id, caption);
        let app = self.app.as_mut();
        // Read the native traffic-light box the instant the window exists, so the
        // single build below sees it and a self-drawn caption yields to the OS
        // overlay instead of drawing its own buttons (chrome data contract).
        let chrome_geom = cx.window_chrome_geom(id);
        // The wrap captures the caption root into this cell during the synchronous
        // build inside `open`; read it back afterward to hold for fullscreen hide.
        let caption_cell = Cell::new(None);
        let services = self.services.clone();
        #[cfg(feature = "headless")]
        if self.harness.is_pending() {
            // The view mounts alone: no caption, no safe area, nothing of the
            // app's own view.
            let mut ws = WindowState::open(
                cx,
                id,
                &self.fonts,
                !self.gamepads.is_empty(),
                services,
                ui_chrome,
                chrome_geom,
                |_| None,
            );
            self.harness.mount(&mut ws);
            self.adopt_tape(&ws);
            self.windows.push(ws);
            return;
        }
        let mut ws = WindowState::open(
            cx,
            id,
            &self.fonts,
            !self.gamepads.is_empty(),
            services,
            ui_chrome,
            chrome_geom,
            |build| {
                let content = |build: &mut BuildCx<'_>| {
                    app.and_then(|app| {
                        app.build(build);
                        build.root()
                    })
                };
                if framed {
                    wrap_root_with_caption(build, &title, caption, &caption_cell, bar, content)
                } else {
                    wrap_root_in_safe_area(build, content)
                }
            },
        );
        ws.caption = caption_cell.get();
        if !framed {
            ws.safe_area_root = ws.root;
        }
        #[cfg(feature = "hot-reload")]
        self.hot_reload.adopt(|| cx.loop_waker(), &mut ws);
        #[cfg(feature = "headless")]
        self.harness.adopt(&mut ws);
        self.adopt_tape(&ws);
        self.windows.push(ws);
        // A link that came before the window opens now.
        self.open_pending_urls(cx);
    }

    fn on_geometry(&mut self, window: WindowId, scale: f64, width: u32, height: u32) {
        // Resize the named window's swapchain so its next frame maps pixel-space
        // to the new extent, and mark its root for relayout so the next
        // incremental frame re-places the tree against the new surface and
        // repaints. Record the new extent on `surface_size` regardless of GPU:
        // layout resolves against it, so a headless window still re-places the
        // tree on resize. An event for an unknown window (already closed) is a
        // no-op.
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            recorder.resize(window, scale as f32, (width, height));
        }
        let Some(ws) = self.window_mut(window) else {
            return;
        };
        let (w, h) = (width.max(1), height.max(1));
        ws.surface_size = (w, h);
        // Track the window's current density so text (re)declared after a
        // scale change rasterizes at the new density. A non-positive scale
        // (unreported) keeps the prior value rather than collapsing to zero.
        if scale > 0.0 {
            ws.dpi = scale as f32;
            ws.follow_scale_factor();
        }
        if let Some(gpu) = &mut ws.gpu {
            if let Some(surface) = gpu.surface {
                gpu.backend.resize_surface(surface, w, h);
            }
            // A resized swapchain holds no earlier frame to keep.
            gpu.renderer.damage_all();
            gpu.size = (w, h);
        }
        if let Some(root) = ws.root {
            let cause = ws.store.set_dirty_cause(DirtyCause::Window);
            ws.store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
            ws.store.set_dirty_cause(cause);
        }
        ws.report_window();
    }

    fn on_input(&mut self, sample: viso_runtime::InputSample, at: viso_runtime::Instant) {
        // The sample arrives in physical pixels (the scheduler resolved the
        // window scale), but layout and hit testing run in logical points, so
        // pointer/scroll positions are divided by the scale factor before they
        // reach the router — the same space as node bounds. Route it to the
        // target window's tree along the hit node's ancestry; any state a handler
        // writes lands in that window's pending set and is turned into targeted
        // dirtying by the next frame's flush (the scheduler already flagged the
        // frame input-dirty). A sample naming an unknown window (already closed)
        // is dropped.
        let _input = crate::profile::time(Span::Input);
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            recorder.input(&sample);
        }
        let target = sample.window();
        let Some(ws) = self.window_mut(target) else {
            return;
        };
        let Some(root) = ws.root else {
            return;
        };
        ws.store.set_input_time(at);
        let scale = ws.dpi.max(1.0);
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        let journaled = match ws.journal {
            Some(_) => {
                ws.route_chain.clear();
                crate::inspect::input_name(&sample, scale)
            }
            None => None,
        };
        let cause = ws.store.set_dirty_cause(DirtyCause::Input);
        // Samples that can edit the focused text control settle their edits
        // once routed; a pointer sample settles only when a handler placed a
        // caret, and scroll samples never record one.
        let edits_text = matches!(
            sample,
            viso_runtime::InputSample::Key(_)
                | viso_runtime::InputSample::Text(_)
                | viso_runtime::InputSample::Paste(_)
                | viso_runtime::InputSample::ImePreedit(_)
                | viso_runtime::InputSample::EditMenu(_)
        );
        match sample {
            viso_runtime::InputSample::Pointer(p) => {
                if p.phase == viso_runtime::PointerPhase::Down {
                    // A press while Alt is held makes it no menu-bar Alt.
                    ws.alt_alone = false;
                }
                let ev = PointerEvent {
                    x: p.x / scale,
                    y: p.y / scale,
                    phase: match p.phase {
                        viso_runtime::PointerPhase::Down => PointerPhase::Down,
                        viso_runtime::PointerPhase::Move => PointerPhase::Move,
                        viso_runtime::PointerPhase::Up => PointerPhase::Up,
                        // A cancelled contact must not activate anything: it
                        // reaches the tree as a leave, which clears press and
                        // hover without a click.
                        viso_runtime::PointerPhase::Leave | viso_runtime::PointerPhase::Cancel => {
                            PointerPhase::Leave
                        }
                    },
                    buttons: PointerButtons(p.buttons),
                    modifiers: Modifiers {
                        shift: p.modifiers.shift,
                        control: p.modifiers.control,
                        alt: p.modifiers.alt,
                        logo: p.modifiers.logo,
                    },
                };
                // Every pointer routes on its own; a finger or pen is a direct
                // contact (landing capture + pan arbitration). A cancelled
                // contact arrives as a leave, which ends it and its capture.
                let contact = viso_ui::PointerContact {
                    id: viso_ui::PointerId(p.pointer.0),
                    direct: p.kind != viso_runtime::PointerKind::Mouse,
                };
                environment::note_pointer(&mut ws.states, p.kind);
                // A press away from the drawn menu and its field closes it.
                if ev.phase == PointerPhase::Down
                    && ws.drawn_menu.shown().is_some()
                    && !ws.drawn_menu.holds(&ws.store, ev.x, ev.y)
                    && !ws.focused_text_node().is_some_and(|id| {
                        let r = ws.store.world(id);
                        r.x <= ev.x && ev.x < r.x + r.w && r.y <= ev.y && ev.y < r.y + r.h
                    })
                {
                    ws.close_drawn_menu();
                }
                // A press outside a menu, a list or a popover dismisses it
                // before it goes on to what it landed on.
                if ev.phase == PointerPhase::Down {
                    for layer in ws.store.dismissed_by_press(ev.x, ev.y) {
                        KeyRouter::route_dismiss(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            layer,
                        );
                    }
                }
                PointerRouter::route_contact(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    root,
                    contact,
                    ev,
                    &mut ws.route_chain,
                );
                let what = match ev.phase {
                    PointerPhase::Down if ev.buttons.contains(PointerButtons::PRIMARY) => {
                        Some(LinkPointer::Down)
                    }
                    PointerPhase::Down => None,
                    PointerPhase::Up => Some(LinkPointer::Up),
                    PointerPhase::Leave => Some(LinkPointer::Left),
                    _ => Some(LinkPointer::Moved),
                };
                if let Some(what) = what
                    && let Some((host, target)) =
                        ws.store.link_pointer(root, contact.id, what, ev.x, ev.y)
                {
                    KeyRouter::route_link(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        host,
                        &target,
                    );
                }
            }
            viso_runtime::InputSample::Key(k) => {
                // Lower the runtime-tier key sample onto the UI-tier event, then
                // route by focus (not hit test). Tab is a framework-level
                // focus-traversal command: on a Tab press we advance the focus
                // ring instead of routing the key to a handler (Shift-Tab goes
                // backward). Every other key routes to the focused node's
                // ancestry so a control can react to it.
                let modifiers = lower_modifiers(k.modifiers);
                environment::note_key(&mut ws.states);
                // Escape ends a pointer lock, as a browser's does, then goes
                // on to the focused node like any key.
                if k.pressed
                    && k.key == viso_runtime::Key::Escape
                    && ws.store.pointer_lock().is_some()
                {
                    KeyRouter::route_pointer_lock_end(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                    );
                }
                // Off Apple systems Alt shows the access keys' cues while it is
                // held, and let go alone asks for the menu bar.
                let access = alt_access() && ws.store.has_access_keys();
                let alt = matches!(
                    k.key,
                    viso_runtime::Key::AltLeft | viso_runtime::Key::AltRight
                );
                if access && alt {
                    let cues = k.pressed;
                    if k.pressed && !k.repeat {
                        ws.alt_alone = !(modifiers.control || modifiers.logo || modifiers.shift);
                    }
                    KeyRouter::route_cues(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        cues,
                    );
                    if !k.pressed && std::mem::take(&mut ws.alt_alone) {
                        KeyRouter::route_menu_bar(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                    }
                } else if k.pressed {
                    ws.alt_alone = false;
                }
                // Alt and a letter or digit, and F10, reach the access keys
                // and the menu bar when the focused node's handlers leave them.
                let access_key = access
                    .then(|| access_key_of(k.key, modifiers))
                    .flatten()
                    .filter(|_| k.pressed && !k.repeat);
                let menu_key = ws.drawn_menu.key(&mut ws.states, k.key, k.pressed);
                if menu_key == edit_menu::MenuKey::Close {
                    ws.close_drawn_menu();
                }
                if menu_key != edit_menu::MenuKey::Ignored {
                    // The drawn context menu takes the keys it answers.
                } else if matches!(
                    k.key,
                    viso_runtime::Key::Tab | viso_runtime::Key::Escape | viso_runtime::Key::Back
                ) && k.pressed
                {
                    // A handler may take Tab, Escape or the system back
                    // action first; else Tab moves the focus, Escape
                    // dismisses the innermost transient layer (a tooltip, a
                    // list the focus is not in), and back does so too or,
                    // with none, turns the deepest navigation stack back.
                    let ev = KeyEvent {
                        key: k.key,
                        pressed: k.pressed,
                        repeat: k.repeat,
                        modifiers,
                    };
                    let taken = KeyRouter::route_key_taken(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        root,
                        ev,
                        &mut ws.route_chain,
                    );
                    if taken || ws.store.carry_key(&ev) {
                        // The handler, or a gesture the keys carry on, did
                        // what the key does.
                    } else if matches!(k.key, viso_runtime::Key::Tab) {
                        focus_next(&mut ws.store, root, !modifiers.shift);
                    } else if let Some(layer) = ws.store.dismissed_by_escape() {
                        KeyRouter::route_dismiss(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            layer,
                        );
                    } else if matches!(k.key, viso_runtime::Key::Back)
                        && !k.repeat
                        && let Some(stack) = ws.store.back_target()
                    {
                        KeyRouter::route_back(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            root,
                            stack,
                            ev,
                            &mut ws.route_chain,
                        );
                    }
                } else if let Some((host, target)) = ws.store.link_key(
                    matches!(k.key, viso_runtime::Key::Enter) && !k.repeat,
                    k.pressed,
                ) {
                    KeyRouter::route_link(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        host,
                        &target,
                    );
                } else if let Some(key) = access_key
                    && ws.route_access_key(root, key, k.key, modifiers)
                {
                    // The focused node's handlers, an access key or the menu
                    // bar took it.
                } else if ws.store.keys_reach_clients()
                    && (ws.store.focused().is_none()
                        || k.repeat
                        || ws.store.frame_clients_key(k.key, k.pressed))
                {
                    // No UI node takes keys, or a focused game viewport
                    // does: a running game takes them. An auto-repeat is not
                    // a new press for it; a key the game has no use for
                    // reaches the viewport's ancestry. With nothing focused,
                    // a key no game takes scrolls the window's viewport.
                    if ws.store.focused().is_none() {
                        let took = if k.repeat {
                            ws.store.has_frame_clients()
                        } else {
                            ws.store.frame_clients_key(k.key, k.pressed)
                        };
                        if !took {
                            let ev = KeyEvent {
                                key: k.key,
                                pressed: k.pressed,
                                repeat: k.repeat,
                                modifiers,
                            };
                            ws.store
                                .scroll_by_key(root, &ev, viso_ui::primary_is_logo());
                        }
                    }
                } else {
                    let ev = KeyEvent {
                        key: k.key,
                        pressed: k.pressed,
                        repeat: k.repeat,
                        modifiers,
                    };
                    let taken = KeyRouter::route_key_taken(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        &mut ws.text_edits,
                        root,
                        ev,
                        &mut ws.route_chain,
                    );
                    // A key nothing took scrolls the viewport the keyboard
                    // is in, unless the focus edits text.
                    let edits = ws.store.focused().is_some_and(|id| {
                        ws.text_edits
                            .get(id)
                            .is_some_and(|buffer| !buffer.look.read_only)
                    });
                    if !taken && !edits {
                        ws.store
                            .scroll_by_key(root, &ev, viso_ui::primary_is_logo());
                    }
                }
            }
            viso_runtime::InputSample::Text(t) => {
                // A committed (post-IME) segment routes to the focused node as an
                // IME commit — the text a control appends to its buffer.
                KeyRouter::route_ime(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    root,
                    ImeEvent::Commit { text: t.text },
                    &mut ws.route_chain,
                );
            }
            viso_runtime::InputSample::PasteRequest(r) => {
                // A paste gesture: the focus says what it takes, read by the
                // backend once this returns.
                r.reply.set(ws.paste_route(root).1);
            }
            viso_runtime::InputSample::Paste(p) => {
                // An answer goes where its request said; a gesture's to the
                // paste target the focus is in, if any.
                let to = match p.token {
                    Some(token) => ws
                        .pastes
                        .iter()
                        .position(|(t, _)| *t == token)
                        .map(|at| ws.pastes.remove(at).1),
                    None => Some(ws.paste_route(root).0),
                };
                match to {
                    // An answer to no request still pending.
                    None => {}
                    Some(Some(node)) => {
                        KeyRouter::route_paste(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            node,
                            &p.data,
                        );
                    }
                    // Pasted text routes to the focused control, which
                    // replaces its selection with it as one edit. A
                    // single-line buffer folds any line breaks in it.
                    Some(None) => {
                        if let Some(text) = p.data.text {
                            KeyRouter::route_ime(
                                &mut ws.store,
                                &mut ws.states,
                                &ws.bindings,
                                &mut ws.text_edits,
                                root,
                                ImeEvent::Paste { text },
                                &mut ws.route_chain,
                            );
                        }
                    }
                }
            }
            viso_runtime::InputSample::Copy(c) => {
                // The platform asks for the selection while its copy/cut handler
                // is still on the stack. Apply any edits queued this event batch
                // first so the reply reflects what the user sees, then answer
                // from the focused text control's buffer. An empty selection,
                // or a secure field's, leaves the reply unset and the
                // clipboard untouched.
                let geometry = ws.text.as_ref().map(|t| t as &dyn text_edit::EditGeometry);
                text_edit::reconcile(&mut ws.store, &mut ws.text_edits, geometry);
                let Some(node) = ws.focused_text_node() else {
                    // Another focused control copies as its shortcut asks:
                    // what it hands the clipboard answers the platform.
                    let apple = viso_ui::primary_is_logo();
                    let ev = KeyEvent {
                        key: if c.cut {
                            viso_runtime::Key::X
                        } else {
                            viso_runtime::Key::C
                        },
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers {
                            logo: apple,
                            control: !apple,
                            ..Modifiers::default()
                        },
                    };
                    if ws.store.focused().is_some() {
                        KeyRouter::route_key_taken(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                            root,
                            ev,
                            &mut ws.route_chain,
                        );
                    }
                    if let Some(data) = ws.store.take_copied() {
                        c.reply.set(data);
                    }
                    return;
                };
                let Some(buffer) = ws.text_edits.get_mut(node) else {
                    return;
                };
                let (start, end) = buffer.sel.logical_range();
                if start == end || buffer.look.secure || (c.cut && buffer.look.read_only) {
                    return;
                }
                c.reply
                    .set(viso_ui::ClipboardData::text(&buffer.text[start.0..end.0]));
                if c.cut {
                    buffer.queue(text_edit::EditIntent::Cut);
                    ws.settle_text_edits();
                }
            }
            viso_runtime::InputSample::Drop(d) => {
                // A drag from outside comes over the target under it; the
                // reply tells the OS what a drop there would do.
                let phase = match d.phase {
                    viso_runtime::DropPhase::Over => viso_ui::ArrivalPhase::Over,
                    viso_runtime::DropPhase::Leave => viso_ui::ArrivalPhase::Leave,
                    viso_runtime::DropPhase::Drop => viso_ui::ArrivalPhase::Drop,
                };
                let at = viso_ui::Vec2::new(d.x / scale, d.y / scale);
                let operation = ws
                    .store
                    .drag_from_outside(root, phase, at, outside_data(&d.offer));
                d.reply.set(operation.map(|op| match op {
                    viso_ui::DropOperation::Move => viso_runtime::DropAction::Move,
                    viso_ui::DropOperation::Copy => viso_runtime::DropAction::Copy,
                    viso_ui::DropOperation::Link => viso_runtime::DropAction::Link,
                }));
            }
            viso_runtime::InputSample::Motion { dx, dy, .. } => {
                KeyRouter::route_motion(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    viso_ui::Vec2::new(dx / scale, dy / scale),
                );
            }
            viso_runtime::InputSample::DragEnded { action, .. } => {
                // The drag the OS took ended: its source hears how.
                ws.store.drag_out_ended(action.map(|a| match a {
                    viso_runtime::DropAction::Move => viso_ui::DropOperation::Move,
                    viso_runtime::DropAction::Copy => viso_ui::DropOperation::Copy,
                    viso_runtime::DropAction::Link => viso_ui::DropOperation::Link,
                }));
            }
            viso_runtime::InputSample::EditMenu(e) => {
                // The menu's Select All selects the focused field's text and
                // keeps the menu; a menu that closed on its own is no longer
                // shown, so a later one is pushed afresh.
                // Only the touch menu's Select All keeps the handles; a
                // context menu's is a plain one.
                let edit = match e.action {
                    viso_platform::EditMenuAction::SelectAll if ws.edit_menu.is_some() => {
                        text_edit::EditIntent::Touch(text_edit::TouchEdit::SelectAll)
                    }
                    viso_platform::EditMenuAction::SelectAll => text_edit::EditIntent::SelectAll,
                    viso_platform::EditMenuAction::Closed => {
                        ws.edit_menu = None;
                        ws.context_menu = None;
                        text_edit::EditIntent::Touch(text_edit::TouchEdit::MenuClosed)
                    }
                };
                if let Some(node) = ws.focused_text_node() {
                    ws.store.queue_edit(node, edit);
                }
            }
            viso_runtime::InputSample::ImePreedit(p) => {
                // An in-progress composition routes as a preedit; a control shows
                // it inline and replaces it on each update until the commit.
                KeyRouter::route_ime(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    root,
                    ImeEvent::Preedit {
                        text: p.text,
                        caret: p.caret,
                        clauses: p.clauses,
                    },
                    &mut ws.route_chain,
                );
            }
            viso_runtime::InputSample::ImeEdit(e) => {
                // An input method's edit of the text around the caret routes
                // as the preedit does.
                KeyRouter::route_ime(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    root,
                    ImeEvent::Edit(e.edit),
                    &mut ws.route_chain,
                );
            }
            viso_runtime::InputSample::Scroll(s) => {
                if ws.drawn_menu.shown() == Some(edit_menu::MenuLook::List) {
                    ws.close_drawn_menu();
                }
                // A wheel/trackpad sample routes to the innermost scroll viewport
                // under the pointer. The runtime reports the delta as content
                // motion (positive = content moves down/right, revealing later
                // content), which is exactly the direction the viewport's offset
                // grows, so it maps straight onto the offset delta. Both the hit
                // position and the delta are divided by the scale factor into
                // logical points — the offset lives in the same logical space as
                // layout. The router clamps per axis and marks only
                // TRANSFORM — a scroll never relayouts or repaints — so no
                // state flush is involved.
                let ev = ScrollEvent {
                    x: s.x / scale,
                    y: s.y / scale,
                    delta_x: s.delta_x / scale,
                    delta_y: s.delta_y / scale,
                    modifiers: lower_modifiers(s.modifiers),
                    phase: s.phase,
                };
                // A wheel turned with Control held zooms what takes pinches
                // under it, as a browser reports a trackpad's pinch.
                let zoomed = ev.modifiers.control
                    && viso_ui::route_wheel_zoom(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        root,
                        (ev.x, ev.y, ev.delta_y),
                        &mut ws.route_chain,
                    )
                    .is_some();
                // A node that takes the wheel hears it before the viewports.
                let taken = !zoomed
                    && viso_ui::route_wheel(
                        &mut ws.store,
                        &mut ws.states,
                        &ws.bindings,
                        root,
                        ev,
                        &mut ws.route_chain,
                    );
                if !zoomed && !taken {
                    ScrollRouter::route(&mut ws.store, root, ev);
                    // A focused slider under the wheel steps.
                    if ws.store.wakes_due() {
                        KeyRouter::route_wakes(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                    }
                }
            }
            viso_runtime::InputSample::Gesture(g) => {
                let ev = viso_ui::TrackpadPinch {
                    x: g.x / scale,
                    y: g.y / scale,
                    scale: g.scale,
                    rotation: g.rotation,
                    dx: g.dx / scale,
                    dy: g.dy / scale,
                    phase: match g.phase {
                        viso_runtime::GesturePhase::Began => viso_ui::PinchPhase::Start,
                        viso_runtime::GesturePhase::Changed => viso_ui::PinchPhase::Move,
                        viso_runtime::GesturePhase::Ended => viso_ui::PinchPhase::End,
                    },
                };
                viso_ui::route_trackpad_pinch(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    root,
                    ev,
                    &mut ws.route_chain,
                );
            }
        }
        if edits_text || ws.store.has_edit_requests() {
            ws.settle_text_edits();
        }
        ws.tell_focus();
        ws.store.set_dirty_cause(cause);
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(journal) = &mut ws.journal {
            if let Some((input, at)) = journaled {
                journal.input(input, at, &ws.route_chain);
            }
            journal.settle(&mut ws.store, &ws.states);
        }
    }

    fn on_lifecycle(&mut self, cx: &mut RuntimeCx<'_>, event: Lifecycle) {
        match event {
            // Frames were skipped while suspended and a surface may have been
            // recreated underneath, so every window paints once on return.
            // What the suspend paused plays again.
            Lifecycle::Resumed => {
                for ws in &mut self.windows {
                    cx.request_redraw(ws.window);
                    ws.store.resume();
                    ws.videos.resume(&ws.store);
                }
                self.tell_lifecycle(event);
            }
            // Give back the GPU memory the next frame can rebuild on demand,
            // and the preserved branches switched away: the next settle frees
            // them, and each mounts afresh when shown again.
            Lifecycle::LowMemory => {
                for ws in &mut self.windows {
                    ws.store.request_memory_trim(&mut ws.states);
                    ws.trim_memory();
                    cx.request_redraw(ws.window);
                }
                self.tell_lifecycle(event);
            }
            // The app may not come back: what the app writes as it goes is
            // made durable with the rest of the persisted writes. Nothing
            // plays in the background: the videos and the games' audio
            // pause.
            Lifecycle::Suspended => {
                self.tell_lifecycle(event);
                self.places.flush();
                for ws in &mut self.windows {
                    ws.videos.suspend();
                    persist::suspend(&mut ws.store, &ws.states);
                }
            }
        }
    }

    fn on_window_placed(&mut self, cx: &mut RuntimeCx<'_>, window: WindowId) {
        self.places.placed(cx, window);
        self.places.store_due(cx.frame_now());
    }

    fn on_wakeup(&mut self, cx: &mut RuntimeCx<'_>) {
        shell::apply(cx, &mut self.shell);
        self.places.store_due(cx.frame_now());
        // A loop kick: a task woke (or was spawned), or a timer deadline
        // elapsed. Frame the windows with work, so the frame polls the tasks
        // and fires the timers.
        #[cfg(feature = "hot-reload")]
        {
            self.hot_reload.wakeup(cx, &self.windows);
            for ask in self.hot_reload.take_profiles() {
                match ask {
                    crate::hot_reload::ProfileAsk::Start { id, hud, drive } => {
                        self.start_profile(id, hud, true, drive, cx);
                        if hud {
                            for ws in &self.windows {
                                cx.request_redraw(ws.window);
                            }
                        }
                    }
                    crate::hot_reload::ProfileAsk::End { id } => self.end_profile(id, cx),
                }
                if !self.profiler.hud() {
                    self.show_hud(None, cx);
                }
            }
        }
        let now = cx.frame_now();
        // A decoded asset frames the windows that show assets.
        let decoded = self.assets.receive();
        let played = self.assets.playback.receive();
        for ws in &self.windows {
            let timer_due = ws
                .timers
                .earliest()
                .into_iter()
                .chain(ws.caret_due)
                .chain(ws.animation_due)
                .chain(ws.video_step.due)
                .chain(ws.text_due)
                .any(|at| at <= now);
            let asset_due =
                (decoded && !ws.store.assets().is_empty()) || (played && !ws.players.is_empty());
            if ws.store.tasks_woken() || timer_due || asset_due {
                cx.request_redraw(ws.window);
            }
        }
    }

    fn on_surface(&mut self, cx: &mut RuntimeCx<'_>, window: WindowId, available: bool) {
        let raw = cx.raw_handle(window);
        let Some(ws) = self.window_mut(window) else {
            return;
        };
        // A lost device makes no surface: the frame replacing it does.
        if ws.gpu_lost() {
            cx.request_redraw(window);
            return;
        }
        let Some(gpu) = &mut ws.gpu else {
            return;
        };
        if let Some(old) = gpu.surface.take() {
            gpu.backend.destroy_surface(old);
        }
        let Some(raw) = raw.filter(|r| available && !matches!(r, RawWindowHandle::Headless)) else {
            return;
        };
        let (w, h) = gpu.size;
        let surface = gpu.backend.create_surface(raw, w, h);
        // A new native window may composite differently from the old one;
        // the pipelines are rebuilt only then.
        if gpu.backend.surface_format(surface) != gpu.renderer.surface_format()
            || gpu.backend.surface_color_space(surface) != gpu.renderer.color_space()
        {
            gpu.renderer = footprint::renderer(&mut gpu.backend, surface);
            ws.shaders = crate::shaders::InstalledShaders::default();
            ws.store.scenes_mut().forget_renderer();
        }
        gpu.surface = Some(surface);
        gpu.renderer.damage_all();
        if let Some(root) = ws.root {
            ws.store.mark_dirty(root, DirtyClass::PAINT);
        }
        cx.request_redraw(window);
    }

    fn on_menu_command(&mut self, command: viso_platform::MenuCommandId) {
        // Hand the pick straight to the user application; the scheduler already
        // flagged the frame input-dirty, so any state it writes flushes next
        // frame. A menu command targets the app, not a window, so there is no
        // per-window routing here.
        if let Some(app) = self.app.as_mut() {
            app.on_menu_command(command);
        }
    }

    fn on_open_url(&mut self, cx: &mut RuntimeCx<'_>, url: &str) {
        self.pending_urls.push(url.to_owned());
        self.open_pending_urls(cx);
    }

    fn after_input(&mut self, cx: &mut RuntimeCx<'_>, window: WindowId) {
        let Some(ws) = self.window_mut(window) else {
            return;
        };
        // The cursor follows the mouse as it moves, not at the next frame,
        // which a move that changes nothing else never asks for; a pointer
        // lock is asked while the press that asked for it is still the
        // event being handled, as a browser requires.
        ws.push_pointer_lock(cx);
        ws.push_cursor(cx);
    }

    fn on_pointer_lock(&mut self, window: WindowId, locked: bool) {
        if let Some(ws) = self.window_mut(window) {
            KeyRouter::route_pointer_lock(
                &mut ws.store,
                &mut ws.states,
                &ws.bindings,
                &mut ws.text_edits,
                locked,
            );
        }
    }

    fn on_tray(&mut self, event: viso_platform::TrayEvent) {
        if let Some(app) = self.app.as_mut() {
            app.on_tray(event);
        }
    }

    fn on_window_focus(&mut self, window: WindowId, focused: bool) {
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            recorder.focus(window, focused);
        }
        // A window losing focus stops delivering key-ups: release every key
        // its games hold so none stays down.
        if let Some(ws) = self.windows.iter_mut().find(|w| w.window == window) {
            if !focused {
                ws.store.release_client_input();
                // A pointer lock ends with the focus.
                KeyRouter::route_pointer_lock_end(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                );
                // Nor Alt's: the access keys' cues go.
                ws.alt_alone = false;
                KeyRouter::route_cues(
                    &mut ws.store,
                    &mut ws.states,
                    &ws.bindings,
                    &mut ws.text_edits,
                    false,
                );
            }
            // An inactive window shows no caret and greys its selection.
            ws.active = focused;
        }
    }

    fn wants_gamepads(&self) -> bool {
        true
    }

    fn on_gamepad(&mut self, cx: &mut RuntimeCx<'_>, event: viso_runtime::GamepadEvent) {
        use viso_runtime::GamepadEvent;
        let connected = !self.gamepads.is_empty();
        match &event {
            GamepadEvent::Connected(info) => self.gamepads.push(info.id),
            GamepadEvent::Disconnected(id) => self.gamepads.retain(|pad| pad != id),
            GamepadEvent::Changed(..) => {}
        }
        let available = !self.gamepads.is_empty();
        for ws in &mut self.windows {
            if available != connected {
                // `env.input.gamepad_available` follows the connections.
                environment::note_gamepads(&mut ws.states, available);
                cx.request_redraw(ws.window);
            }
            // A pad's input goes to the games of the window holding input
            // focus, while no UI node takes the keys; every game forgets a
            // pad that went away.
            let reaches = match event {
                GamepadEvent::Connected(_) => false,
                GamepadEvent::Changed(..) => ws.active && ws.store.keys_reach_clients(),
                GamepadEvent::Disconnected(_) => true,
            };
            if reaches && ws.store.has_frame_clients() {
                ws.store.frame_clients_gamepad(&event);
            }
        }
    }

    fn on_window_chrome_geom(
        &mut self,
        _cx: &mut RuntimeCx<'_>,
        window: WindowId,
        buttons_rect: LogicalRect,
    ) {
        // Record the native traffic-light box so a caption widget can align its
        // content around the buttons (it reads the width through `BuildCx::chrome`)
        // and so the app can too. The draggable regions are no longer derived here:
        // a caption widget declares which of its nodes are draggable, and the Layout
        // phase pushes their real world boxes to the platform once the tree has been
        // laid out — the geometry is only half the picture, the widget's own layout
        // is the other half. An event for an unknown window (already closed) is a
        // no-op.
        if let Some(ws) = self.window_mut(window) {
            ws.chrome_buttons = Some(buttons_rect);
        }
    }

    fn on_safe_area(&mut self, window: WindowId, insets: LogicalInsets) {
        if let Some(ws) = self.window_mut(window) {
            ws.safe_area = insets;
            ws.apply_safe_area();
            ws.report_window();
        }
    }

    fn on_appearance(&mut self, appearance: viso_platform::Appearance) {
        for ws in &mut self.windows {
            ws.report_appearance(appearance);
        }
    }

    fn on_locale(&mut self, locale: &str) {
        for ws in &mut self.windows {
            ws.report_locale(locale);
        }
    }

    fn on_display_features(
        &mut self,
        window: WindowId,
        features: &[viso_platform::DisplayFeature],
    ) {
        if let Some(ws) = self.window_mut(window) {
            environment::report_display_features(&mut ws.states, features);
        }
    }

    fn on_keyboard_inset(&mut self, window: WindowId, height: f64) {
        if let Some(ws) = self.window_mut(window) {
            ws.keyboard_inset = height;
            ws.apply_safe_area();
            ws.report_window();
        }
    }

    fn on_accessibility(&mut self, window: WindowId, request: viso_platform::AccessRequest) {
        if PANEL_ONLY || !self.accessibility {
            return;
        }
        let Some(ws) = self.window_mut(window) else {
            return;
        };
        let (target, action) = match request {
            viso_platform::AccessRequest::Activated => {
                ws.access = Some(accessibility::AccessBridge::new());
                return;
            }
            viso_platform::AccessRequest::Deactivated => {
                ws.access = None;
                return;
            }
            viso_platform::AccessRequest::Action { target, action } => (target, action),
        };
        let Some((node, link)) = ws.access.as_ref().and_then(|access| access.target(target)) else {
            return;
        };
        if !ws.store.arena().is_live(node) {
            return;
        }
        let scale = ws.dpi.max(1.0);
        // The action's samples arrive with the last input.
        let at = ws
            .store
            .input_time()
            .unwrap_or_else(viso_runtime::Instant::now);
        if let Some(samples) =
            accessibility::perform(&mut ws.store, window, node, link, action, scale)
        {
            for sample in samples {
                self.on_input(sample, at);
            }
        }
    }

    fn on_fullscreen_changed(&mut self, window: WindowId, fullscreen: bool) {
        self.places.fullscreen_changed(window, fullscreen);
        // Hide the self-drawn caption while fullscreen and restore it on exit by
        // toggling its root's visibility in place — on macOS the OS draws its own
        // auto-hiding title bar in fullscreen and removes the traffic lights, so
        // the caption must yield. `set_hidden` marks LAYOUT|PAINT, so the next
        // frame reflows the body to fill the reclaimed strip. A window with no
        // caption, or an event for an already-closed window, is a no-op.
        // Leaving it brings back a node shown full screen.
        if let Some(ws) = self.window_mut(window) {
            if let Some(cap) = ws.caption {
                ws.store.set_hidden(cap, fullscreen);
            }
            ws.store.window_fullscreen_changed(fullscreen);
        }
    }

    fn run_phase(&mut self, phase: FramePhase, cx: &mut RuntimeCx<'_>) {
        // The render phases drive the GPU from the real retained tree: Measure +
        // Layout resolve node boxes, then paint lowers them to primitives which
        // the renderer batches and submits. Non-render phases are no-ops here.
        //
        // Each phase runs once per open window. Windows own disjoint state
        // (store/gpu/animations/timers), so the per-window bodies are
        // independent — a linear walk of `self.windows` is the whole fan-out.
        // A single-window app (the common case) walks a one-element vec; the
        // cost scales with the window count, not with any per-window map lookup
        // (we iterate all windows, never key one).
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if phase == FramePhase::CollectInput {
            self.begin_profiled_frame();
            if let Some(recorder) = &mut self.recorder {
                recorder.frame(cx.frame_delta());
            }
            for ws in &mut self.windows {
                if let Some(journal) = &mut ws.journal
                    && journal.begin_frame()
                {
                    ws.journal = None;
                    crate::inspect::Journal::end(&mut ws.store);
                }
            }
        }
        let _span = match phase {
            FramePhase::FlushStateTransactions => Some(crate::profile::time(Span::Flush)),
            FramePhase::UpdateSemantics => Some(crate::profile::time(Span::Semantics)),
            FramePhase::UploadGpuChanges => Some(crate::profile::time(Span::Upload)),
            FramePhase::Submit => Some(crate::profile::time(Span::Submit)),
            _ => None,
        };
        match phase {
            FramePhase::RunContinuations => {
                // The frame earns its share of inline work.
                self.pace.begin_frame(cx.frame_delta());
                // The faults the games raised last frame: to `viso run` when
                // it is attached, to stderr otherwise.
                let faults = viso_view::take_game_faults();
                #[cfg(feature = "hot-reload")]
                let faults = self.hot_reload.forward_game_faults(faults);
                for line in faults {
                    eprintln!("[viso] {line}");
                }
                // The window places that settled.
                self.places.store_due(cx.frame_now());
                // A window whose device was lost draws on a new one from this
                // frame, once one opens.
                for ws in &mut self.windows {
                    if ws.gpu_lost() {
                        let raw = native_handle(cx, ws.window);
                        ws.recover_gpu(raw, &self.fonts, cx.frame_now());
                    }
                }
                for ws in &mut self.windows {
                    // Arm any one-shot timers a handler requested this frame (a
                    // toast's auto-dismiss), then fire every timer whose deadline
                    // this frame's instant has crossed. Arming resolves each
                    // request's `delay` against `frame_now` (not a stray
                    // `Instant::now`), so a headless `ManualClock` stays
                    // deterministic. `fire_due` runs each due callback with the
                    // live store and removes it, so the registry empties itself
                    // and the loop can halt again — a timer costs no frame while
                    // it waits, only the one frame it fires on. Both steps are
                    // no-ops when no timer is armed or pending (the steady case).
                    ws.store.set_dirty_cause(DirtyCause::Task);
                    ws.store.take_timer_requests(&mut ws.timer_requests);
                    for req in ws.timer_requests.drain(..) {
                        ws.timers.arm_request(req, cx.frame_now());
                    }
                    if !ws.timers.is_empty() {
                        ws.timers.fire_due(&mut ws.store, cx.frame_now());
                        // A press held still that long is a long press now.
                        if ws.store.long_press_due() {
                            viso_ui::deliver_long_presses(
                                &mut ws.store,
                                &mut ws.states,
                                &ws.bindings,
                            );
                        }
                        // A wheel's pinch that rested ends.
                        if ws.store.pinch_end_due() {
                            viso_ui::deliver_pinch_ends(
                                &mut ws.store,
                                &mut ws.states,
                                &ws.bindings,
                            );
                        }
                        // The wakes whose time came reach their handlers.
                        if ws.store.wakes_due() {
                            KeyRouter::route_wakes(
                                &mut ws.store,
                                &mut ws.states,
                                &ws.bindings,
                                &mut ws.text_edits,
                            );
                            ws.tell_focus();
                        }
                    }
                    // The viewports a pull asked to refresh hear it.
                    if ws.store.refreshes_due() {
                        KeyRouter::route_refreshes(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // The snapping viewports whose item changed hear it.
                    if ws.store.snaps_due() {
                        KeyRouter::route_snaps(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // The rows whose gestures settled hear what they did.
                    if ws.store.row_gestures_due() {
                        KeyRouter::route_row_gestures(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // The sheets a drag moved hear where it left them.
                    if ws.store.sheets_due() {
                        KeyRouter::route_sheets(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // The targets and sources a drag passed hear what it did.
                    if ws.store.drops_due() {
                        KeyRouter::route_drops(
                            &mut ws.store,
                            &mut ws.states,
                            &ws.bindings,
                            &mut ws.text_edits,
                        );
                        ws.tell_focus();
                    }
                    // Poll the tasks spawned or woken since the last frame and
                    // run each finished one's continuation; what it writes joins
                    // this frame's transaction. No woken task (the steady case)
                    // takes no lock and polls nothing.
                    // Off-loop jobs a build without worker threads runs here,
                    // within what is left of the frame's inline share.
                    self.pace.run(|slice| slice.map(viso_ui::work::run_inline));
                    if ws.store.tasks_woken() {
                        ws.store.poll_tasks(&mut ws.continuations);
                        for then in ws.continuations.drain(..).flatten() {
                            then(&mut UpdateCx::__new(
                                &mut ws.states,
                                &ws.bindings,
                                &mut ws.store,
                            ));
                        }
                    }
                    ws.store.set_dirty_cause(DirtyCause::Other);
                }
            }
            #[cfg(feature = "hot-reload")]
            FramePhase::ApplyDevPatches => {
                // Commit the edits the watcher staged at the frame boundary, so
                // a reload's writes join this frame's transaction and paint
                // this frame; a snapshot the host asked for takes where each
                // window is.
                {
                    if self.hot_reload.snapshot_asked() {
                        let positions = self
                            .windows
                            .iter()
                            .map(|ws| (ws.window, cx.window_position(ws.window)))
                            .collect();
                        self.hot_reload.set_positions(positions);
                    }
                    for ws in &mut self.windows {
                        ws.store.set_dirty_cause(DirtyCause::Reload);
                    }
                    let before = self.hot_reload.revision();
                    self.hot_reload.reload(&mut self.windows, &mut self.assets);
                    if let (Some(recorder), Some(revision)) =
                        (&mut self.recorder, self.hot_reload.revision())
                        && before != Some(revision)
                    {
                        recorder.patch(revision);
                    }
                    for ws in &mut self.windows {
                        ws.store.set_dirty_cause(DirtyCause::Other);
                    }
                }
            }
            FramePhase::AdvanceAnimations => {
                for ws in &mut self.windows {
                    // Advance every moving look property and animation by this
                    // frame's delta, writing the value each node shows (a
                    // TRANSFORM write for a translate). This runs before the
                    // state flush and the Layout phase, so a moved translate is
                    // resolved into `world` and painted this same frame. A
                    // no-op when nothing moves (the steady case).
                    ws.store.set_dirty_cause(DirtyCause::Animation);
                    if ws.store.is_transitioning() {
                        ws.store.tick_transitions(cx.frame_delta());
                    }
                    // The viewports a flick left coasting move on.
                    if ws.store.is_scroll_moving() {
                        ws.store.tick_scroll_motion(cx.frame_delta());
                    }
                    // The spinning arcs chase on.
                    if ws.store.is_spinning() {
                        ws.store.tick_spinners(cx.frame_delta());
                    }
                    // The placeholders shimmer on.
                    if ws.store.is_shimmering() {
                        ws.store.tick_shimmers(cx.frame_delta());
                    }
                    // The press ripples spread and fade.
                    if ws.store.is_rippling() {
                        ws.store.tick_ripples(cx.frame_delta());
                    }
                    // The progress indicators run, or ease to their values.
                    if ws.store.is_progressing() {
                        ws.store.tick_progress(cx.frame_delta());
                    }
                    // The sliders' thumbs ease to where they were told.
                    if ws.store.is_sliding() {
                        ws.store.tick_sliders(cx.frame_delta());
                    }
                    ws.store.set_dirty_cause(DirtyCause::Other);
                    // Advance the games the window's views run — the ticks
                    // their clocks owe, then their frame hooks — after the dev
                    // patches committed, so a patch staged during the last
                    // frame's ticks runs from the next tick on.
                    ws.store.run_frame_clients(cx.frame_delta().as_secs_f64());
                    // A game's rumble plays on every pad of its local
                    // player, while its window holds input focus.
                    if let Some(rumble) = ws.store.take_frame_client_rumble()
                        && ws.active
                    {
                        for pad in &self.gamepads {
                            cx.rumble_gamepad(*pad, rumble);
                        }
                    }
                }
            }
            FramePhase::FlushStateTransactions => {
                for ws in &mut self.windows {
                    // Flush this frame's one transaction — the handlers',
                    // continuations', dev patches' and animations' writes —
                    // through the derivations, bindings, structure hooks and
                    // semantic-state projections, re-flushing any write a hook
                    // makes while settling, and queue the effects it wakes for
                    // `RunEffects`. Many writes collapse into one round; a frame
                    // with no writes touches nothing. A flush that never settles
                    // is a reactive cycle: it stops at a bound and drops the
                    // rest.
                    if let Err(cycle) = viso_ui::settle_states(
                        &mut ws.store,
                        &mut ws.states,
                        &mut ws.bindings,
                        &mut ws.computeds,
                        &mut ws.projectors,
                        &mut ws.effects,
                        &mut ws.virtual_lists,
                        &mut ws.changed,
                    ) {
                        report_cycle(
                            #[cfg(feature = "hot-reload")]
                            &mut self.hot_reload,
                            &cycle,
                            ws,
                        );
                    }
                }
                // Serve the assets the settle offered, before layout: what
                // decoded since the last frame reaches every window, and each
                // window whose table moved gets its decodes started and its
                // rasters uploaded, so this frame measures and paints them.
                for ws in &mut self.windows {
                    ws.store.set_dirty_cause(DirtyCause::Asset);
                }
                self.assets
                    .provide(self.windows.iter_mut().map(|ws| &mut ws.store));
                for ws in &mut self.windows {
                    if ws.store.assets().revision() == ws.assets_served {
                        continue;
                    }
                    let gpu = ws.gpu.as_mut().map(|g| (&mut g.backend, &mut g.renderer));
                    let served = self.assets.serve(
                        &mut ws.store,
                        gpu,
                        &|| cx.loop_waker(),
                        self.pace.slice(),
                    );
                    self.pace.charge(served.inline);
                    if served.deferred {
                        // Decodes left for the next frame's slice.
                        cx.request_redraw(ws.window);
                    } else {
                        ws.assets_served = ws.store.assets().revision();
                    }
                }
                self.sweep_assets();
                self.play_animations(cx.frame_now(), &|| cx.loop_waker());
                self.play_videos(cx.frame_now());
                for ws in &mut self.windows {
                    ws.store.set_dirty_cause(DirtyCause::Other);
                }
                // Install the programs the settle's shader views took and
                // release the ones no node paints with any more, so this
                // frame paints with them.
                for ws in &mut self.windows {
                    if let Some(gpu) = ws.gpu.as_mut() {
                        ws.shaders
                            .sync(ws.store.shaders(), &mut gpu.backend, &mut gpu.renderer);
                    }
                }

                // The links no handler kept open in the apps their URLs
                // belong to, and the haptic the frame's controls asked for;
                // what the system answers is not the app's to see.
                let services = self
                    .services
                    .as_deref()
                    .and_then(|any| any.downcast_ref::<services::Services>());
                for ws in &mut self.windows {
                    for url in ws.store.take_link_opens() {
                        if let Some(services) = services {
                            drop(services.launcher().open_url(&url));
                        }
                    }
                    if let Some(haptic) = ws.store.take_haptic()
                        && let Some(services) = services
                    {
                        drop(services.haptics().play(play(haptic)));
                    }
                }

                // Session-level window open/close drain, once per frame after
                // every window has flushed its own transaction. A `window(...)`
                // handler or a `WindowHandle::close` recorded its request through
                // `EventCx`, the router handed it to the *originating* window's
                // store queue, and here — the one phase holding a live
                // `RuntimeCx` — the driver services it. It runs after the
                // per-window loop, not inside it, because opening a window pushes
                // to `self.windows` (which the loop borrows) and needs the live
                // `cx` to create the OS window; draining every window's queue
                // into driver-owned scratch first releases that borrow.
                //
                // Collect this frame's requests off every open window's store
                // into the reusable driver scratch. `take_*` clears the scratch
                // and appends, so a window that queued nothing contributes
                // nothing; a frame with no `window()` call leaves both empty and
                // the two loops below run zero iterations (steady-path no-op).
                self.pending_opens.clear();
                self.pending_closes.clear();
                for ws in &mut self.windows {
                    if let Some(data) = ws.store.take_copied() {
                        cx.set_clipboard(data);
                    }
                    ws.store.take_paste_asks(&mut ws.scratch_pastes);
                    for i in 0..ws.scratch_pastes.len() {
                        let (node, kinds) = ws.scratch_pastes[i];
                        let token = ws.paste_token(Some(node));
                        cx.request_paste(ws.window, kinds, token);
                    }
                    if let Some(on) = ws.store.take_fullscreen_ask() {
                        cx.set_fullscreen(ws.window, on);
                    }
                    ws.push_pointer_lock(cx);
                    ws.store.take_window_opens(&mut ws.scratch_opens);
                    self.pending_opens.append(&mut ws.scratch_opens);
                    ws.store.take_window_closes(&mut ws.scratch_closes);
                    self.pending_closes.append(&mut ws.scratch_closes);
                }
                // Open each requested window through the shared bring-up path:
                // create the OS window (bumping the scheduler's open count so the
                // loop stays alive), then author its tree with the deferred build
                // closure the handler passed to `window(...).content(...)`. The
                // new window's root is marked dirty, so it renders next frame with
                // no self-scheduled spin. A `create_window` failure drops the
                // request — the app simply gets no window, no panic.
                for req in self.pending_opens.drain(..) {
                    // Carry the UI-tier chrome mode through to the build below as
                    // the build-time half of the caption data contract. It is the
                    // same value translated into the platform config, kept as the
                    // UI enum so `WindowState::open` seeds `BuildCx::chrome`.
                    let ui_chrome = req.config.chrome;
                    let mut config = to_platform_config(&req.config);
                    let opening = self.places.open(
                        req.config.persist.as_deref(),
                        cx.framed_windows(),
                        &mut config,
                    );
                    // Values the build closure moves in to wrap the deferred tree
                    // in the default caption band, exactly as the launch path does.
                    let title = req.config.title.clone();
                    let framed = !PANEL_ONLY && cx.framed_windows();
                    let caption = req.config.caption && framed;
                    let build = req.build;
                    let id_slot = req.id_slot;
                    if let Ok(id) = cx.create_window(config) {
                        if let Some(opening) = opening {
                            self.places.opened(cx, id, opening);
                        }
                        // Back-fill the tracking handle's slot with the raw id
                        // so a `WindowHandle` can observe and later close this
                        // window. `create_window` is the one place the id is
                        // known; an untracked open (`id_slot == None`) skips it.
                        if let Some(slot) = &id_slot {
                            slot.set(Some(id.0));
                        }
                        // Read the native traffic-light box synchronously so the
                        // deferred build sees it, exactly as the launch path does.
                        let chrome_geom = cx.window_chrome_geom(id);
                        let bar = drawn_menu_bar(
                            self.app_menu.as_ref(),
                            &self.menu_picks,
                            cx,
                            id,
                            caption,
                        );
                        // Capture the caption root during the synchronous build, as
                        // the launch path does, to hold for fullscreen hide.
                        let caption_cell = Cell::new(None);
                        let mut ws = WindowState::open(
                            cx,
                            id,
                            &self.fonts,
                            !self.gamepads.is_empty(),
                            self.services.clone(),
                            ui_chrome,
                            chrome_geom,
                            |cx| {
                                if framed {
                                    wrap_root_with_caption(
                                        cx,
                                        &title,
                                        caption,
                                        &caption_cell,
                                        bar,
                                        build,
                                    )
                                } else {
                                    wrap_root_in_safe_area(cx, build)
                                }
                            },
                        );
                        ws.caption = caption_cell.get();
                        if !framed {
                            ws.safe_area_root = ws.root;
                        }
                        #[cfg(feature = "hot-reload")]
                        self.hot_reload.adopt(|| cx.loop_waker(), &mut ws);
                        #[cfg(feature = "headless")]
                        self.harness.adopt(&mut ws);
                        self.windows.push(ws);
                    }
                }
                // Close each requested window through the platform seam only:
                // `close_window` destroys the OS window, which delivers a
                // `WindowClosed` event, which routes to `on_window_closed` — the
                // single teardown path shared with a user-driven OS close. We do
                // *not* retain-drop the `WindowState` here: doing both would tear
                // the window down twice and desync the scheduler's open count.
                for raw in self.pending_closes.drain(..) {
                    cx.close_window(WindowId(raw));
                }
            }
            FramePhase::Layout => {
                for ws in &mut self.windows {
                    // Reconcile virtualized lists first: each list reads its
                    // viewport's current scroll, and only when the visible range
                    // crossed a row boundary does it recycle/rebind a handful of
                    // hosts and mark the canvas LAYOUT|MEASURE. Steady scroll
                    // within a row is a no-op here (the scroll's TRANSFORM already
                    // moved the mounted rows), so the relayout below stays
                    // confined to the changed canvas subtree.
                    ws.store.set_dirty_cause(DirtyCause::Layout);
                    virtual_list::reconcile(
                        &mut ws.store,
                        &mut ws.virtual_lists,
                        &mut ws.states,
                        &mut ws.bindings,
                        &mut ws.effects,
                    );
                    // Apply any text edits queued this frame by the key/IME router
                    // into each control's retained buffer: a buffer whose text
                    // actually changed re-declares its `TextRequest` so the
                    // shaping step below re-shapes the new string. A no-op when no
                    // control took an edit this frame (the steady case), so a
                    // non-text frame pays nothing.
                    // Commit the layouts the text worker finished since the
                    // last frame, within the commit budget, so this frame's
                    // measure sees them; what the budget leaves waits for the
                    // next frame, drawing its last good layout meanwhile.
                    ws.store.set_dirty_cause(DirtyCause::Text);
                    {
                        let _text = crate::profile::time(Span::Text);
                        ws.commit_text_work(&mut self.pace);
                        ws.settle_text_edits();
                        // Shape any text (re)declared this frame — a rebuilt
                        // list row, an applied text edit, or a future reactive
                        // text update leaves a pending `TextRequest` — into a
                        // content payload before measure, so a `Fit` text node
                        // sizes to its run. A no-op when nothing declared text
                        // (the steady case).
                        ws.shape_pending_text();
                    }
                    // Incrementally re-place invalidated subtrees; a clean frame
                    // touches nothing.
                    ws.store.set_dirty_cause(DirtyCause::Layout);
                    ws.relayout();
                    // Width-aware text reflow (DL1): the layout just above records
                    // a reflow for each wrap-eligible leaf whose assigned box width
                    // differs from the width it was shaped at. Drain and reshape
                    // those at their assigned width, then relayout so a run whose
                    // wrapped height grew re-places its siblings — bounded, and a
                    // no-op on the steady frame that enqueued none. Runs before
                    // `absorb_measurements` so a virtualized list's height model
                    // sees the final wrapped row heights, not the unconstrained
                    // single-line ones.
                    ws.store.set_dirty_cause(DirtyCause::Text);
                    let reflow_passes = {
                        let _text = crate::profile::time(Span::Text);
                        let passes = ws.reflow_wrapped_text();
                        // Every layout this frame asked for goes to the worker
                        // at once, so it lays them out while the frame renders.
                        ws.dispatch_text_work();
                        passes
                    };
                    // First-frame double-shape visibility: a wrapped `Fill`
                    // paragraph is shaped once unconstrained then once at its width
                    // on the frame it appears/resizes; this surfaces that one-time
                    // cost, gated so a steady frame (zero passes) pays only the
                    // env-var check and nothing prints.
                    if reflow_passes > 0 && std::env::var_os("VISO_DEBUG").is_some() {
                        eprintln!("[viso] text reflow: {reflow_passes} pass(es)");
                    }
                    ws.store.set_dirty_cause(DirtyCause::Layout);
                    // Feed measured row heights back into each list's height model
                    // so a variable-height list corrects its extent and anchor
                    // next frame. Bounded to this frame's newly-mounted rows — no
                    // full-list sweep.
                    virtual_list::absorb_measurements(&ws.store, &mut ws.virtual_lists);
                    // A list whose viewport this layout resized, as a list's
                    // first layout does, mounts the rows it now covers in
                    // this frame rather than the next.
                    if virtual_list::unsettled(&ws.store, &ws.virtual_lists) {
                        virtual_list::reconcile(
                            &mut ws.store,
                            &mut ws.virtual_lists,
                            &mut ws.states,
                            &mut ws.bindings,
                            &mut ws.effects,
                        );
                        ws.store.set_dirty_cause(DirtyCause::Text);
                        ws.shape_pending_text();
                        ws.store.set_dirty_cause(DirtyCause::Layout);
                        ws.relayout();
                        virtual_list::absorb_measurements(&ws.store, &mut ws.virtual_lists);
                    }
                }
            }
            FramePhase::ResolveStyle => {
                for ws in &mut self.windows {
                    ws.resolve_style();
                }
            }
            FramePhase::UpdateTransforms => {
                self.take_menu_picks(cx);
                for ws in &mut self.windows {
                    ws.update_transforms();
                    ws.represent_text(cx.frame_now());
                    // Push the caption's draggable regions to the platform, but only
                    // when this frame's layout moved them (or first established them).
                    // The world boxes are final now; a caption that held still — the
                    // steady case, and every window without one — recomputes a tiny
                    // candidate list, finds it unchanged, and skips the cold
                    // back-channel entirely.
                    if ws.draggable_regions_changed() {
                        cx.set_draggable_regions(ws.window, &ws.draggable_cache);
                    }
                    ws.update_field_marks(cx.frame_now());
                    // Tell the platform where text entry is, so an IME candidate
                    // window and the soft keyboard track the focused control. The
                    // keyboard is toggled only when focus crosses into or out of
                    // text entry, not when the control merely moves.
                    let was_editing = matches!(ws.text_entry, Some(Some(_)));
                    if ws.text_entry_changed() {
                        let entry = ws.text_entry.flatten();
                        cx.set_text_entry(ws.window, entry);
                        if was_editing != entry.is_some() {
                            cx.show_soft_keyboard(ws.window, entry.is_some());
                        }
                    }
                    // And the text around the caret, which it reads to
                    // reconvert, correct and predict.
                    if let Some(text) = ws.surrounding_changed() {
                        cx.set_surrounding_text(ws.window, text.as_ref());
                    }
                    // A drag carried past the window's edge goes on as the
                    // OS's, where the OS takes one.
                    if let Some(data) = ws.store.drag_out_asked() {
                        let started = cx.start_drag(ws.window, drag_out(&data));
                        ws.store.drag_out_started(started);
                    }
                    ws.push_cursor(cx);
                    // Platform views over nodes follow them, now that the world
                    // boxes are final.
                    if ws.store.has_native_views()
                        && let Some(window) = cx.raw_handle(ws.window)
                    {
                        let (w, h) = ws.logical_surface();
                        let viewport = viso_ui::Rect::new(0.0, 0.0, w, h);
                        ws.store
                            .place_native_views(window, viewport, ws.dpi.max(1.0));
                    }
                    // The edit menu follows what a touch made of the focused
                    // field's selection, where the platform has one.
                    let caps = cx.capabilities();
                    if caps.edit_menu && ws.edit_menu_changed(!caps.context_menu) {
                        cx.set_edit_menu(ws.window, ws.edit_menu);
                    }
                    // A secondary click or the menu key asks for the context
                    // menu once; where the system draws one it pops up.
                    if caps.context_menu
                        && ws.context_menu_changed()
                        && let Some(menu) = ws.context_menu
                    {
                        let labels = viso_platform::EditLabels::for_locale(
                            &ws.states.env().environment().locale,
                        );
                        cx.show_context_menu(ws.window, menu, labels);
                    }
                    ws.update_drawn_menu(cx);
                    if caps.loupe && ws.loupe_changed() {
                        cx.set_loupe(ws.window, ws.loupe);
                    }
                }
            }
            FramePhase::BuildPaintChanges => {
                for ws in &mut self.windows {
                    ws.repaint();
                }
            }
            FramePhase::RunEffects => {
                for ws in &mut self.windows {
                    // The effects this frame's flush queued run now, after the
                    // frame presented; what they write is the next frame's
                    // transaction, so that frame is scheduled.
                    ws.store.set_dirty_cause(DirtyCause::Task);
                    // Edits the frame itself asked for (a number field the
                    // focus left settling) apply now; what they report is the
                    // next frame's.
                    if ws.store.has_edit_requests() {
                        ws.settle_text_edits();
                        cx.request_redraw(ws.window);
                    }
                    // What the frame's layout moved reaches the nodes that
                    // report it — a viewport's scroll, a list's visible rows;
                    // their handlers' writes are the next frame's, as the
                    // effects' are.
                    if viso_ui::deliver_notices(&mut ws.store, &ws.virtual_lists, &mut ws.states)
                        > 0
                        && ws.states.has_pending()
                    {
                        cx.request_state_flush(ws.window);
                    }
                    let done = viso_ui::run_effects(&mut ws.store, &mut ws.states, &mut ws.effects);
                    if let Some(cycle) = done.cycle {
                        report_cycle(
                            #[cfg(feature = "hot-reload")]
                            &mut self.hot_reload,
                            &cycle,
                            ws,
                        );
                    }
                    if done.pending {
                        cx.request_redraw(ws.window);
                    }
                    ws.store.set_dirty_cause(DirtyCause::Other);
                }
            }
            FramePhase::UpdateSemantics if !PANEL_ONLY => {
                for ws in &mut self.windows {
                    let (Some(access), Some(root)) = (&mut ws.access, ws.root) else {
                        continue;
                    };
                    if let Some(update) = access.update(&ws.store, root, ws.dpi.max(1.0)) {
                        cx.update_accessibility(ws.window, update);
                    }
                }
            }
            FramePhase::UploadGpuChanges => {
                let now = cx.frame_now();
                for ws in &mut self.windows {
                    ws.upload_gpu_changes();
                    // Videos drawn plainly and on top show on the display's
                    // planes, where it has them, going up or down with this
                    // frame.
                    if !ws.videos.is_empty()
                        && let Some(viso_gpu::RawWindowHandle::Scanout { scanout }) =
                            cx.raw_handle(ws.window)
                    {
                        // SAFETY: a scanout handle's display lives as long as
                        // its window, on this thread.
                        ws.scan_out_videos(unsafe { &*scanout }, now);
                    }
                }
            }
            FramePhase::Submit => {
                for ws in &mut self.windows {
                    // The viewport is the logical size: the projection maps
                    // primitive positions (logical) into the physical surface,
                    // which the shader's `pos / viewport * 2 - 1` does correctly
                    // whatever the scale — the physical surface is still fully
                    // covered and fragments still rasterize at physical density.
                    let scale = ws.dpi.max(1.0);
                    if let Some(gpu) = &mut ws.gpu {
                        // The clock brushes read: this frame's instant, from the
                        // window's first frame, so a headless clock stays
                        // deterministic and a reload keeps it running.
                        let now = cx.frame_now();
                        let origin = *ws.clock_origin.get_or_insert(now);
                        gpu.renderer
                            .set_time(now.saturating_duration_since(origin).as_secs_f32());
                        let (w, h) = gpu.size;
                        let viewport = [w as f32 / scale, h as f32 / scale];
                        // One-shot proof the first frame reached the GPU, gated on
                        // an env var so it costs a single bool check per
                        // steady-state frame and nothing else. Read once, then the
                        // branch dies. Stats must be read before `submit` consumes
                        // the segments.
                        if ws.awaiting_first_frame {
                            ws.awaiting_first_frame = false;
                            if std::env::var_os("VISO_DEBUG").is_some() {
                                let stats = gpu.renderer.frame_stats();
                                let recompute = ws.recompute;
                                eprintln!(
                                    "viso: first frame submitting {w}x{h} {stats:?} {recompute:?}"
                                );
                                let a = self.assets.counters();
                                eprintln!(
                                    "viso: assets decodes={} decode_failures={} uploads={} upload_bytes={} evictions={} animation_frames={} video_frames={}",
                                    a.decodes,
                                    a.decode_failures,
                                    a.uploads,
                                    a.upload_bytes,
                                    a.evictions,
                                    self.assets.playback.frames(),
                                    ws.videos.frames,
                                );
                                if let Some(text) = &ws.text {
                                    let c = text.counters();
                                    eprintln!(
                                        "viso: text reshapes={} relinebreaks={} shaped_runs={} main_shaped_runs={} commits={} deferred={} main_commit_us={} rasters={} atlas_upload_bytes={}",
                                        c.reshapes(),
                                        c.relinebreaks(),
                                        c.shaped_runs(),
                                        c.main_shaped_runs(),
                                        c.commits(),
                                        c.deferred(),
                                        c.main_commit_micros(),
                                        c.rasters(),
                                        c.atlas_upload_bytes(),
                                    );
                                    // Glyph residency, per representation pool:
                                    // what is resident, how many pages hold it,
                                    // and how the pools turned over this frame.
                                    let r = text.residency();
                                    use viso_text::GlyphImageKind as Pool;
                                    let pools = [
                                        ("a8", Pool::MaskA8),
                                        ("mtsdf", Pool::ScalableMtsdf),
                                        ("rgba", Pool::ColorRgba8),
                                        ("vector", Pool::OutlineVector),
                                    ];
                                    for (name, kind) in pools {
                                        eprintln!(
                                            "viso: glyph pool {name} glyphs={} pages={} bytes={} upload_bytes={}",
                                            r.pool_resident_glyphs(kind),
                                            r.pool_page_count(kind),
                                            r.pool_resident_bytes(kind),
                                            r.pool_upload_bytes(kind),
                                        );
                                    }
                                    eprintln!(
                                        "viso: glyph residency evictions={} admission_failures={}",
                                        c.evictions(),
                                        c.admission_failures(),
                                    );
                                    eprintln!(
                                        "viso: glyph representations evaluations={} promotions={} demotions={} fields={}",
                                        c.representation_evaluations(),
                                        c.promotions(),
                                        c.demotions(),
                                        c.fields(),
                                    );
                                    let f = text.face_cache();
                                    eprintln!(
                                        "viso: face cache bytes={}/{} faces={} hits={} misses={} evictions={} recency_updates={}",
                                        f.total_bytes(),
                                        f.budget_bytes(),
                                        f.len(),
                                        f.hits(),
                                        f.misses(),
                                        f.evictions(),
                                        f.recency_updates(),
                                    );
                                    let s = text.shaping_cache();
                                    eprintln!(
                                        "viso: shaping cache bytes={}/{} spans={} hits={} misses={} evictions={}",
                                        s.bytes(),
                                        s.budget_bytes(),
                                        s.len(),
                                        s.hits(),
                                        s.misses(),
                                        s.evictions(),
                                    );
                                }
                            }
                        }
                        #[cfg(any(feature = "hot-reload", feature = "headless"))]
                        if self.profiler.is_on() {
                            let stats = gpu.renderer.frame_stats();
                            for (count, value) in [
                                (Count::Primitives, u64::from(stats.visible_primitives)),
                                (Count::DrawCalls, stats.draw_calls as u64),
                                (Count::Batches, stats.batches as u64),
                                (Count::Instances, stats.instances as u64),
                                (Count::UploadBytes, stats.gpu_upload_bytes as u64),
                            ] {
                                self.profiler.count(count, value);
                            }
                        }
                        #[cfg(feature = "headless")]
                        crate::__test_support::FRAME_PROBE.with(|probe| {
                            if let Ok(mut probe) = probe.try_borrow_mut()
                                && let Some(probe) = probe.as_mut()
                            {
                                probe(&gpu.renderer.frame_stats(), ws.uploaded);
                            }
                        });
                        if let Some(surface) = gpu.surface {
                            #[cfg(any(feature = "hot-reload", feature = "headless"))]
                            let before = gpu.renderer.damage_stats();
                            // The window shows the theme's background under
                            // what its views draw.
                            let under = ws.store.palette().get(viso_ui::ColorToken::Background);
                            gpu.renderer.submit(
                                &mut gpu.backend,
                                surface,
                                [
                                    under.r * under.a,
                                    under.g * under.a,
                                    under.b * under.a,
                                    under.a,
                                ],
                                viewport,
                            );
                            #[cfg(feature = "headless")]
                            if crate::__test_support::device_lost_now() {
                                gpu.backend.lose_device();
                            }
                            #[cfg(any(feature = "hot-reload", feature = "headless"))]
                            if self.profiler.is_on() {
                                let after = gpu.renderer.damage_stats();
                                for (count, value) in [
                                    (Count::DamagePx, after.damage_px),
                                    (Count::DamageRects, u64::from(after.damage_rects)),
                                    (
                                        Count::FullFrameFallbacks,
                                        after.full_frame_fallbacks - before.full_frame_fallbacks,
                                    ),
                                ] {
                                    self.profiler.count(count, value);
                                }
                            }
                        }
                    }
                }
                // The launch window's GPU stands for the frame's.
                #[cfg(any(feature = "hot-reload", feature = "headless"))]
                if let Some(gpu) = self.windows.first().and_then(|ws| ws.gpu.as_ref()) {
                    self.profiler.presented(gpu.backend.frame_epoch());
                }
            }
            FramePhase::PostFrameCleanup => {
                #[cfg(any(feature = "hot-reload", feature = "headless"))]
                if self.profiler.is_on() {
                    self.count_frame();
                }
                for ws in &mut self.windows {
                    // The nodes a full store refused this frame are not in the
                    // tree: said once a frame that refused any.
                    if let Some((full, refused)) = ws.store.take_refused() {
                        eprintln!(
                            "[viso] ArenaFull: the window's tree holds {} nodes ({} profile); {refused} past them were not created",
                            full.capacity,
                            footprint::Footprint::of_build().name()
                        );
                    }
                    // A timer the frame's own passes asked for (a scroll bar's
                    // fade) is armed now, so an idle loop wakes for it.
                    ws.store.take_timer_requests(&mut ws.timer_requests);
                    for req in ws.timer_requests.drain(..) {
                        ws.timers.arm_request(req, cx.frame_now());
                    }
                    // What the frame marked, for the inspector following the
                    // window.
                    ws.store.set_dirty_cause(DirtyCause::Other);
                    #[cfg(any(feature = "hot-reload", feature = "headless"))]
                    if let Some(journal) = &mut ws.journal {
                        journal.settle(&mut ws.store, &ws.states);
                    }
                    // The incremental passes have consumed this frame's
                    // invalidation; clear every node's dirty set so the next frame
                    // starts clean and an idle frame recomputes nothing.
                    ws.store.clear_dirty();
                    ws.style_seen = 0;
                    // Close the text frame: fold the glyph pages drawn this frame
                    // into page recency once, and zero the counters so each
                    // frame's trace reflects only that frame's shaping / raster /
                    // upload work (a steady-state repaint reads back all zeros).
                    if let Some(text) = &mut ws.text {
                        text.end_frame();
                    }
                    // Keep the loop beating while this window has live animations.
                    // `wants_animation` already tells the scheduler to stay in
                    // `Poll`, but on a headless backend `Poll` produces no beat on
                    // its own — a beat comes only from a `request_redraw`. So
                    // self-reschedule the next frame here whenever the registry is
                    // non-empty; the moment it empties (the last slide settled)
                    // this stops firing and, once every window is idle, the loop
                    // falls idle, holding the zero-CPU-when-idle contract.
                    // Text the worker still owes is committed by a later frame,
                    // so keep beating until it lands, and while the window owes
                    // one.
                    if ws.is_animating()
                        || ws.text_work_pending()
                        || viso_ui::work::inline_pending()
                        || ws.owes_frame()
                    {
                        cx.request_redraw(ws.window);
                    }
                }
                self.watch_budget(cx);
                self.drive_captures(cx);
                #[cfg(feature = "headless")]
                if let (Some(script), Some(ws)) = (&mut self.script, self.windows.first()) {
                    let mounts = self.harness.mounts();
                    let assets = &self.assets;
                    script.after_frame(
                        ws,
                        &|| crate::headless::census(ws, mounts),
                        Some(&|| crate::inspect::window_detail(ws, assets)),
                        cx,
                    );
                }
                #[cfg(any(feature = "hot-reload", feature = "headless"))]
                if let Some(player) = &mut self.player {
                    player.after_frame(cx);
                }
                #[cfg(feature = "hot-reload")]
                self.hot_reload.play(&self.windows, cx);
                #[cfg(any(feature = "hot-reload", feature = "headless"))]
                self.end_profiled_frame(cx);
            }
            _ => {}
        }
    }

    fn on_window_closed(&mut self, window: WindowId) {
        // A window closed — the OS close button or a programmatic
        // `RuntimeCx::close_window`, both arriving as the same `WindowClosed`
        // event, so this single hook is the one teardown path (no double
        // teardown). Cancel every scoped effect this window's tree owns first,
        // so each effect runs its cleanup (release resource) then drops, before
        // the store that indexed them is discarded — otherwise a scoped effect
        // would leak the resource it was holding. Then drop the whole
        // `WindowState`: its store/gpu/timers/animations destruct in turn, and
        // `GpuState`'s drop releases the surface/device. A hook for an unknown
        // window (already gone) finds nothing to retain and is a no-op.
        self.places.closed(window);
        if let Some(ws) = self.window_mut(window) {
            persist::suspend(&mut ws.store, &ws.states);
            ws.effects.cancel_all();
        }
        #[cfg(feature = "hot-reload")]
        self.hot_reload.close(window);
        #[cfg(any(feature = "hot-reload", feature = "headless"))]
        if let Some(recorder) = &mut self.recorder {
            recorder.closed(window);
        }
        self.windows.retain(|w| w.window != window);
    }

    fn wants_animation(&self) -> bool {
        // Any window with a non-empty registry has at least one node mid-slide,
        // so the scheduler should keep requesting frames (each window's flush
        // phase ticks its own registry). Each window's registry empties itself
        // as its animations finish; once every window is idle this returns
        // false and the loop is free to idle — the counterpart to the per-window
        // `request_redraw` self-reschedule in `PostFrameCleanup`.
        let scripted = {
            #[cfg(feature = "headless")]
            {
                self.script
                    .as_ref()
                    .is_some_and(crate::scenario::ScriptRunner::wants_frames)
            }
            #[cfg(not(feature = "headless"))]
            {
                false
            }
        } || {
            #[cfg(feature = "hot-reload")]
            {
                self.hot_reload.wants_frames()
            }
            #[cfg(not(feature = "hot-reload"))]
            {
                false
            }
        };
        // A capture being made lays out and settles over frames.
        scripted
            || !self.captures.is_empty()
            || self.windows.iter().any(|w| {
                w.is_animating()
                    || w.text_work_pending()
                    || viso_ui::work::inline_pending()
                    || w.owes_frame()
            })
    }

    fn next_timer_deadline(&self) -> Option<viso_runtime::Instant> {
        // The earliest live timer deadline across all windows (a toast's
        // auto-dismiss instant), or `None` when no window has a timer armed. The
        // scheduler turns `Some(deadline)` into a `ControlFlow::WaitUntil` so an
        // idle loop blocks until the soonest timer is due instead of spinning —
        // unlike `wants_animation`, this does *not* keep beating frames. Each
        // window's registry empties itself as its timers fire, so once the last
        // timer in the last window dismisses this returns `None` and the loop
        // falls fully idle (the zero-CPU-when-idle contract).
        self.windows
            .iter()
            .flat_map(|w| {
                w.timers
                    .earliest()
                    .into_iter()
                    .chain(w.caret_due)
                    .chain(w.animation_due)
                    .chain(w.video_step.due)
                    .chain(w.text_due)
                    .chain(w.lost_gpu.map(|l| l.due))
            })
            .chain(self.places.deadline())
            .min()
    }
}

/// Reports a reactive cycle `ws` raised: to the dev host as a diagnostic
/// naming its states and effects when a dev session runs, else on stderr.
fn report_cycle(
    #[cfg(feature = "hot-reload")] session: &mut crate::hot_reload::HotReloadSession,
    cycle: &viso_ui::ReactiveCycle,
    ws: &WindowState,
) {
    #[cfg(feature = "hot-reload")]
    if session.report_cycle(cycle, ws) {
        return;
    }
    #[cfg(not(feature = "hot-reload"))]
    let _ = ws;
    eprintln!("[viso] {cycle}");
}
