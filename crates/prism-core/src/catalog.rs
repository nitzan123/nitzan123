//! SQLite catalog: assets, folders, keywords, collections, saved searches and
//! develop settings. All writes are single statements or transactions so the
//! file is safe to share between the UI thread and the indexer.

use crate::develop::DevelopSettings;
use crate::error::{Error, Result};
use crate::scan::ScannedFile;
use crate::search::SearchQuery;
use crate::similarity::hamming;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS folders (
    id   INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS assets (
    id            INTEGER PRIMARY KEY,
    folder_id     INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    path          TEXT NOT NULL UNIQUE,
    file_name     TEXT NOT NULL,
    ext           TEXT NOT NULL,
    size          INTEGER NOT NULL,
    mtime         INTEGER NOT NULL,
    width         INTEGER,
    height        INTEGER,
    taken_at      TEXT,
    camera_make   TEXT,
    camera_model  TEXT,
    lens          TEXT,
    iso           INTEGER,
    f_number      REAL,
    exposure_time REAL,
    focal_length  REAL,
    orientation   INTEGER NOT NULL DEFAULT 1,
    rating        INTEGER NOT NULL DEFAULT 0,
    color_label   INTEGER NOT NULL DEFAULT 0,
    tagged        INTEGER NOT NULL DEFAULT 0,
    caption       TEXT,
    phash         INTEGER,
    develop       TEXT,
    gps_lat       REAL,
    gps_lon       REAL,
    -- 1 once GPS was read from the file (catalogs from v1 are backfilled).
    gps_scanned   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS assets_folder ON assets(folder_id);
CREATE INDEX IF NOT EXISTS assets_taken ON assets(taken_at);
CREATE INDEX IF NOT EXISTS assets_rating ON assets(rating);
CREATE TABLE IF NOT EXISTS keywords (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE COLLATE NOCASE
);
CREATE TABLE IF NOT EXISTS asset_keywords (
    asset_id   INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    keyword_id INTEGER NOT NULL REFERENCES keywords(id) ON DELETE CASCADE,
    PRIMARY KEY (asset_id, keyword_id)
);
CREATE TABLE IF NOT EXISTS collections (
    id   INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS collection_assets (
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    asset_id      INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    PRIMARY KEY (collection_id, asset_id)
);
CREATE TABLE IF NOT EXISTS ai_analysis (
    asset_id  INTEGER PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
    mtime     INTEGER NOT NULL,
    embedding BLOB
);
CREATE TABLE IF NOT EXISTS ai_tags (
    asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    label    TEXT NOT NULL,
    score    REAL NOT NULL,
    PRIMARY KEY (asset_id, label)
);
CREATE TABLE IF NOT EXISTS persons (
    id   INTEGER PRIMARY KEY,
    name TEXT
);
CREATE TABLE IF NOT EXISTS faces (
    id        INTEGER PRIMARY KEY,
    asset_id  INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    x REAL NOT NULL, y REAL NOT NULL, w REAL NOT NULL, h REAL NOT NULL,
    score     REAL NOT NULL,
    embedding BLOB NOT NULL,
    person_id INTEGER REFERENCES persons(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS faces_asset ON faces(asset_id);
CREATE INDEX IF NOT EXISTS faces_person ON faces(person_id);
CREATE TABLE IF NOT EXISTS places (
    key  TEXT PRIMARY KEY,
    name TEXT
);
CREATE TABLE IF NOT EXISTS roots (
    path      TEXT PRIMARY KEY,
    recursive INTEGER NOT NULL DEFAULT 1
);
-- Folders removed from the library while a parent is still a library root:
-- the parent's watcher and rescans skip them.
CREATE TABLE IF NOT EXISTS excluded (
    path TEXT PRIMARY KEY
);
CREATE TABLE IF NOT EXISTS track_points (
    t   INTEGER NOT NULL,
    lat REAL NOT NULL,
    lon REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS track_points_t ON track_points(t);
CREATE TABLE IF NOT EXISTS presets (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL UNIQUE COLLATE NOCASE,
    settings TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS saved_searches (
    id    INTEGER PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE,
    query TEXT NOT NULL
);
"#;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: i64,
    pub path: String,
    pub folder: String,
    pub file_name: String,
    pub ext: String,
    pub size: i64,
    pub mtime: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub taken_at: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub f_number: Option<f32>,
    pub exposure_time: Option<f32>,
    pub focal_length: Option<f32>,
    pub orientation: u16,
    pub rating: u8,
    pub color_label: u8,
    pub tagged: bool,
    pub caption: Option<String>,
    pub developed: bool,
    /// `[latitude, longitude]` in degrees, from EXIF.
    pub gps: Option<[f64; 2]>,
    /// Has text on the photo (date, place, …) in its develop settings.
    pub has_text: bool,
    /// Has text whose place is still empty (no GPS / lookup failed).
    pub needs_place: bool,
    /// Its place or date was inferred and is waiting to be confirmed.
    pub to_check: bool,
}

/// SQL: the photo has text but no place yet.
macro_rules! place_missing_sql {
    () => {
        "(json_extract(a.develop, '$.text') IS NOT NULL \
     AND trim(coalesce(json_extract(a.develop, '$.text.place'), '')) = '')"
    };
}
/// SQL: the photo's place or date was inferred and hasn't been confirmed.
macro_rules! to_check_sql {
    () => {
        "coalesce((json_extract(a.develop, '$.text') IS NOT NULL AND ( \
     (trim(coalesce(json_extract(a.develop, '$.text.place'), '')) != '' \
      AND json_extract(a.develop, '$.text.placeFrom') IN ('nearby', 'phone', 'timeline')) \
  OR (trim(coalesce(json_extract(a.develop, '$.text.date'), '')) != '' \
      AND json_extract(a.develop, '$.text.dateFrom') IN ('fileName', 'fileDate')))), 0)"
    };
}

const ASSET_COLUMNS: &str = concat!(
    "a.id, a.path, f.path, a.file_name, a.ext, a.size, a.mtime, a.width, a.height, \
    a.taken_at, a.camera_make, a.camera_model, a.lens, a.iso, a.f_number, a.exposure_time, a.focal_length, \
    a.orientation, a.rating, a.color_label, a.tagged, a.caption, a.develop IS NOT NULL, a.gps_lat, a.gps_lon, \
    json_extract(a.develop, '$.text') IS NOT NULL, ",
    place_missing_sql!(),
    ", ",
    to_check_sql!()
);

fn asset_from_row(r: &Row<'_>) -> rusqlite::Result<Asset> {
    Ok(Asset {
        id: r.get(0)?,
        path: r.get(1)?,
        folder: r.get(2)?,
        file_name: r.get(3)?,
        ext: r.get(4)?,
        size: r.get(5)?,
        mtime: r.get(6)?,
        width: r.get(7)?,
        height: r.get(8)?,
        taken_at: r.get(9)?,
        camera_make: r.get(10)?,
        camera_model: r.get(11)?,
        lens: r.get(12)?,
        iso: r.get(13)?,
        f_number: r.get(14)?,
        exposure_time: r.get(15)?,
        focal_length: r.get(16)?,
        orientation: r.get(17)?,
        rating: r.get(18)?,
        color_label: r.get(19)?,
        tagged: r.get(20)?,
        caption: r.get(21)?,
        developed: r.get(22)?,
        gps: match (r.get::<_, Option<f64>>(23)?, r.get::<_, Option<f64>>(24)?) {
            (Some(lat), Some(lon)) => Some([lat, lon]),
            _ => None,
        },
        has_text: r.get(25)?,
        needs_place: r.get(26)?,
        to_check: r.get(27)?,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RemovedFolder {
    /// Photos taken out of the catalog.
    pub photos: usize,
    /// Library roots that were inside the folder (stop watching them).
    pub unwatched: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub id: i64,
    pub path: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NamedCount {
    pub id: i64,
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedSearch {
    pub id: i64,
    pub name: String,
    pub query: SearchQuery,
}

/// AI results for one asset, written by `store_analysis`.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub asset_id: i64,
    pub mtime: i64,
    pub embedding: Option<Vec<f32>>,
    pub tags: Vec<(String, f32)>,
    pub faces: Vec<AnalyzedFace>,
}

#[derive(Debug, Clone)]
pub struct AnalyzedFace {
    /// x, y, w, h normalised to the (orientation-corrected) image, 0…1.
    pub rect: [f32; 4],
    pub score: f32,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FaceRow {
    pub id: i64,
    pub asset_id: i64,
    pub rect: [f32; 4],
    pub person_id: Option<i64>,
    pub person_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersonRow {
    pub id: i64,
    pub name: Option<String>,
    pub photos: i64,
    pub cover_face: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiCounts {
    pub analyzed: i64,
    pub total: i64,
    pub faces: i64,
    pub people: i64,
}

/// What `upsert_scanned` did with a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Inserted(i64),
    Updated(i64),
    Unchanged(i64),
}

impl Upsert {
    pub fn id(self) -> i64 {
        match self {
            Upsert::Inserted(i) | Upsert::Updated(i) | Upsert::Unchanged(i) => i,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DayCount {
    pub day: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub days: Vec<DayCount>,
    pub undated: i64,
}

pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v < SCHEMA_VERSION {
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        }
        Ok(Self { conn })
    }

    // ------------------------------------------------------------------ roots

    /// Records a folder the user added to the library (watched for changes).
    /// Adding it back un-does an earlier removal of it or anything below it.
    pub fn add_root(&self, path: &str, recursive: bool) -> Result<()> {
        let gone: Vec<String> = self.excluded()?.into_iter().filter(|e| at_or_below(e, path)).collect();
        for e in gone {
            self.conn.execute("DELETE FROM excluded WHERE path = ?1", [e])?;
        }
        self.conn.execute(
            "INSERT INTO roots(path, recursive) VALUES (?1, ?2) \
             ON CONFLICT(path) DO UPDATE SET recursive = max(recursive, excluded.recursive)",
            params![path, recursive],
        )?;
        Ok(())
    }

    /// Library roots, without ones already covered by a recursive parent.
    pub fn roots(&self) -> Result<Vec<(String, bool)>> {
        let mut st = self.conn.prepare("SELECT path, recursive FROM roots ORDER BY path")?;
        let all: Vec<(String, bool)> =
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        let recursive: Vec<String> = all.iter().filter(|r| r.1).map(|r| r.0.clone()).collect();
        Ok(all
            .into_iter()
            .filter(|(p, _)| {
                !recursive.iter().any(|o| {
                    let o = o.trim_end_matches(['/', '\\']);
                    o != p.as_str() && p.strip_prefix(o).is_some_and(|rest| rest.starts_with(['/', '\\']))
                })
            })
            .collect())
    }

    /// Removes `path` and everything below it from the library: catalog
    /// rows (ratings, edits, faces…) go, the files on disk are not touched.
    /// Library roots inside it are dropped; if it sits inside a root that
    /// stays, it is remembered as excluded so rescans don't bring it back.
    pub fn remove_folder(&mut self, path: &str) -> Result<RemovedFolder> {
        let path = path.trim_end_matches(['/', '\\']).to_string();
        let ids: Vec<i64> = self.assets_under(&path, true)?.into_iter().map(|(id, _)| id).collect();
        let all_roots: Vec<(String, bool)> = {
            let mut st = self.conn.prepare("SELECT path, recursive FROM roots")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let (dropped, kept): (Vec<_>, Vec<_>) = all_roots.into_iter().partition(|(r, _)| at_or_below(r, &path));
        let covered = kept.iter().any(|(r, recursive)| *recursive && at_or_below(&path, r));
        let tx = self.conn.transaction()?;
        {
            let mut del = tx.prepare_cached("DELETE FROM assets WHERE id = ?1")?;
            for id in &ids {
                del.execute([id])?;
            }
            for (r, _) in &dropped {
                tx.execute("DELETE FROM roots WHERE path = ?1", [r])?;
            }
            if covered {
                tx.execute("INSERT OR IGNORE INTO excluded(path) VALUES (?1)", [&path])?;
            }
            // Empty folder rows would otherwise linger in the folder list.
            tx.execute("DELETE FROM folders WHERE id NOT IN (SELECT DISTINCT folder_id FROM assets)", [])?;
        }
        tx.commit()?;
        Ok(RemovedFolder { photos: ids.len(), unwatched: dropped.into_iter().map(|(r, _)| r).collect() })
    }

    /// Folders removed from the library inside a root that is still watched.
    pub fn excluded(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT path FROM excluded")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---------------------------------------------------------------- folders

    pub fn folder_id(&self, path: &str) -> Result<i64> {
        folder_id_on(&self.conn, path)
    }

    pub fn folders(&self) -> Result<Vec<FolderInfo>> {
        let mut st = self.conn.prepare(
            "SELECT f.id, f.path, COUNT(a.id) FROM folders f LEFT JOIN assets a ON a.folder_id = f.id \
             GROUP BY f.id HAVING COUNT(a.id) > 0 ORDER BY f.path COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| Ok(FolderInfo { id: r.get(0)?, path: r.get(1)?, count: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ----------------------------------------------------------------- assets

    pub fn upsert_scanned(&self, f: &ScannedFile) -> Result<Upsert> {
        upsert_on(&self.conn, f)
    }

    /// Inserts/updates a whole scan in one transaction.
    pub fn upsert_many(&mut self, files: &[ScannedFile]) -> Result<Vec<Upsert>> {
        let tx = self.conn.transaction()?;
        let res = files.iter().map(|f| upsert_on(&tx, f)).collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(res)
    }

    /// Removes rows for files that disappeared anywhere at or below `root`
    /// (only `root` itself when not `recursive`), including folders that
    /// were deleted or emptied entirely.
    pub fn prune_under(&self, root: &str, recursive: bool) -> Result<usize> {
        let gone: Vec<i64> = self
            .assets_under(root, recursive)?
            .into_iter()
            .filter(|(_, p)| !Path::new(p).exists())
            .map(|(id, _)| id)
            .collect();
        self.delete_assets(&gone)?;
        Ok(gone.len())
    }

    /// `(id, path)` of every asset at or below `root` (only directly in it
    /// when not `recursive`). Lets callers check the disk without holding
    /// the catalog lock.
    pub fn assets_under(&self, root: &str, recursive: bool) -> Result<Vec<(i64, String)>> {
        let root = root.trim_end_matches(['/', '\\']);
        let mut st =
            self.conn.prepare("SELECT a.id, a.path, f.path FROM assets a JOIN folders f ON f.id = a.folder_id")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, path, folder) = row?;
            let below = folder.strip_prefix(root).is_some_and(|rest| rest.starts_with(['/', '\\']));
            if folder == root || (recursive && below) {
                out.push((id, path));
            }
        }
        Ok(out)
    }

    pub fn delete_assets(&self, ids: &[i64]) -> Result<()> {
        let mut st = self.conn.prepare_cached("DELETE FROM assets WHERE id = ?1")?;
        for id in ids {
            st.execute([id])?;
        }
        Ok(())
    }

    /// Removes catalog rows whose files disappeared from `folder`.
    pub fn prune_missing(&self, folder: &str) -> Result<usize> {
        let mut st = self
            .conn
            .prepare("SELECT a.id, a.path FROM assets a JOIN folders f ON f.id = a.folder_id WHERE f.path = ?1")?;
        let gone: Vec<i64> = st
            .query_map([folder], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|r| r.ok())
            .filter(|(_, p)| !Path::new(p).exists())
            .map(|(id, _)| id)
            .collect();
        for id in &gone {
            self.conn.execute("DELETE FROM assets WHERE id = ?1", [id])?;
        }
        Ok(gone.len())
    }

    pub fn asset_by_path(&self, path: &str) -> Result<Option<Asset>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {ASSET_COLUMNS} FROM assets a JOIN folders f ON f.id = a.folder_id WHERE a.path = ?1"),
                [path],
                asset_from_row,
            )
            .optional()?)
    }

    pub fn asset(&self, id: i64) -> Result<Asset> {
        self.conn
            .query_row(
                &format!("SELECT {ASSET_COLUMNS} FROM assets a JOIN folders f ON f.id = a.folder_id WHERE a.id = ?1"),
                [id],
                asset_from_row,
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("asset {id}")))
    }

    pub fn search(&self, q: &SearchQuery) -> Result<Vec<Asset>> {
        let mut sql = format!("SELECT {ASSET_COLUMNS} FROM assets a JOIN folders f ON f.id = a.folder_id WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let arg = |v: rusqlite::types::Value, args: &mut Vec<_>| {
            args.push(v);
            format!("?{}", args.len())
        };
        use rusqlite::types::Value as V;

        if let Some(folder) = &q.folder {
            sql += &format!(" AND f.path = {}", arg(V::Text(folder.clone()), &mut args));
        }
        if let Some(root) = &q.folder_tree {
            let root = root.trim_end_matches(['/', '\\']);
            let eq = arg(V::Text(root.to_string()), &mut args);
            let slash = arg(V::Text(format!("{}/%", like_escape(root))), &mut args);
            // Windows separators: an escaped `\` followed by the wildcard.
            let back = arg(V::Text(format!("{}\\\\%", like_escape(root))), &mut args);
            sql +=
                &format!(" AND (f.path = {eq} OR f.path LIKE {slash} ESCAPE '\\' OR f.path LIKE {back} ESCAPE '\\')");
        }
        if let Some(g) = q.has_gps {
            sql += if g { " AND a.gps_lat IS NOT NULL" } else { " AND a.gps_lat IS NULL" };
        }
        if let Some(n) = q.needs_place {
            sql += if n {
                concat!(" AND (", place_missing_sql!(), " OR ", to_check_sql!(), ")")
            } else {
                concat!(" AND NOT (", place_missing_sql!(), " OR ", to_check_sql!(), ")")
            };
        }
        if let Some(d) = q.dated {
            sql += if d { " AND a.taken_at IS NOT NULL" } else { " AND a.taken_at IS NULL" };
        }
        if let Some(text) = q.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            let p = arg(V::Text(format!("%{}%", like_escape(text))), &mut args);
            sql += &format!(
                " AND (a.file_name LIKE {p} ESCAPE '\\' OR a.caption LIKE {p} ESCAPE '\\' OR EXISTS (SELECT 1 \
                 FROM asset_keywords ak JOIN keywords k ON k.id = ak.keyword_id WHERE ak.asset_id = a.id \
                 AND k.name LIKE {p} ESCAPE '\\'))"
            );
        }
        if let Some(r) = q.min_rating.filter(|r| *r > 0) {
            sql += &format!(" AND a.rating >= {}", arg(V::Integer(r.into()), &mut args));
        }
        if let Some(l) = q.color_label {
            sql += &format!(" AND a.color_label = {}", arg(V::Integer(l.into()), &mut args));
        }
        if let Some(t) = q.tagged {
            sql += &format!(" AND a.tagged = {}", arg(V::Integer(t.into()), &mut args));
        }
        if let Some(kw) = &q.keyword {
            sql += &format!(
                " AND EXISTS (SELECT 1 FROM asset_keywords ak JOIN keywords k ON k.id = ak.keyword_id \
                 WHERE ak.asset_id = a.id AND k.name = {})",
                arg(V::Text(kw.clone()), &mut args)
            );
        }
        if let Some(p) = q.person_id {
            sql += &format!(
                " AND EXISTS (SELECT 1 FROM faces fc WHERE fc.asset_id = a.id AND fc.person_id = {})",
                arg(V::Integer(p), &mut args)
            );
        }
        if let Some(c) = q.collection_id {
            sql += &format!(
                " AND EXISTS (SELECT 1 FROM collection_assets ca WHERE ca.asset_id = a.id AND ca.collection_id = {})",
                arg(V::Integer(c), &mut args)
            );
        }
        if let Some(d) = &q.date_from {
            sql += &format!(" AND a.taken_at >= {}", arg(V::Text(d.clone()), &mut args));
        }
        if let Some(d) = &q.date_to {
            sql += &format!(" AND a.taken_at <= {}", arg(V::Text(format!("{d} 99")), &mut args));
        }
        if let Some(c) = q.camera.as_ref().filter(|c| !c.is_empty()) {
            let p = arg(V::Text(format!("%{}%", like_escape(c))), &mut args);
            sql += &format!(" AND (a.camera_make LIKE {p} ESCAPE '\\' OR a.camera_model LIKE {p} ESCAPE '\\')");
        }
        if let Some(d) = q.developed {
            sql += if d { " AND a.develop IS NOT NULL" } else { " AND a.develop IS NULL" };
        }
        sql += &format!(" ORDER BY {} {}, a.id", q.sort.column(), if q.descending { "DESC" } else { "ASC" });
        if let Some(limit) = q.limit {
            sql += &format!(" LIMIT {limit}");
        }
        let mut st = self.conn.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args), asset_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ------------------------------------------------------------ attributes

    pub fn set_rating(&self, ids: &[i64], rating: u8) -> Result<()> {
        self.update_each(ids, "UPDATE assets SET rating = ?2 WHERE id = ?1", rating.min(5) as i64)
    }

    pub fn set_color_label(&self, ids: &[i64], label: u8) -> Result<()> {
        self.update_each(ids, "UPDATE assets SET color_label = ?2 WHERE id = ?1", label.min(5) as i64)
    }

    pub fn set_tagged(&self, ids: &[i64], tagged: bool) -> Result<()> {
        self.update_each(ids, "UPDATE assets SET tagged = ?2 WHERE id = ?1", tagged as i64)
    }

    pub fn set_caption(&self, id: i64, caption: &str) -> Result<()> {
        let c = Some(caption.trim()).filter(|c| !c.is_empty());
        self.conn.execute("UPDATE assets SET caption = ?2 WHERE id = ?1", params![id, c])?;
        Ok(())
    }

    fn update_each(&self, ids: &[i64], sql: &str, value: i64) -> Result<()> {
        let mut st = self.conn.prepare_cached(sql)?;
        for id in ids {
            st.execute(params![id, value])?;
        }
        Ok(())
    }

    // --------------------------------------------------------------- keywords

    pub fn add_keyword(&self, ids: &[i64], name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        self.conn.execute("INSERT OR IGNORE INTO keywords(name) VALUES (?1)", [name])?;
        let kid: i64 = self.conn.query_row("SELECT id FROM keywords WHERE name = ?1", [name], |r| r.get(0))?;
        let mut st =
            self.conn.prepare_cached("INSERT OR IGNORE INTO asset_keywords(asset_id, keyword_id) VALUES (?1, ?2)")?;
        for id in ids {
            st.execute(params![id, kid])?;
        }
        Ok(())
    }

    pub fn remove_keyword(&self, ids: &[i64], name: &str) -> Result<()> {
        let mut st = self.conn.prepare_cached(
            "DELETE FROM asset_keywords WHERE asset_id = ?1 AND keyword_id = (SELECT id FROM keywords WHERE name = ?2)",
        )?;
        for id in ids {
            st.execute(params![id, name])?;
        }
        Ok(())
    }

    pub fn keywords_of(&self, id: i64) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "SELECT k.name FROM keywords k JOIN asset_keywords ak ON ak.keyword_id = k.id \
             WHERE ak.asset_id = ?1 ORDER BY k.name COLLATE NOCASE",
        )?;
        let rows = st.query_map([id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn keywords(&self) -> Result<Vec<NamedCount>> {
        let mut st = self.conn.prepare(
            "SELECT k.id, k.name, COUNT(ak.asset_id) FROM keywords k JOIN asset_keywords ak ON ak.keyword_id = k.id \
             GROUP BY k.id ORDER BY k.name COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| Ok(NamedCount { id: r.get(0)?, name: r.get(1)?, count: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ------------------------------------------------------------ collections

    pub fn create_collection(&self, name: &str) -> Result<i64> {
        self.conn.execute("INSERT OR IGNORE INTO collections(name) VALUES (?1)", [name.trim()])?;
        Ok(self.conn.query_row("SELECT id FROM collections WHERE name = ?1", [name.trim()], |r| r.get(0))?)
    }

    pub fn add_to_collection(&self, collection_id: i64, ids: &[i64]) -> Result<()> {
        let mut st = self
            .conn
            .prepare_cached("INSERT OR IGNORE INTO collection_assets(collection_id, asset_id) VALUES (?1, ?2)")?;
        for id in ids {
            st.execute(params![collection_id, id])?;
        }
        Ok(())
    }

    pub fn remove_from_collection(&self, collection_id: i64, ids: &[i64]) -> Result<()> {
        let mut st =
            self.conn.prepare_cached("DELETE FROM collection_assets WHERE collection_id = ?1 AND asset_id = ?2")?;
        for id in ids {
            st.execute(params![collection_id, id])?;
        }
        Ok(())
    }

    pub fn collections(&self) -> Result<Vec<NamedCount>> {
        let mut st = self.conn.prepare(
            "SELECT c.id, c.name, COUNT(ca.asset_id) FROM collections c LEFT JOIN collection_assets ca \
             ON ca.collection_id = c.id GROUP BY c.id ORDER BY c.name COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| Ok(NamedCount { id: r.get(0)?, name: r.get(1)?, count: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // --------------------------------------------------------- saved searches

    pub fn save_search(&self, name: &str, q: &SearchQuery) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO saved_searches(name, query) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET query = excluded.query",
            params![name.trim(), serde_json::to_string(q)?],
        )?;
        Ok(self.conn.query_row("SELECT id FROM saved_searches WHERE name = ?1", [name.trim()], |r| r.get(0))?)
    }

    pub fn saved_searches(&self) -> Result<Vec<SavedSearch>> {
        let mut st = self.conn.prepare("SELECT id, name, query FROM saved_searches ORDER BY name COLLATE NOCASE")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, q) = row?;
            out.push(SavedSearch { id, name, query: serde_json::from_str(&q)? });
        }
        Ok(out)
    }

    pub fn delete_saved_search(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM saved_searches WHERE id = ?1", [id])?;
        Ok(())
    }

    // --------------------------------------------------------------- calendar

    /// Photo counts per capture day (`YYYY-MM-DD`), oldest first, plus the
    /// number of photos without a capture date.
    pub fn calendar(&self) -> Result<Calendar> {
        let mut st = self.conn.prepare(
            "SELECT substr(taken_at, 1, 10) d, COUNT(*) FROM assets WHERE taken_at IS NOT NULL \
             GROUP BY d ORDER BY d",
        )?;
        let days = st
            .query_map([], |r| Ok(DayCount { day: r.get(0)?, count: r.get(1)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let undated = self.conn.query_row("SELECT COUNT(*) FROM assets WHERE taken_at IS NULL", [], |r| r.get(0))?;
        Ok(Calendar { days, undated })
    }

    /// Assets whose GPS has not been read yet (catalogs from before v2).
    pub fn gps_pending(&self, limit: usize) -> Result<Vec<(i64, String)>> {
        let mut st = self.conn.prepare("SELECT id, path FROM assets WHERE gps_scanned = 0 LIMIT ?1")?;
        let rows = st.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_gps(&self, id: i64, gps: Option<[f64; 2]>) -> Result<()> {
        self.conn.execute(
            "UPDATE assets SET gps_lat = ?2, gps_lon = ?3, gps_scanned = 1 WHERE id = ?1",
            params![id, gps.map(|g| g[0]), gps.map(|g| g[1])],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- presets

    /// Saves (or overwrites, by name) a user develop preset.
    pub fn save_preset(&self, name: &str, settings: &DevelopSettings) -> Result<i64> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("preset name is empty".into()));
        }
        self.conn.execute(
            "INSERT INTO presets(name, settings) VALUES (?1, ?2) \
             ON CONFLICT(name) DO UPDATE SET settings = excluded.settings",
            params![name, serde_json::to_string(settings)?],
        )?;
        Ok(self.conn.query_row("SELECT id FROM presets WHERE name = ?1", [name], |r| r.get(0))?)
    }

    pub fn presets(&self) -> Result<Vec<crate::workflow::Preset>> {
        let mut st = self.conn.prepare("SELECT id, name, settings FROM presets ORDER BY name COLLATE NOCASE")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, json) = row?;
            out.push(crate::workflow::Preset { id: Some(id), name, settings: serde_json::from_str(&json)? });
        }
        Ok(out)
    }

    pub fn delete_preset(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM presets WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Records that an asset's file was renamed inside its folder.
    pub fn rename_asset(&self, id: i64, new_path: &str) -> Result<()> {
        let p = Path::new(new_path);
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.conn.execute("UPDATE assets SET path = ?2, file_name = ?3 WHERE id = ?1", params![id, new_path, name])?;
        Ok(())
    }

    // ---------------------------------------------------------------- develop

    pub fn develop(&self, id: i64) -> Result<Option<DevelopSettings>> {
        let s: Option<String> =
            self.conn.query_row("SELECT develop FROM assets WHERE id = ?1", [id], |r| r.get(0)).optional()?.flatten();
        Ok(match s {
            Some(s) => Some(serde_json::from_str(&s)?),
            None => None,
        })
    }

    /// `None` (or default settings) clears the develop state.
    pub fn set_develop(&self, id: i64, settings: Option<&DevelopSettings>) -> Result<()> {
        let json = match settings.filter(|s| !s.is_identity()) {
            Some(s) => Some(serde_json::to_string(s)?),
            None => None,
        };
        self.conn.execute("UPDATE assets SET develop = ?2 WHERE id = ?1", params![id, json])?;
        Ok(())
    }

    // ----------------------------------------------------------- locations

    /// Replaces the imported location history with `points` (naive time,
    /// lat, lon). Returns how many were stored.
    pub fn set_track(&mut self, points: &[(i64, f64, f64)]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM track_points", [])?;
        {
            let mut st = tx.prepare("INSERT INTO track_points(t, lat, lon) VALUES (?1, ?2, ?3)")?;
            for (t, la, lo) in points {
                st.execute(params![t, la, lo])?;
            }
        }
        tx.commit()?;
        Ok(points.len())
    }

    pub fn track(&self) -> Result<Vec<crate::locate::RefPoint>> {
        let mut st = self.conn.prepare("SELECT t, lat, lon FROM track_points ORDER BY t")?;
        let rows = st.query_map([], |r| {
            Ok(crate::locate::RefPoint { t: r.get(0)?, lat: r.get(1)?, lon: r.get(2)?, camera: None, asset_id: None })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `(points, first, last)` of the imported location history.
    pub fn track_summary(&self) -> Result<(i64, Option<i64>, Option<i64>)> {
        Ok(self.conn.query_row("SELECT COUNT(*), MIN(t), MAX(t) FROM track_points", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?)
    }

    /// Dated photos without GPS: the ones whose place can be estimated.
    /// Dates read from file names (when a photo was sent) are left out.
    pub fn gps_targets(&self) -> Result<Vec<crate::locate::Target>> {
        let mut st = self.conn.prepare(
            "SELECT id, taken_at, camera_model, file_name FROM assets WHERE gps_lat IS NULL AND taken_at IS NOT NULL",
        )?;
        let rows = st.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, String>(3)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, taken, camera, name) = row?;
            if crate::metadata::date_from_filename(&name).as_deref() == Some(taken.as_str()) {
                continue;
            }
            if let Some(t) = crate::locate::naive_secs(&taken) {
                out.push(crate::locate::Target { id, t, camera });
            }
        }
        Ok(out)
    }

    /// Photos whose own metadata has both a capture time and GPS: the
    /// evidence for estimating where the other photos were taken.
    pub fn gps_refs(&self) -> Result<Vec<crate::locate::RefPoint>> {
        let mut st = self.conn.prepare(
            "SELECT id, taken_at, camera_model, gps_lat, gps_lon, file_name FROM assets \
             WHERE gps_lat IS NOT NULL AND gps_lon IS NOT NULL AND taken_at IS NOT NULL",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, taken, camera, lat, lon, name) = row?;
            // A date from the file name is when it was sent, not taken.
            if crate::metadata::date_from_filename(&name).as_deref() == Some(taken.as_str()) {
                continue;
            }
            if let Some(t) = crate::locate::naive_secs(&taken) {
                out.push(crate::locate::RefPoint { t, lat, lon, camera, asset_id: Some(id) });
            }
        }
        Ok(out)
    }

    /// Renames a place in the text of every photo that uses it (e.g. the
    /// district "Lince" → "Lima"). Returns the ids that changed.
    pub fn replace_place(&self, old: &str, new: &str) -> Result<Vec<i64>> {
        let rows: Vec<(i64, String)> = self
            .conn
            .prepare("SELECT id, develop FROM assets WHERE json_extract(develop, '$.text.place') = ?1")?
            .query_map([old], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut changed = Vec::new();
        for (id, json) in rows {
            let mut s: DevelopSettings = serde_json::from_str(&json)?;
            if let Some(t) = s.text.as_mut() {
                t.place = new.trim().to_string();
                t.place_from = crate::locate::Source::Typed;
                self.set_develop(id, Some(&s))?;
                changed.push(id);
            }
        }
        Ok(changed)
    }

    // ----------------------------------------------------------------- places

    /// Cached reverse-geocoding result. `Some(None)` = looked up, no place.
    pub fn place(&self, key: &str) -> Result<Option<Option<String>>> {
        Ok(self.conn.query_row("SELECT name FROM places WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_place(&self, key: &str, name: Option<&str>) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO places(key, name) VALUES (?1, ?2)", params![key, name])?;
        Ok(())
    }

    // --------------------------------------------------------------------- AI

    /// Assets that have never been analysed or changed since.
    pub fn ai_pending(&self) -> Result<Vec<(i64, String, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT a.id, a.path, a.mtime FROM assets a LEFT JOIN ai_analysis x ON x.asset_id = a.id \
             WHERE x.asset_id IS NULL OR x.mtime != a.mtime ORDER BY a.id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn ai_counts(&self) -> Result<AiCounts> {
        let q = |sql: &str| -> Result<i64> { Ok(self.conn.query_row(sql, [], |r| r.get(0))?) };
        Ok(AiCounts {
            analyzed: q("SELECT COUNT(*) FROM ai_analysis x JOIN assets a ON a.id = x.asset_id AND a.mtime = x.mtime")?,
            total: q("SELECT COUNT(*) FROM assets")?,
            faces: q("SELECT COUNT(*) FROM faces")?,
            people: q("SELECT COUNT(*) FROM persons p WHERE p.name IS NOT NULL OR \
                       (SELECT COUNT(*) FROM faces f WHERE f.person_id = p.id) >= 2")?,
        })
    }

    /// Replaces an asset's AI results in one transaction.
    pub fn store_analysis(&mut self, a: &Analysis) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM ai_tags WHERE asset_id = ?1", [a.asset_id])?;
        tx.execute("DELETE FROM faces WHERE asset_id = ?1", [a.asset_id])?;
        tx.execute(
            "INSERT OR REPLACE INTO ai_analysis(asset_id, mtime, embedding) VALUES (?1, ?2, ?3)",
            params![a.asset_id, a.mtime, a.embedding.as_ref().map(|e| crate::ai::to_blob(e))],
        )?;
        for (label, score) in &a.tags {
            tx.execute(
                "INSERT INTO ai_tags(asset_id, label, score) VALUES (?1, ?2, ?3)",
                params![a.asset_id, label, score],
            )?;
        }
        for f in &a.faces {
            tx.execute(
                "INSERT INTO faces(asset_id, x, y, w, h, score, embedding) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    a.asset_id,
                    f.rect[0],
                    f.rect[1],
                    f.rect[2],
                    f.rect[3],
                    f.score,
                    crate::ai::to_blob(&f.embedding)
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn embeddings(&self) -> Result<Vec<(i64, Vec<f32>)>> {
        let mut st = self.conn.prepare("SELECT asset_id, embedding FROM ai_analysis WHERE embedding IS NOT NULL")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, crate::ai::from_blob(&r.get::<_, Vec<u8>>(1)?))))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn ai_tags(&self, id: i64) -> Result<Vec<(String, f32)>> {
        let mut st = self.conn.prepare("SELECT label, score FROM ai_tags WHERE asset_id = ?1 ORDER BY score DESC")?;
        let rows = st.query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn faces_of(&self, id: i64) -> Result<Vec<FaceRow>> {
        let mut st = self.conn.prepare(
            "SELECT f.id, f.asset_id, f.x, f.y, f.w, f.h, f.person_id, p.name FROM faces f \
             LEFT JOIN persons p ON p.id = f.person_id WHERE f.asset_id = ?1 ORDER BY f.x",
        )?;
        let rows = st.query_map([id], face_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn face(&self, face_id: i64) -> Result<FaceRow> {
        self.conn
            .query_row(
                "SELECT f.id, f.asset_id, f.x, f.y, f.w, f.h, f.person_id, p.name FROM faces f \
                 LEFT JOIN persons p ON p.id = f.person_id WHERE f.id = ?1",
                [face_id],
                face_row,
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("face {face_id}")))
    }

    /// People with a name or seen at least twice, most photographed first.
    pub fn persons(&self) -> Result<Vec<PersonRow>> {
        let mut st = self.conn.prepare(
            "SELECT p.id, p.name, COUNT(DISTINCT f.asset_id), MAX(f.id) FROM persons p JOIN faces f ON f.person_id = p.id \
             GROUP BY p.id HAVING p.name IS NOT NULL OR COUNT(*) >= 2 \
             ORDER BY p.name IS NULL, COUNT(DISTINCT f.asset_id) DESC, p.name COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| {
            Ok(PersonRow { id: r.get(0)?, name: r.get(1)?, photos: r.get(2)?, cover_face: r.get(3)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Groups unassigned faces into people: each joins the person whose mean
    /// embedding is most similar (≥ `threshold`), otherwise starts a new one.
    pub fn cluster_faces(&mut self, threshold: f32) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut centroids: Vec<(i64, Vec<f32>, f32)> = Vec::new(); // (person, sum, n)
        {
            let mut st = tx.prepare("SELECT person_id, embedding FROM faces WHERE person_id IS NOT NULL")?;
            let rows =
                st.query_map([], |r| Ok((r.get::<_, i64>(0)?, crate::ai::from_blob(&r.get::<_, Vec<u8>>(1)?))))?;
            for row in rows {
                let (p, e) = row?;
                match centroids.iter_mut().find(|c| c.0 == p) {
                    Some(c) => {
                        c.1.iter_mut().zip(&e).for_each(|(a, b)| *a += b);
                        c.2 += 1.0;
                    }
                    None => centroids.push((p, e, 1.0)),
                }
            }
        }
        let unassigned: Vec<(i64, Vec<f32>)> = {
            let mut st = tx.prepare("SELECT id, embedding FROM faces WHERE person_id IS NULL ORDER BY id")?;
            let rows =
                st.query_map([], |r| Ok((r.get::<_, i64>(0)?, crate::ai::from_blob(&r.get::<_, Vec<u8>>(1)?))))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (face_id, e) in &unassigned {
            let best = centroids
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut m: Vec<f32> = c.1.iter().map(|v| v / c.2).collect();
                    crate::ai::normalize(&mut m);
                    (i, crate::ai::cosine(&m, e))
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            let idx = match best {
                Some((i, s)) if s >= threshold => i,
                _ => {
                    tx.execute("INSERT INTO persons(name) VALUES (NULL)", [])?;
                    centroids.push((tx.last_insert_rowid(), vec![0.0; e.len()], 0.0));
                    centroids.len() - 1
                }
            };
            let c = &mut centroids[idx];
            c.1.iter_mut().zip(e).for_each(|(a, b)| *a += b);
            c.2 += 1.0;
            tx.execute("UPDATE faces SET person_id = ?2 WHERE id = ?1", params![face_id, c.0])?;
        }
        tx.execute(
            "DELETE FROM persons WHERE id NOT IN (SELECT DISTINCT person_id FROM faces WHERE person_id IS NOT NULL)",
            [],
        )?;
        tx.commit()?;
        Ok(unassigned.len())
    }

    /// Names a person. If another person already has that name, the two are
    /// merged (the same person found in separate clusters). Returns the id
    /// that now holds the name.
    pub fn name_person(&mut self, person_id: i64, name: &str) -> Result<i64> {
        let name = name.trim();
        let tx = self.conn.transaction()?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM persons WHERE name = ?1 COLLATE NOCASE AND id != ?2",
                params![name, person_id],
                |r| r.get(0),
            )
            .optional()?;
        let target = match existing {
            Some(other) => {
                tx.execute("UPDATE faces SET person_id = ?1 WHERE person_id = ?2", params![other, person_id])?;
                tx.execute("DELETE FROM persons WHERE id = ?1", [person_id])?;
                other
            }
            None => {
                tx.execute(
                    "UPDATE persons SET name = ?2 WHERE id = ?1",
                    params![person_id, Some(name).filter(|n| !n.is_empty())],
                )?;
                person_id
            }
        };
        tx.commit()?;
        Ok(target)
    }

    /// Assigns one face to the person called `name` (created if needed).
    pub fn set_face_name(&mut self, face_id: i64, name: &str) -> Result<()> {
        let name = name.trim();
        let tx = self.conn.transaction()?;
        let pid: i64 = match tx
            .query_row("SELECT id FROM persons WHERE name = ?1 COLLATE NOCASE", [name], |r| r.get(0))
            .optional()?
        {
            Some(id) => id,
            None => {
                tx.execute("INSERT INTO persons(name) VALUES (?1)", [name])?;
                tx.last_insert_rowid()
            }
        };
        tx.execute("UPDATE faces SET person_id = ?2 WHERE id = ?1", params![face_id, pid])?;
        tx.execute(
            "DELETE FROM persons WHERE id NOT IN (SELECT DISTINCT person_id FROM faces WHERE person_id IS NOT NULL)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------- similarity

    pub fn set_phash(&self, id: i64, hash: u64) -> Result<()> {
        self.conn.execute("UPDATE assets SET phash = ?2 WHERE id = ?1", params![id, hash as i64])?;
        Ok(())
    }

    pub fn missing_phash(&self) -> Result<Vec<i64>> {
        let mut st = self.conn.prepare("SELECT id FROM assets WHERE phash IS NULL")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Assets whose perceptual hash is within `max_distance` bits of `id`,
    /// closest first. The asset itself is excluded.
    pub fn similar(&self, id: i64, max_distance: u32) -> Result<Vec<(Asset, u32)>> {
        let target: Option<i64> =
            self.conn.query_row("SELECT phash FROM assets WHERE id = ?1", [id], |r| r.get(0)).optional()?.flatten();
        let Some(target) = target else { return Ok(Vec::new()) };
        let mut st = self.conn.prepare("SELECT id, phash FROM assets WHERE phash IS NOT NULL AND id != ?1")?;
        let mut hits: Vec<(i64, u32)> = st
            .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
            .filter_map(|r| r.ok())
            .map(|(i, h)| (i, hamming(target as u64, h as u64)))
            .filter(|(_, d)| *d <= max_distance)
            .collect();
        hits.sort_by_key(|(_, d)| *d);
        hits.into_iter().map(|(i, d)| Ok((self.asset(i)?, d))).collect()
    }
}

/// Escapes `%`, `_` and `\` for use in `LIKE … ESCAPE '\'`.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn face_row(r: &Row<'_>) -> rusqlite::Result<FaceRow> {
    Ok(FaceRow {
        id: r.get(0)?,
        asset_id: r.get(1)?,
        rect: [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
        person_id: r.get(6)?,
        person_name: r.get(7)?,
    })
}

/// `path` is `root` or a file/folder somewhere below it (either separator).
pub fn at_or_below(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches(['/', '\\']);
    path == root || path.strip_prefix(root).is_some_and(|rest| rest.starts_with(['/', '\\']))
}

/// Paths in `folders` that are not inside another path of the list.
fn top_level(folders: &[String]) -> Vec<&String> {
    folders
        .iter()
        .filter(|f| {
            !folders.iter().any(|o| {
                let o = o.trim_end_matches(['/', '\\']);
                o != f.as_str() && f.strip_prefix(o).is_some_and(|rest| rest.starts_with(['/', '\\']))
            })
        })
        .collect()
}

/// Brings catalogs created by older versions up to the current columns.
fn migrate(conn: &Connection) -> Result<()> {
    let has_gps: bool =
        conn.query_row("SELECT COUNT(*) FROM pragma_table_info('assets') WHERE name = 'gps_lat'", [], |r| r.get(0))?;
    if !has_gps {
        conn.execute_batch(
            "ALTER TABLE assets ADD COLUMN gps_lat REAL; \
             ALTER TABLE assets ADD COLUMN gps_lon REAL; \
             ALTER TABLE assets ADD COLUMN gps_scanned INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS assets_gps ON assets(gps_lat) WHERE gps_lat IS NOT NULL;")?;
    // Photos indexed without a date may carry one in their file name
    // (WhatsApp and phone cameras); cheap, so check on every start.
    let undated: Vec<(i64, String)> = conn
        .prepare("SELECT id, file_name FROM assets WHERE taken_at IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, name) in undated {
        if let Some(d) = crate::metadata::date_from_filename(&name) {
            conn.execute("UPDATE assets SET taken_at = ?2 WHERE id = ?1", params![id, d])?;
        }
    }
    // Catalogs from before library roots were recorded: the top-most
    // folders that hold photos become the roots.
    let roots: i64 = conn.query_row("SELECT COUNT(*) FROM roots", [], |r| r.get(0))?;
    if roots == 0 {
        let mut st = conn.prepare("SELECT path FROM folders")?;
        let folders: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for f in top_level(&folders) {
            conn.execute("INSERT OR IGNORE INTO roots(path, recursive) VALUES (?1, 1)", [f])?;
        }
    }
    Ok(())
}

fn folder_id_on(conn: &Connection, path: &str) -> Result<i64> {
    conn.execute("INSERT OR IGNORE INTO folders(path) VALUES (?1)", [path])?;
    Ok(conn.query_row("SELECT id FROM folders WHERE path = ?1", [path], |r| r.get(0))?)
}

fn upsert_on(conn: &Connection, f: &ScannedFile) -> Result<Upsert> {
    let path = f.path.to_string_lossy();
    let existing: Option<(i64, i64, i64)> = conn
        .query_row("SELECT id, size, mtime FROM assets WHERE path = ?1", [&*path], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    let folder_id = folder_id_on(conn, &f.folder.to_string_lossy())?;
    let e = &f.exif;
    match existing {
        Some((id, size, mtime)) if size == f.size && mtime == f.mtime => {
            // Catalogs from before GPS was stored learn it on the next scan.
            conn.prepare_cached(
                "UPDATE assets SET gps_lat = ?2, gps_lon = ?3, gps_scanned = 1 WHERE id = ?1 AND gps_scanned = 0",
            )?
            .execute(params![id, e.gps.map(|g| g[0]), e.gps.map(|g| g[1])])?;
            Ok(Upsert::Unchanged(id))
        }
        Some((id, ..)) => {
            conn.execute(
                "UPDATE assets SET folder_id=?2, size=?3, mtime=?4, width=?5, height=?6, taken_at=?7, \
                 camera_make=?8, camera_model=?9, lens=?10, iso=?11, f_number=?12, exposure_time=?13, \
                 focal_length=?14, orientation=?15, phash=NULL, gps_lat=?16, gps_lon=?17, gps_scanned=1 WHERE id=?1",
                params![
                    id,
                    folder_id,
                    f.size,
                    f.mtime,
                    e.width,
                    e.height,
                    e.taken_at,
                    e.camera_make,
                    e.camera_model,
                    e.lens,
                    e.iso,
                    e.f_number,
                    e.exposure_time,
                    e.focal_length,
                    e.orientation,
                    e.gps.map(|g| g[0]),
                    e.gps.map(|g| g[1])
                ],
            )?;
            Ok(Upsert::Updated(id))
        }
        None => {
            conn.execute(
                "INSERT INTO assets(folder_id, path, file_name, ext, size, mtime, width, height, taken_at, \
                 camera_make, camera_model, lens, iso, f_number, exposure_time, focal_length, orientation, \
                 gps_lat, gps_lon, gps_scanned) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,1)",
                params![
                    folder_id,
                    &*path,
                    f.file_name,
                    f.ext,
                    f.size,
                    f.mtime,
                    e.width,
                    e.height,
                    e.taken_at,
                    e.camera_make,
                    e.camera_model,
                    e.lens,
                    e.iso,
                    e.f_number,
                    e.exposure_time,
                    e.focal_length,
                    e.orientation,
                    e.gps.map(|g| g[0]),
                    e.gps.map(|g| g[1])
                ],
            )?;
            Ok(Upsert::Inserted(conn.last_insert_rowid()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::ExifInfo;
    use crate::search::SortKey;
    use std::path::PathBuf;

    fn scanned(path: &str, mtime: i64, taken: &str) -> ScannedFile {
        let p = PathBuf::from(path);
        ScannedFile {
            folder: p.parent().unwrap().to_path_buf(),
            file_name: p.file_name().unwrap().to_string_lossy().into(),
            ext: "jpg".into(),
            size: 100,
            mtime,
            exif: ExifInfo {
                taken_at: Some(taken.into()),
                camera_model: Some("X-T5".into()),
                orientation: 1,
                ..Default::default()
            },
            path: p,
        }
    }

    fn in_folder(folder: &str, name: &str, taken: Option<&str>, gps: Option<[f64; 2]>) -> ScannedFile {
        let sep = if folder.contains('\\') { '\\' } else { '/' };
        ScannedFile {
            folder: PathBuf::from(folder),
            file_name: name.into(),
            ext: "jpg".into(),
            size: 100,
            mtime: 1,
            exif: ExifInfo { taken_at: taken.map(Into::into), gps, orientation: 1, ..Default::default() },
            path: PathBuf::from(format!("{folder}{sep}{name}")),
        }
    }

    #[test]
    fn folder_tree_gps_and_calendar() {
        let mut c = Catalog::open_in_memory().unwrap();
        let files = [
            in_folder("/p", "a.jpg", Some("2024-05-01 10:00:00"), Some([31.77, 35.21])),
            in_folder("/p/sub", "b.jpg", Some("2024-05-01 18:00:00"), None),
            in_folder("/p/sub/deep", "c.jpg", Some("2023-12-31 23:59:59"), None),
            in_folder("/pp", "d.jpg", None, Some([32.08, 34.78])),
            in_folder("/p_x", "e.jpg", None, None),
            in_folder("C:\\Photos\\2024", "f.jpg", None, None),
            in_folder("C:\\Photos", "g.jpg", None, None),
        ];
        c.upsert_many(&files).unwrap();
        let count = |q: SearchQuery| c.search(&q).unwrap().len();
        let tree = |p: &str| count(SearchQuery { folder_tree: Some(p.into()), ..Default::default() });
        assert_eq!(tree("/p"), 3, "/pp and /p_x are not inside /p");
        assert_eq!(tree("/p/"), 3);
        assert_eq!(tree("/p/sub"), 2);
        assert_eq!(tree("C:\\Photos"), 2);
        assert_eq!(count(SearchQuery { has_gps: Some(true), ..Default::default() }), 2);
        assert_eq!(count(SearchQuery { dated: Some(false), ..Default::default() }), 4);
        let a = c.search(&SearchQuery { has_gps: Some(true), sort: SortKey::Name, ..Default::default() }).unwrap();
        assert_eq!(a[0].gps, Some([31.77, 35.21]));

        let cal = c.calendar().unwrap();
        assert_eq!(cal.undated, 4);
        assert_eq!(
            cal.days,
            vec![DayCount { day: "2023-12-31".into(), count: 1 }, DayCount { day: "2024-05-01".into(), count: 2 }]
        );
        // The UI filters a day with dateFrom = dateTo = day.
        let day = SearchQuery {
            date_from: Some("2024-05-01".into()),
            date_to: Some("2024-05-01".into()),
            ..Default::default()
        };
        assert_eq!(count(day), 2);
    }

    #[test]
    fn v1_catalog_migrates_and_backfills_gps() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cat.db");
        {
            // A v1 catalog: assets table without GPS columns.
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE folders (id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE);
                 CREATE TABLE assets (id INTEGER PRIMARY KEY, folder_id INTEGER NOT NULL, path TEXT NOT NULL UNIQUE,
                   file_name TEXT NOT NULL, ext TEXT NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL,
                   width INTEGER, height INTEGER, taken_at TEXT, camera_make TEXT, camera_model TEXT, lens TEXT,
                   iso INTEGER, f_number REAL, exposure_time REAL, focal_length REAL,
                   orientation INTEGER NOT NULL DEFAULT 1, rating INTEGER NOT NULL DEFAULT 0,
                   color_label INTEGER NOT NULL DEFAULT 0, tagged INTEGER NOT NULL DEFAULT 0, caption TEXT,
                   phash INTEGER, develop TEXT);
                 INSERT INTO folders VALUES (1, '/p');
                 INSERT INTO assets(id, folder_id, path, file_name, ext, size, mtime, rating)
                   VALUES (1, 1, '/p/a.jpg', 'a.jpg', 'jpg', 100, 1, 4);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let mut c = Catalog::open(&db).unwrap();
        let a = c.asset(1).unwrap();
        assert_eq!((a.rating, a.gps), (4, None));
        assert_eq!(c.gps_pending(10).unwrap(), vec![(1, "/p/a.jpg".to_string())]);
        // An unchanged rescan fills GPS in…
        c.upsert_many(&[in_folder("/p", "a.jpg", None, Some([1.5, 2.5]))]).unwrap();
        assert_eq!(c.asset(1).unwrap().gps, Some([1.5, 2.5]));
        assert!(c.gps_pending(10).unwrap().is_empty());
        // …and so does the background backfill.
        c.set_gps(1, None).unwrap();
        assert_eq!(c.asset(1).unwrap().gps, None);
    }

    #[test]
    fn roots_are_recorded_deduplicated_and_derived_for_old_catalogs() {
        let c = Catalog::open_in_memory().unwrap();
        c.add_root("/p", true).unwrap();
        c.add_root("/p/sub", false).unwrap();
        c.add_root("/q", false).unwrap();
        c.add_root("/q", true).unwrap();
        assert_eq!(c.roots().unwrap(), vec![("/p".to_string(), true), ("/q".to_string(), true)]);

        // A catalog with folders but no roots derives the top-level ones.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("c.db");
        {
            let mut c = Catalog::open(&db).unwrap();
            c.upsert_many(&[in_folder("/a/x", "1.jpg", None, None), in_folder("/a/x/y", "2.jpg", None, None)]).unwrap();
            c.upsert_many(&[in_folder("/b", "3.jpg", None, None)]).unwrap();
            c.conn.execute("DELETE FROM roots", []).unwrap();
        }
        let c = Catalog::open(&db).unwrap();
        assert_eq!(c.roots().unwrap(), vec![("/a/x".to_string(), true), ("/b".to_string(), true)]);
    }

    #[test]
    fn removing_a_folder_forgets_it_but_not_its_neighbours() {
        let mut c = Catalog::open_in_memory().unwrap();
        c.add_root("/trip", true).unwrap();
        c.add_root("/trip/cusco/raw", false).unwrap();
        c.add_root("/home", true).unwrap();
        c.upsert_many(&[
            in_folder("/trip/lima", "1.jpg", None, None),
            in_folder("/trip/cusco", "2.jpg", None, None),
            in_folder("/trip/cusco/raw", "3.jpg", None, None),
            in_folder("/trip/cusco-2", "4.jpg", None, None),
            in_folder("/home", "5.jpg", None, None),
        ])
        .unwrap();
        let rated = c.asset_by_path("/trip/cusco/2.jpg").unwrap().unwrap().id;
        c.set_rating(&[rated], 5).unwrap();

        // A sub-folder of a root: its photos go, the root stays, and the
        // folder is excluded so the root's watcher won't re-add it.
        let r = c.remove_folder("/trip/cusco/").unwrap();
        assert_eq!(r.photos, 2);
        assert_eq!(r.unwatched, vec!["/trip/cusco/raw".to_string()]);
        let left: Vec<String> = c.folders().unwrap().into_iter().map(|f| f.path).collect();
        assert_eq!(left, vec!["/home", "/trip/cusco-2", "/trip/lima"]);
        assert_eq!(c.roots().unwrap(), vec![("/home".to_string(), true), ("/trip".to_string(), true)]);
        assert_eq!(c.excluded().unwrap(), vec!["/trip/cusco".to_string()]);

        // Adding it back clears the exclusion; the photo returns fresh.
        c.add_root("/trip/cusco", true).unwrap();
        assert!(c.excluded().unwrap().is_empty());
        c.upsert_many(&[in_folder("/trip/cusco", "2.jpg", None, None)]).unwrap();
        assert_eq!(c.asset_by_path("/trip/cusco/2.jpg").unwrap().unwrap().rating, 0);

        // A whole root: no exclusion needed, it is simply no longer watched.
        let r = c.remove_folder("/home").unwrap();
        assert_eq!((r.photos, r.unwatched), (1, vec!["/home".to_string()]));
        assert!(c.excluded().unwrap().is_empty());
        assert_eq!(c.search(&SearchQuery::default()).unwrap().len(), 3);
    }

    #[test]
    fn upsert_detects_changes() {
        let c = Catalog::open_in_memory().unwrap();
        let a = c.upsert_scanned(&scanned("/p/a.jpg", 1, "2024-01-01 10:00:00")).unwrap();
        assert!(matches!(a, Upsert::Inserted(_)));
        assert!(matches!(c.upsert_scanned(&scanned("/p/a.jpg", 1, "x")).unwrap(), Upsert::Unchanged(_)));
        assert!(matches!(c.upsert_scanned(&scanned("/p/a.jpg", 2, "x")).unwrap(), Upsert::Updated(_)));
        assert_eq!(c.folders().unwrap()[0].count, 1);
    }

    #[test]
    fn batch_upsert_and_search() {
        let mut c = Catalog::open_in_memory().unwrap();
        let files = vec![
            scanned("/p/a.jpg", 1, "2024-01-01 10:00:00"),
            scanned("/p/b.jpg", 1, "2024-06-01 10:00:00"),
            scanned("/q/c.jpg", 1, "2025-01-01 10:00:00"),
        ];
        let ids: Vec<i64> = c.upsert_many(&files).unwrap().into_iter().map(Upsert::id).collect();
        c.set_rating(&ids[..2], 4).unwrap();
        c.set_color_label(&[ids[1]], 2).unwrap();
        c.add_keyword(&[ids[0], ids[2]], "beach").unwrap();

        let q = |q: SearchQuery| c.search(&q).unwrap().into_iter().map(|a| a.id).collect::<Vec<_>>();
        assert_eq!(q(SearchQuery { min_rating: Some(4), ..Default::default() }), vec![ids[0], ids[1]]);
        assert_eq!(q(SearchQuery { color_label: Some(2), ..Default::default() }), vec![ids[1]]);
        assert_eq!(q(SearchQuery { keyword: Some("beach".into()), ..Default::default() }), vec![ids[0], ids[2]]);
        assert_eq!(q(SearchQuery { text: Some("bea".into()), ..Default::default() }), vec![ids[0], ids[2]]);
        assert_eq!(q(SearchQuery { folder: Some("/q".into()), ..Default::default() }), vec![ids[2]]);
        assert_eq!(
            q(SearchQuery {
                date_from: Some("2024-03-01".into()),
                date_to: Some("2024-12-31".into()),
                ..Default::default()
            }),
            vec![ids[1]]
        );
        assert_eq!(
            q(SearchQuery { sort: crate::search::SortKey::TakenAt, descending: true, ..Default::default() }),
            vec![ids[2], ids[1], ids[0]]
        );
        assert_eq!(c.keywords_of(ids[0]).unwrap(), vec!["beach".to_string()]);
        // LIKE wildcards in user text are literal.
        assert!(q(SearchQuery { text: Some("a_jpg".into()), ..Default::default() }).is_empty());
        assert!(q(SearchQuery { text: Some("%".into()), ..Default::default() }).is_empty());
        c.remove_keyword(&[ids[0]], "beach").unwrap();
        assert!(c.keywords_of(ids[0]).unwrap().is_empty());
    }

    #[test]
    fn develop_roundtrip_and_identity_clears() {
        let c = Catalog::open_in_memory().unwrap();
        let id = c.upsert_scanned(&scanned("/p/a.jpg", 1, "2024")).unwrap().id();
        let mut s = DevelopSettings::default();
        s.light.exposure = 0.5;
        c.set_develop(id, Some(&s)).unwrap();
        assert_eq!(c.develop(id).unwrap(), Some(s));
        assert!(c.asset(id).unwrap().developed);
        c.set_develop(id, Some(&DevelopSettings::default())).unwrap();
        assert_eq!(c.develop(id).unwrap(), None);
    }

    #[test]
    fn collections_saved_searches_similarity() {
        let c = Catalog::open_in_memory().unwrap();
        let a = c.upsert_scanned(&scanned("/p/a.jpg", 1, "2024")).unwrap().id();
        let b = c.upsert_scanned(&scanned("/p/b.jpg", 1, "2024")).unwrap().id();
        let d = c.upsert_scanned(&scanned("/p/d.jpg", 1, "2024")).unwrap().id();
        let col = c.create_collection("Best").unwrap();
        c.add_to_collection(col, &[a, b]).unwrap();
        assert_eq!(c.collections().unwrap()[0].count, 2);
        let q = SearchQuery { collection_id: Some(col), ..Default::default() };
        assert_eq!(c.search(&q).unwrap().len(), 2);

        assert_eq!(c.place("1,2").unwrap(), None);
        c.set_place("1,2", Some("Haifa, Israel")).unwrap();
        c.set_place("0,0", None).unwrap();
        assert_eq!(c.place("1,2").unwrap(), Some(Some("Haifa, Israel".into())));
        assert_eq!(c.place("0,0").unwrap(), Some(None));

        c.save_search("five", &SearchQuery { min_rating: Some(5), ..Default::default() }).unwrap();
        assert_eq!(c.saved_searches().unwrap()[0].query.min_rating, Some(5));

        let mut look = DevelopSettings::default();
        look.light.contrast = 30.0;
        let pid = c.save_preset(" Punchy ", &look).unwrap();
        look.light.contrast = 40.0;
        assert_eq!(c.save_preset("punchy", &look).unwrap(), pid, "same name (any case) overwrites");
        assert_eq!(c.presets().unwrap()[0].settings.light.contrast, 40.0);
        assert!(c.save_preset("  ", &look).is_err());
        c.delete_preset(pid).unwrap();
        assert!(c.presets().unwrap().is_empty());

        let with_text = |place: &str| DevelopSettings {
            text: Some(crate::overlay::TextOverlay {
                place: place.into(),
                date: "01.10.2026".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        c.set_develop(a, Some(&with_text("Lince"))).unwrap();
        c.set_develop(b, Some(&with_text(""))).unwrap();
        let needs = c.search(&SearchQuery { needs_place: Some(true), ..Default::default() }).unwrap();
        assert_eq!(needs.iter().map(|x| x.id).collect::<Vec<_>>(), vec![b]);
        assert!(needs[0].has_text && needs[0].needs_place);
        assert_eq!(c.replace_place("Lince", "Lima").unwrap(), vec![a]);
        assert_eq!(c.develop(a).unwrap().unwrap().text.unwrap().place, "Lima");

        // Text saved before sources were recorded (no placeFrom/dateFrom).
        c.conn
            .execute(
                "UPDATE assets SET develop = '{\"text\":{\"place\":\"Lima\",\"date\":\"01.10.2026\"}}' WHERE id = ?1",
                [a],
            )
            .unwrap();
        let legacy = c.asset(a).unwrap();
        assert!(legacy.has_text && !legacy.to_check && !legacy.needs_place);
        assert_eq!(c.search(&SearchQuery { needs_place: Some(false), ..Default::default() }).unwrap().len(), 2);

        // An estimated place shows up for review until confirmed.
        let mut est = with_text("Cusco");
        est.text.as_mut().unwrap().place_from = crate::locate::Source::Phone;
        c.set_develop(d, Some(&est)).unwrap();
        let review = |c: &Catalog| {
            c.search(&SearchQuery { needs_place: Some(true), ..Default::default() })
                .unwrap()
                .iter()
                .map(|x| x.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(review(&c), vec![b, d]);
        assert!(c.asset(d).unwrap().to_check && !c.asset(d).unwrap().needs_place);
        est.text.as_mut().unwrap().confirm();
        c.set_develop(d, Some(&est)).unwrap();
        assert_eq!(review(&c), vec![b]);

        let mut c = c;
        assert_eq!(c.set_track(&[(100, 1.0, 2.0), (200, 3.0, 4.0)]).unwrap(), 2);
        assert_eq!(c.track_summary().unwrap(), (2, Some(100), Some(200)));
        assert_eq!(c.track().unwrap()[1].lat, 3.0);

        c.rename_asset(a, "/p/renamed.jpg").unwrap();
        let ra = c.asset(a).unwrap();
        assert_eq!((ra.path.as_str(), ra.file_name.as_str()), ("/p/renamed.jpg", "renamed.jpg"));
        assert_eq!(c.asset_by_path("/p/renamed.jpg").unwrap().map(|x| x.id), Some(a));
        assert!(c.asset_by_path("/p/a.jpg").unwrap().is_none());

        c.set_phash(a, 0b1111).unwrap();
        c.set_phash(b, 0b1110).unwrap();
        c.set_phash(d, u64::MAX).unwrap();
        let sim = c.similar(a, 8).unwrap();
        assert_eq!(sim.len(), 1);
        assert_eq!((sim[0].0.id, sim[0].1), (b, 1));
    }

    #[test]
    fn ai_analysis_people_and_search() {
        let mut c = Catalog::open_in_memory().unwrap();
        let ids: Vec<i64> = ["a", "b", "c"]
            .iter()
            .map(|n| c.upsert_scanned(&scanned(&format!("/p/{n}.jpg"), 1, "2024")).unwrap().id())
            .collect();
        assert_eq!(c.ai_pending().unwrap().len(), 3);
        let unit = |i: usize| {
            let mut v = vec![0.0f32; 8];
            v[i] = 1.0;
            v
        };
        let near = |i: usize| {
            let mut v = unit(i);
            v[7] = 0.1;
            crate::ai::normalize(&mut v);
            v
        };
        let face = |e: Vec<f32>| AnalyzedFace { rect: [0.1, 0.1, 0.2, 0.2], score: 0.9, embedding: e };
        // a: Alice; b: Alice + Bob; c: Bob
        let data = [
            (ids[0], vec![face(unit(0))]),
            (ids[1], vec![face(near(0)), face(unit(1))]),
            (ids[2], vec![face(near(1))]),
        ];
        for (id, faces) in data {
            c.store_analysis(&Analysis {
                asset_id: id,
                mtime: 1,
                embedding: Some(unit(2)),
                tags: vec![("beach".into(), 0.6)],
                faces,
            })
            .unwrap();
        }
        assert!(c.ai_pending().unwrap().is_empty());
        assert_eq!(c.cluster_faces(0.8).unwrap(), 4);
        let people = c.persons().unwrap();
        assert_eq!(people.len(), 2, "{people:?}");
        assert!(people.iter().all(|p| p.photos == 2));
        let alice = c.faces_of(ids[0]).unwrap()[0].person_id.unwrap();
        c.name_person(alice, "Alice").unwrap();
        let q = SearchQuery { person_id: Some(alice), ..Default::default() };
        assert_eq!(c.search(&q).unwrap().iter().map(|a| a.id).collect::<Vec<_>>(), vec![ids[0], ids[1]]);
        // Naming Bob "alice" merges the clusters.
        let bob = c.faces_of(ids[2]).unwrap()[0].person_id.unwrap();
        assert_eq!(c.name_person(bob, "alice").unwrap(), alice);
        assert_eq!(c.persons().unwrap().len(), 1);
        // Re-assign one face to a new name.
        let f = c.faces_of(ids[2]).unwrap()[0].id;
        c.set_face_name(f, "Bob").unwrap();
        assert_eq!(c.face(f).unwrap().person_name.as_deref(), Some("Bob"));
        assert_eq!(c.ai_tags(ids[0]).unwrap(), vec![("beach".to_string(), 0.6)]);
        assert_eq!(c.embeddings().unwrap().len(), 3);
        let n = c.ai_counts().unwrap();
        assert_eq!((n.analyzed, n.total, n.faces), (3, 3, 4));
        // Editing the file makes it pending again.
        c.upsert_scanned(&scanned("/p/a.jpg", 2, "2024")).unwrap();
        assert_eq!(c.ai_pending().unwrap().len(), 1);
    }
}
