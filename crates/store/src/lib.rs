pub mod clipboard;
#[cfg(feature = "history")]
pub mod history;
pub mod local;
pub mod store_factory;
#[cfg(feature = "webdav")]
pub mod webdav;

pub use store_factory::{BackendKind, StorageOptions, build_storage};
#[cfg(feature = "history")]
pub use history::HistoryRepo;
