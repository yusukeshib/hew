pub mod actions;
pub mod model;

pub use actions::diff;
// Re-exported for external callers / tests.
#[allow(unused_imports)]
pub use model::{Comment, CommentStore, LineRange, Thread};
