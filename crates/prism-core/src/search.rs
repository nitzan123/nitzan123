//! Structured catalog query. Translated to parameterised SQL by the catalog.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchQuery {
    /// Substring match on file name, caption and keywords.
    pub text: Option<String>,
    /// Only assets directly inside this folder.
    pub folder: Option<String>,
    /// Assets in this folder or any folder below it.
    pub folder_tree: Option<String>,
    /// Only photos with (`true`) or without (`false`) a GPS position.
    pub has_gps: Option<bool>,
    /// Only photos whose text needs attention — no place yet, or an
    /// inferred place/date to confirm (`true`) — or the rest.
    pub needs_place: Option<bool>,
    /// Only photos with (`true`) or without (`false`) a capture date.
    pub dated: Option<bool>,
    pub min_rating: Option<u8>,
    /// Exact colour label (1..=5). 0 means "no label".
    pub color_label: Option<u8>,
    pub tagged: Option<bool>,
    pub keyword: Option<String>,
    pub collection_id: Option<i64>,
    /// Photos containing this person (AI face recognition).
    pub person_id: Option<i64>,
    /// Inclusive `YYYY-MM-DD` bounds on capture date.
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    pub camera: Option<String>,
    pub developed: Option<bool>,
    pub sort: SortKey,
    pub descending: bool,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SortKey {
    #[default]
    Name,
    TakenAt,
    Rating,
    Size,
    Modified,
}

impl SortKey {
    pub fn column(self) -> &'static str {
        match self {
            SortKey::Name => "a.file_name COLLATE NOCASE",
            SortKey::TakenAt => "COALESCE(a.taken_at, '')",
            SortKey::Rating => "a.rating",
            SortKey::Size => "a.size",
            SortKey::Modified => "a.mtime",
        }
    }
}
