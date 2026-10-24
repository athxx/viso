//! The videos a node store's nodes show.
//!
//! A video node is an asset node ([`Content::Asset`](crate::content::Content::Asset))
//! whose resource no decoder fills: the facade plays the video and hands the
//! table its size once known and each picture as a texture
//! ([`NodeStore::size_video`](crate::component::NodeStore::size_video),
//! [`NodeStore::show_video_frame`](crate::component::NodeStore::show_video_frame)).
//! It fits, tints, masks, shows its stand-ins and reports `loaded` / `failed`
//! as an image does.
//!
//! Beside it, the store's [`VideoTable`] keeps one entry a video node: where
//! it plays from, what the app asks of it ([`Playback`], a seek) and what the
//! player reports ([`VideoStatus`]). The node is the video's identity: showing
//! the same source again keeps it playing where it is; another source starts
//! a new resource, which the facade opens afresh.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use viso_render::asset::ResourceId;

use crate::asset::AssetBytes;
use crate::node::NodeId;

/// Where a video plays from.
#[derive(Debug, Clone)]
pub enum VideoSource {
    /// A file.
    File(Arc<Path>),
    /// A URL the platform's player fetches: `http(s)://`, an HLS playlist.
    Url(Arc<str>),
    /// A whole file's encoded bytes: embedded, or given at run time.
    Bytes(AssetBytes),
}

impl VideoSource {
    /// The video in the file at `path`.
    pub fn file(path: impl Into<PathBuf>) -> VideoSource {
        VideoSource::File(path.into().into())
    }

    /// The video at `url`.
    pub fn url(url: impl Into<String>) -> VideoSource {
        VideoSource::Url(url.into().into())
    }

    /// The video `bytes` hold, a whole file's.
    pub fn bytes(bytes: impl Into<Arc<[u8]>>) -> VideoSource {
        VideoSource::Bytes(AssetBytes::Shared(bytes.into()))
    }

    /// Whether it plays the same video as `other`: the same path or URL, or
    /// the same bytes — the same buffer at once, else byte for byte.
    pub fn same(&self, other: &VideoSource) -> bool {
        match (self, other) {
            (VideoSource::File(a), VideoSource::File(b)) => a == b,
            (VideoSource::Url(a), VideoSource::Url(b)) => a == b,
            (VideoSource::Bytes(a), VideoSource::Bytes(b)) => {
                (a.as_ptr() == b.as_ptr() && a.len() == b.len()) || **a == **b
            }
            _ => false,
        }
    }
}

impl From<crate::asset::Asset> for VideoSource {
    /// The video an asset holds: `asset!("…")` embeds its bytes.
    fn from(asset: crate::asset::Asset) -> VideoSource {
        match asset.source {
            crate::asset::AssetSource::Bytes(bytes) => VideoSource::Bytes(bytes),
            crate::asset::AssetSource::File(path) => VideoSource::File(path),
        }
    }
}

impl PartialEq for VideoSource {
    fn eq(&self, other: &VideoSource) -> bool {
        self.same(other)
    }
}

/// What the app asks of a video.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    /// Whether it plays; it ends paused at its end unless it loops.
    pub playing: bool,
    /// Whether it starts over at its end.
    pub looping: bool,
    /// Whether its sound is silenced.
    pub muted: bool,
    /// Its sound's volume, from 0 to 1.
    pub volume: f32,
    /// How many times its speed it plays at.
    pub rate: f32,
}

impl Default for Playback {
    /// Paused, once through, sounding at full volume and its own speed.
    fn default() -> Playback {
        Playback {
            playing: false,
            looping: false,
            muted: false,
            volume: 1.0,
            rate: 1.0,
        }
    }
}

/// What a video's player reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoStatus {
    /// Whether it opened and can play.
    pub ready: bool,
    /// Where it is.
    pub position: Duration,
    /// How long it plays; `None` before it is ready, and for a live stream.
    pub duration: Option<Duration>,
    /// Whether it plays.
    pub playing: bool,
    /// Whether it waits for data to play on.
    pub waiting: bool,
    /// Whether it played to its end and stopped there.
    pub ended: bool,
    /// Whether its sound is silenced.
    pub muted: bool,
    /// Its sound's volume, from 0 to 1.
    pub volume: f32,
}

impl Default for VideoStatus {
    /// Not yet open, at the start, sounding at full volume.
    fn default() -> VideoStatus {
        VideoStatus {
            ready: false,
            position: Duration::ZERO,
            duration: None,
            playing: false,
            waiting: false,
            ended: false,
            muted: false,
            volume: 1.0,
        }
    }
}

