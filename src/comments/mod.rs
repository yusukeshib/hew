pub mod model;

// Re-exported for the (forthcoming) session server and external callers.
#[allow(unused_imports)]
pub use model::{Comment, CommentStore, LineRange, Thread};
