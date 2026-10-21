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

const SCHEMA_VERSION: i64 = 1;

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
    develop       TEXT
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
}

const ASSET_COLUMNS: &str = "a.id, a.path, f.path, a.file_name, a.ext, a.size, a.mtime, a.width, a.height, \
    a.taken_at, a.camera_make, a.camera_model, a.lens, a.iso, a.f_number, a.exposure_time, a.focal_length, \
    a.orientation, a.rating, a.color_label, a.tagged, a.caption, a.develop IS NOT NULL";

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
    })
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
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v < SCHEMA_VERSION {
            conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))?;
        }
        Ok(Self { conn })
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
        if let Some(text) = q.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
            let p = arg(V::Text(format!("%{text}%")), &mut args);
            sql += &format!(
                " AND (a.file_name LIKE {p} OR a.caption LIKE {p} OR EXISTS (SELECT 1 FROM asset_keywords ak \
                 JOIN keywords k ON k.id = ak.keyword_id WHERE ak.asset_id = a.id AND k.name LIKE {p}))"
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
            let p = arg(V::Text(format!("%{c}%")), &mut args);
            sql += &format!(" AND (a.camera_make LIKE {p} OR a.camera_model LIKE {p})");
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
        Some((id, size, mtime)) if size == f.size && mtime == f.mtime => Ok(Upsert::Unchanged(id)),
        Some((id, ..)) => {
            conn.execute(
                "UPDATE assets SET folder_id=?2, size=?3, mtime=?4, width=?5, height=?6, taken_at=?7, \
                 camera_make=?8, camera_model=?9, lens=?10, iso=?11, f_number=?12, exposure_time=?13, \
                 focal_length=?14, orientation=?15, phash=NULL WHERE id=?1",
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
                    e.orientation
                ],
            )?;
            Ok(Upsert::Updated(id))
        }
        None => {
            conn.execute(
                "INSERT INTO assets(folder_id, path, file_name, ext, size, mtime, width, height, taken_at, \
                 camera_make, camera_model, lens, iso, f_number, exposure_time, focal_length, orientation) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
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
                    e.orientation
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

        c.save_search("five", &SearchQuery { min_rating: Some(5), ..Default::default() }).unwrap();
        assert_eq!(c.saved_searches().unwrap()[0].query.min_rating, Some(5));

        c.set_phash(a, 0b1111).unwrap();
        c.set_phash(b, 0b1110).unwrap();
        c.set_phash(d, u64::MAX).unwrap();
        let sim = c.similar(a, 8).unwrap();
        assert_eq!(sim.len(), 1);
        assert_eq!((sim[0].0.id, sim[0].1), (b, 1));
    }
}
