//! The record of every `view!` mount, kept for the development session that
//! hot-reloads it.
//!
//! Under the `hot-reload` feature a `view!` expansion records, once its tree is
//! built, the identities a reload needs to reach the mount: the `.vs` file and
//! the hash of the source it was compiled from, the module identity, catalogs,
//! grants and compiler schema it compiles under, the mounted root and static
//! nodes, the state cells it allocated and the behavior host it dispatches
//! into. No source text is embedded: the host holds it. The records queue on
//! the UI thread until the session [takes](take_mounts) them. Without the
//! feature `__record_mount!` expands to nothing, so a release binary carries
//! no record.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::state::StateKey;
use viso_ui::{NodeId, StateId};

use crate::ViewHost;

/// One `view!` mount.
pub struct MountRecord {
    /// The canonical path of the `.vs` file.
    pub file: &'static str,
    /// The [`source_hash`](crate::dev::wire::source_hash) of the source the
    /// mount was compiled from.
    pub source_hash: u64,
    /// The package the file compiles in.
    pub package: &'static str,
    /// The file's module path within the package.
    pub module: &'static [&'static str],
    /// The language edition the package declares.
    pub language: Option<&'static str>,
    /// The package's message catalogs, its source locale and the directory
    /// holding them, when it has any.
    pub catalog: Option<(&'static str, &'static str)>,
    /// The capabilities the package grants, which the build checked the file
    /// against.
    pub capabilities: &'static [&'static str],
    /// The fingerprint of the compiler schema the build compiled the file with.
    pub schema: u128,
    /// The number of static children of each static node, in pre-order: the
    /// shape a view without regions names its static nodes by.
    pub statics: &'static [u32],
    /// The mounted view's root.
    pub root: NodeId,
    /// Each source the view reads, by its durable key, and the cell holding it.
    pub cells: Vec<(StateKey, StateId)>,
    /// The behavior host the view's nodes dispatch into, if it has behavior.
    pub host: Option<Rc<RefCell<ViewHost>>>,
    /// The live node of each static node of a view with regions, which
    /// interleave with the static nodes so the tree alone does not name them,
    /// or with `env` reads, which anchor at them, by static index; empty for
    /// a view with neither, whose static nodes [`statics`](Self::statics)
    /// names.
    pub nodes: Vec<Option<NodeId>>,
}

thread_local! {
    static MOUNTS: RefCell<Vec<MountRecord>> = const { RefCell::new(Vec::new()) };
}

/// Queues `record` for the development session.
#[doc(hidden)]
pub fn __mounted_view(record: MountRecord) {
    MOUNTS.with(|mounts| mounts.borrow_mut().push(record));
}

/// Moves every mount recorded on this thread since the last call into `out`, in
/// mount order.
pub fn take_mounts(out: &mut Vec<MountRecord>) {
    MOUNTS.with(|mounts| out.append(&mut mounts.borrow_mut()));
}

/// Records a `view!` mount for the development session.
#[cfg(feature = "hot-reload")]
#[doc(hidden)]
#[macro_export]
macro_rules! __record_mount {
    (
        root: $root:expr,
        file: $file:expr,
        package: $package:expr,
        module: [$($module:expr),* $(,)?],
        language: $language:expr,
        catalog: $catalog:expr,
        capabilities: $capabilities:expr,
        schema: $schema:expr,
        source_hash: $source_hash:expr,
        statics: $statics:expr,
        cells: [$($cell:expr),* $(,)?],
        host: $host:expr,
        nodes: $nodes:expr $(,)?
    ) => {
        $crate::__mounted_view($crate::MountRecord {
            file: $file,
            source_hash: $source_hash,
            package: $package,
            module: &[$($module),*],
            language: $language,
            catalog: $catalog,
            capabilities: $capabilities,
            schema: $schema,
            statics: $statics,
            root: $root,
            cells: ::std::vec![$($cell),*],
            host: $host,
            nodes: $nodes,
        })
    };
}