/// One video node.
#[derive(Debug, Clone)]
pub struct VideoEntry {
    pub node: NodeId,
    /// The asset resource its pictures are shown as.
    pub resource: ResourceId,
    pub source: VideoSource,
    pub playback: Playback,
    /// What the node's build last asked for: a build asking the same again
    /// leaves alone what was asked since.
    pub declared: Option<Playback>,
    /// A seek asked for and not yet made.
    pub seek: Option<Duration>,
    pub status: VideoStatus,
}

/// The resources video nodes are shown as, apart from any asset's.
const VIDEO: u64 = 0x71de_0000_0000_0000;

/// The video nodes of one store, one entry a node. Cold: a frame visits it
/// only where a video plays.
#[derive(Debug, Default)]
pub struct VideoTable {
    entries: Vec<VideoEntry>,
}

impl VideoTable {
    /// Node `node`'s video.
    pub fn get(&self, node: NodeId) -> Option<&VideoEntry> {
        self.entries.iter().find(|e| e.node == node)
    }

    /// Node `node`'s video, to report to or take requests from.
    pub fn get_mut(&mut self, node: NodeId) -> Option<&mut VideoEntry> {
        self.entries.iter_mut().find(|e| e.node == node)
    }

    /// The video shown as `resource`.
    pub fn of(&self, resource: ResourceId) -> Option<&VideoEntry> {
        self.entries.iter().find(|e| e.resource == resource)
    }

    pub fn iter(&self) -> impl Iterator<Item = &VideoEntry> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut VideoEntry> {
        self.entries.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Node `node`'s video playing `source`: the one it has when it plays the
    /// same, else a new entry with a resource of its own, paused at the
    /// start. Returns the entry's resource and whether it is new.
    pub(crate) fn show(&mut self, node: NodeId, source: &VideoSource) -> (ResourceId, bool) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        if let Some(entry) = self.get(node) {
            if entry.source.same(source) {
                return (entry.resource, false);
            }
            self.forget(node);
        }
        let resource = ResourceId(VIDEO | NEXT.fetch_add(1, Ordering::Relaxed));
        self.entries.push(VideoEntry {
            node,
            resource,
            source: source.clone(),
            playback: Playback::default(),
            declared: None,
            seek: None,
            status: VideoStatus::default(),
        });
        (resource, true)
    }

    /// Puts back an entry taken out with [`forget`](Self::forget).
    pub(crate) fn restore(&mut self, entry: VideoEntry) {
        self.forget(entry.node);
        self.entries.push(entry);
    }

