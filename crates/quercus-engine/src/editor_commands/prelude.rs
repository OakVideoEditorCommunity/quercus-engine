//! Shared domain imports for editor command modules.

pub use std::path::Path;
pub use std::sync::{Arc, Mutex, MutexGuard};
pub use quercus_core::{Rational, TimeRange};
pub use quercus_node::block::{AdjustmentBlockBehavior, BlockCore, ClipBlockBehavior};
pub use quercus_node::folder::FolderBehavior;
pub use quercus_node::footage::FootageBehavior;
pub use quercus_node::graph::{Graph, NodeEntry};
pub use quercus_node::id::NodeId;
pub use quercus_node::project::Project;
pub use quercus_node::sequence::SequenceBehavior;
pub use quercus_node::track::{TrackBehavior, TrackListBehavior, TrackType};
pub use quercus_node::value::VideoParams;
pub use quercus_timeline::handle::CHandle;
pub use quercus_timeline::util::{
	block_in, block_length, block_out, block_set_in, block_set_length_and_media_out,
	block_set_length_keeping_out, clip_set_media_in, NodeRef,
};
pub use quercus_storage::backend::StorageBackend;
/// Shared ownership of the live project graph and its settings.
///
/// The mutex protects mutable project access; it does not replace the outer
/// editor-session lock that serializes session-level command state.
pub type ProjectRef = Arc<Mutex<Project>>;
