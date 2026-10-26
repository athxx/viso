//! The navigator's offline data, as `viso-map-build` writes it: the base
//! map (`<prefix>.pmtiles`, Shortbread vector tiles), the place search
//! index (`<prefix>.search`) and the road graph (`<prefix>.graph`).
//!
//! On the desktop and on phones they are files: `<prefix>` is
//! `VISO_MAP_DATA`, or the first argument, or else `luxembourg` in the
//! working directory on the desktop, in the app's `Documents` on iOS and in
//! its external files directory on Android (`adb push` reaches it). On the
//! web they are fetched from beside the page. Each loads without
//! blocking the loop: the archive's directory is read on `viso-tiles`' I/O
//! threads, and the index and the graph are read and parsed on the worker
//! pool.

use std::sync::Arc;

use viso_map::{ServiceError, TileId, TileSource};
use viso_map_nav::{RouteGraph, SearchIndex};
use viso_tiles::{Pmtiles, TileSource as _};

/// Where the data is: a path prefix, or a URL prefix on the web.
pub fn prefix() -> String {
    std::env::var("VISO_MAP_DATA")
        .ok()
        .or_else(|| std::env::args().nth(1))
        .unwrap_or_else(beside_app)
}

/// The data's place when nothing names it.
fn beside_app() -> String {
    const NAME: &str = "luxembourg";
    #[cfg(target_arch = "wasm32")]
    {
        // The page's directory: what a relative name means to the page.
        let href = web_sys::window()
            .and_then(|w| w.location().href().ok())
            .unwrap_or_default();
        let page = href.split(['?', '#']).next().unwrap_or_default();
        let dir = page.rsplit_once('/').map_or(page, |(dir, _)| dir);
        format!("{dir}/{NAME}")
    }
    #[cfg(target_os = "ios")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{home}/Documents/{NAME}")
    }
    #[cfg(target_os = "android")]
    {
        // The process is named after the package.
        let cmdline = std::fs::read("/proc/self/cmdline").unwrap_or_default();
        let package =
            String::from_utf8_lossy(cmdline.split(|&b| b == 0).next().unwrap_or(&[])).into_owned();
        format!("/storage/emulated/0/Android/data/{package}/files/{NAME}")
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "ios", target_os = "android")))]
    {
        NAME.to_owned()
    }
}

/// The base map, opened.
pub async fn tiles(prefix: &str) -> Result<Arc<Pmtiles>, String> {
    let path = format!("{prefix}.pmtiles");
    #[cfg(not(target_arch = "wasm32"))]
    let opened = Pmtiles::open(&path).await;
    #[cfg(target_arch = "wasm32")]
    let opened = Pmtiles::open_url(&viso_net::Client::new(), path.clone()).await;
    opened.map(Arc::new).map_err(|e| format!("{path}: {e}"))
}

/// The archive's tiles as the map view asks for them; a request the view
/// drops is dropped in the archive's queue too.
pub fn source(archive: Arc<Pmtiles>) -> impl TileSource {
    move |t: TileId| {
        let reply = archive.tile(t.z, t.x, t.y);
        async move { reply.await.map_err(|e| ServiceError::Failed(e.to_string())) }
    }
}

/// The bytes of `<prefix>.<ext>`.
async fn bytes(prefix: &str, ext: &str) -> Result<Vec<u8>, String> {
    let path = format!("{prefix}.{ext}");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let p = path.clone();
        off_loop(move || std::fs::read(&p).map_err(|e| e.to_string()))
            .await
            .map_err(|e| format!("{path}: {e}"))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let response = viso_net::Client::new()
            .get(&path)
            .send()
            .await
            .map_err(|e| format!("{path}: {e}"))?;
        response.bytes().await.map_err(|e| format!("{path}: {e}"))
    }
}

/// `job` run on the worker pool, its answer awaited on the loop.
pub async fn off_loop<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (done, answer) = viso_map::reply();
    viso::ui::work::submit(move || done.complete(Ok(job())));
    answer.await.map_err(|e| e.to_string())?
}

/// The place search index, loaded.
pub async fn search(prefix: &str) -> Result<SearchIndex, String> {
    let bytes = bytes(prefix, "search").await?;
    off_loop(move || SearchIndex::from_bytes(&bytes).map_err(|e| e.to_string())).await
}

/// The road graph, loaded.
pub async fn graph(prefix: &str) -> Result<RouteGraph, String> {
    let bytes = bytes(prefix, "graph").await?;
    off_loop(move || RouteGraph::from_bytes(&bytes).map_err(|e| e.to_string())).await
}
