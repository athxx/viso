//! `viso-dev-protocol` — the development link between `viso` (the CLI) and a
//! dev runtime.
//!
//! The host and the runtime speak the framed messages of [`wire`]; a patch
//! carries the typed UI and game sections of [`patch`] and the asset blobs of
//! [`resource`]; a game answers the queries of [`query`]. What a running app
//! reports to a tool is data here too: its [`inspect`]ion census and node
//! details, its [`profile`] frames, the headless scenario a run follows
//! ([`headless`]) and the UI input [`tape`] it records or replays. The runtime
//! fills these in; the CLI reads them, and links no runtime to do so.

pub mod headless;
pub mod inspect;
pub mod patch;
pub mod profile;
pub mod query;
pub mod resource;
pub mod tape;
pub mod wire;
