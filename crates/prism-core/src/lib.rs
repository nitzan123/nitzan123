//! Prism Studio core: catalog, scanning, metadata, thumbnails, similarity
//! and the non-destructive develop engine. UI-agnostic; the Tauri shell in
//! `src-tauri` is a thin layer over this crate.

pub mod catalog;
pub mod develop;
pub mod error;
pub mod geocode;
pub mod imaging;
pub mod metadata;
pub mod overlay;
pub mod raw;
pub mod scan;
pub mod search;
pub mod similarity;
pub mod thumbs;
pub mod xmp;

pub use catalog::{Asset, Catalog};
pub use develop::DevelopSettings;
pub use error::{Error, Result};
pub use search::SearchQuery;
