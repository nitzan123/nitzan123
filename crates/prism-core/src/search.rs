//! Structured catalog query. Translated to parameterised SQL by the catalog.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchQuery {
    /// Substring match on file name, caption and keywords.
    pub text: Option<String>,
    /// Only assets directly inside this folder.
    pub folder: Option<String>,
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