    /// Lets go of node `node`'s video.
    pub(crate) fn forget(&mut self, node: NodeId) -> Option<VideoEntry> {
        let at = self.entries.iter().position(|e| e.node == node)?;
        Some(self.entries.swap_remove(at))
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::{AssetDraw, AssetFit};
    use crate::component::{BuildCx, NodeStore};
    use crate::content::Content;
    use crate::notice::{Notice, NoticeKinds, deliver_notices};
    use crate::reactive::EffectStore;
    use crate::state::StateStore;
    use crate::virtual_list::VirtualLists;
    use crate::{BoxStyle, LeafStyle, Size};
    use std::cell::RefCell;
    use std::rc::Rc;
    use viso_render::TextureId;

    /// A store of one 40 × 30 leaf under a root, and the leaf.
    fn leaf() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let mut leaf = None;
        {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(Default::default(), |cx| {
                leaf = Some(
                    cx.leaf(LeafStyle {
                        size: Size::fixed(40.0, 30.0),
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            });
        }
        (store, leaf.unwrap())
    }

    fn shown(store: &NodeStore, node: NodeId) -> (AssetDraw, [f32; 2]) {
        match store.content_payload(node) {
            Some(Content::Asset { draw, natural, .. }) => (draw.clone(), [natural.x, natural.y]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_video_node_shows_its_pictures_as_the_facade_provides_them() {
        let (mut store, node) = leaf();
        let clip = VideoSource::file("clip.mp4");
        store.set_video(node, &clip, AssetFit::Contain, None);
        let resource = store.videos().get(node).unwrap().resource;
        assert_eq!(shown(&store, node), (AssetDraw::Nothing, [0.0; 2]));
        assert_eq!(store.playback(node), Some(Playback::default()));
        assert!(
            store.assets().wanted().next().is_none(),
            "no bytes are looked for"
        );

        store.size_video(resource, 64, 48);
        assert_eq!(shown(&store, node), (AssetDraw::Nothing, [64.0, 48.0]));
        assert_eq!(
            store.show_video_frame(resource, TextureId::new(7), (64, 48)),
            None
        );
        assert_eq!(
            shown(&store, node),
            (AssetDraw::Raster(TextureId::new(7)), [64.0, 48.0])
        );
        assert_eq!(store.assets().texture_bytes(), 64 * 48 * 4);
        // A sweep leaves what the facade provides alone.
        assert_eq!(store.assets_mut().evict_unshown(|_| panic!()), 0);

        // The same source again keeps the video where it is.
        store.set_video_playing(node, true);
        store.set_video(node, &VideoSource::file("clip.mp4"), AssetFit::Cover, None);
        let entry = store.videos().get(node).unwrap();
        assert_eq!(entry.resource, resource);
        assert!(entry.playback.playing);
        assert_eq!(store.assets().users(resource), 1);

        // Another starts over as a new resource; the old is the facade's to
        // forget.
        store.set_video(node, &VideoSource::file("other.mp4"), AssetFit::Cover, None);
        let other = store.videos().get(node).unwrap().resource;
        assert_ne!(other, resource);
        assert!(!store.videos().get(node).unwrap().playback.playing);
        assert_eq!(store.forget_video(resource), Some(TextureId::new(7)));
        assert!(!store.assets().knows(resource));
        assert_eq!(store.assets().texture_bytes(), 0);

        // A node freed lets go of its video.
        let (mut effects, mut scratch) = (EffectStore::default(), Vec::new());
        store.free_tree(node, &mut effects, &mut scratch);
        assert!(store.videos().is_empty());
        assert_eq!(store.forget_video(other), None);
        assert!(!store.assets().knows(other));
    }

    #[test]
    fn a_watcher_hears_each_change_of_status_its_end_once_its_playing_and_a_failure() {
        let (mut store, node) = leaf();
        store.set_video(
            node,
            &VideoSource::url("https://x/live.m3u8"),
            AssetFit::Contain,
            None,
        );
        let resource = store.videos().get(node).unwrap().resource;
        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = Rc::clone(&heard);
        store.watch(
            node,
            NoticeKinds::VIDEO | NoticeKinds::ASSET,
            Box::new(move |notice: &Notice, _: &mut StateStore| log.borrow_mut().push(*notice)),
        );
        let (lists, mut states) = (VirtualLists::new(), StateStore::new());
        deliver_notices(&mut store, &lists, &mut states);
        assert_eq!(*heard.borrow(), [Notice::Video(VideoStatus::default())]);

        let mut status = VideoStatus {
            ready: true,
            playing: true,
            ..VideoStatus::default()
        };
        store.videos_mut().get_mut(node).unwrap().status = status;
        deliver_notices(&mut store, &lists, &mut states);
        deliver_notices(&mut store, &lists, &mut states);
        status.ended = true;
        status.playing = false;
        store.videos_mut().get_mut(node).unwrap().status = status;
        deliver_notices(&mut store, &lists, &mut states);
        status.position = Duration::from_secs(1);
        store.videos_mut().get_mut(node).unwrap().status = status;
        deliver_notices(&mut store, &lists, &mut states);
        assert_eq!(heard.borrow()[2], Notice::VideoPlaying(true));
        assert_eq!(heard.borrow()[4], Notice::VideoEnded, "once");
        assert_eq!(heard.borrow()[5], Notice::VideoPlaying(false));
        assert_eq!(heard.borrow().len(), 7);
        assert!(!heard.borrow()[6..].contains(&Notice::VideoEnded));

        heard.borrow_mut().clear();
        assert_eq!(store.fail_video(resource), None);
        assert_eq!(shown(&store, node).0, AssetDraw::Failed);
        deliver_notices(&mut store, &lists, &mut states);
        assert_eq!(*heard.borrow(), [Notice::AssetFailed]);
    }

    #[test]
    fn a_source_is_the_same_by_path_url_or_bytes() {
        let bytes: Arc<[u8]> = Arc::from(&b"clip"[..]);
        assert!(VideoSource::file("a.mp4").same(&VideoSource::file("a.mp4")));
        assert!(!VideoSource::file("a.mp4").same(&VideoSource::file("b.mp4")));
        assert!(VideoSource::url("https://x/a").same(&VideoSource::url("https://x/a")));
        assert!(!VideoSource::url("a.mp4").same(&VideoSource::file("a.mp4")));
        let shared = VideoSource::Bytes(AssetBytes::Shared(Arc::clone(&bytes)));
        assert!(shared.same(&VideoSource::Bytes(AssetBytes::Shared(bytes))));
        assert!(shared.same(&VideoSource::Bytes(AssetBytes::Static(b"clip"))));
        assert!(!shared.same(&VideoSource::Bytes(AssetBytes::Static(b"clap"))));
    }
}
