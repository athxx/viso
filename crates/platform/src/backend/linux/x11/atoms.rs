//! The atoms the X11 backend names, interned in one round trip.

x11rb::atom_manager! {
    pub(super) Atoms: AtomsCookie {
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        WM_CHANGE_STATE,
        UTF8_STRING,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_PING,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NORMAL,
        _NET_WM_MOVERESIZE,
        _MOTIF_WM_HINTS,
        CLIPBOARD,
        TARGETS,
        MULTIPLE,
        INCR,
        TIMESTAMP,
        TEXT,
        TEXT_PLAIN_UTF8: b"text/plain;charset=utf-8",
        TEXT_PLAIN: b"text/plain",
        VISO_SELECTION,
        ABS_PRESSURE: b"Abs Pressure",
    }
}
