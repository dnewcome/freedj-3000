use opendeck_types::{BeatGrid, CueMap, CuePoint, CueKind, Rgb, SavedLoop, TrackInfo};
use rusqlite::{Connection, Result, params};
use std::path::Path;

pub struct Library {
    conn: Connection,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let lib = Self { conn };
        lib.create_schema()?;
        Ok(lib)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let lib = Self { conn };
        lib.create_schema()?;
        Ok(lib)
    }

    fn create_schema(&self) -> Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS tracks (
                id               INTEGER PRIMARY KEY,
                path             TEXT NOT NULL UNIQUE,
                file_hash        BLOB NOT NULL,
                title            TEXT,
                artist           TEXT,
                album            TEXT,
                duration_frames  INTEGER,
                sample_rate      INTEGER,
                channels         INTEGER,
                bpm              REAL,
                key              TEXT,
                analyzed_at      INTEGER,
                play_count       INTEGER DEFAULT 0,
                last_played      INTEGER
            );

            CREATE TABLE IF NOT EXISTS beat_grids (
                track_id         INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                anchor_sample    INTEGER NOT NULL,
                bpm              REAL NOT NULL,
                beats_blob       BLOB,
                downbeat_offset  INTEGER DEFAULT 0,
                confidence       REAL,
                locked           INTEGER DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS cue_points (
                id               INTEGER PRIMARY KEY,
                track_id         INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                slot             INTEGER NOT NULL,
                position         INTEGER NOT NULL,
                color_r          INTEGER, color_g INTEGER, color_b INTEGER,
                label            TEXT,
                kind             TEXT NOT NULL DEFAULT 'hot'
            );

            CREATE TABLE IF NOT EXISTS saved_loops (
                id               INTEGER PRIMARY KEY,
                track_id         INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                slot             INTEGER NOT NULL,
                in_pt            INTEGER NOT NULL,
                out_pt           INTEGER NOT NULL,
                label            TEXT
            );

            CREATE TABLE IF NOT EXISTS playlists (
                id               INTEGER PRIMARY KEY,
                name             TEXT NOT NULL,
                parent_id        INTEGER REFERENCES playlists(id),
                position         INTEGER
            );

            CREATE TABLE IF NOT EXISTS playlist_tracks (
                playlist_id      INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                track_id         INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                position         INTEGER,
                PRIMARY KEY (playlist_id, track_id)
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS tracks_fts USING fts5(
                title, artist, album,
                content='tracks', content_rowid='id'
            );
        ")?;
        Ok(())
    }

    // ── Track CRUD ────────────────────────────────────────────────────────────

    pub fn upsert_track(&self, track: &TrackInfo, hash: &[u8]) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO tracks (path, file_hash, title, artist, album,
                duration_frames, sample_rate, channels, bpm, key)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(path) DO UPDATE SET
                file_hash=excluded.file_hash,
                title=excluded.title,
                artist=excluded.artist",
            params![
                track.path.to_string_lossy().as_ref(),
                hash,
                track.title,
                track.artist,
                track.album,
                track.duration_frames as i64,
                track.sample_rate as i64,
                track.channels as i64,
                track.bpm,
                track.key,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_track(&self, id: i64) -> Result<Option<TrackInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,path,title,artist,album,duration_frames,sample_rate,channels,bpm,key
             FROM tracks WHERE id=?1"
        )?;
        let mut rows = stmt.query(params![id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_track(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn search_tracks(&self, query: &str, limit: usize) -> Result<Vec<TrackInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id,t.path,t.title,t.artist,t.album,
                    t.duration_frames,t.sample_rate,t.channels,t.bpm,t.key
             FROM tracks t
             JOIN tracks_fts f ON f.rowid = t.id
             WHERE tracks_fts MATCH ?1
             ORDER BY rank LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |row| {
            row_to_track(row)
        })?;
        rows.collect()
    }

    // ── Beat grid ─────────────────────────────────────────────────────────────

    pub fn save_beat_grid(&self, track_id: i64, grid: &BeatGrid) -> Result<()> {
        let beats_blob: Vec<u8> = grid.beats.iter()
            .flat_map(|&s| s.to_le_bytes())
            .collect();
        self.conn.execute(
            "INSERT INTO beat_grids (track_id,anchor_sample,bpm,beats_blob,
                downbeat_offset,confidence,locked)
             VALUES (?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(track_id) DO UPDATE SET
                anchor_sample=excluded.anchor_sample,
                bpm=excluded.bpm,
                beats_blob=excluded.beats_blob,
                confidence=excluded.confidence,
                locked=excluded.locked",
            params![
                track_id,
                grid.anchor_sample as i64,
                grid.bpm,
                &beats_blob,
                grid.downbeat_offset as i64,
                grid.confidence as f64,
                grid.locked as i64,
            ],
        )?;
        Ok(())
    }

    pub fn load_beat_grid(&self, track_id: i64) -> Result<Option<BeatGrid>> {
        let mut stmt = self.conn.prepare(
            "SELECT anchor_sample,bpm,beats_blob,downbeat_offset,confidence,locked
             FROM beat_grids WHERE track_id=?1"
        )?;
        let mut rows = stmt.query(params![track_id])?;
        if let Some(row) = rows.next()? {
            let anchor: i64 = row.get(0)?;
            let bpm: f64    = row.get(1)?;
            let blob: Option<Vec<u8>> = row.get(2)?;
            let beats: Vec<u64> = blob.map(|b| {
                b.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect()
            }).unwrap_or_default();
            Ok(Some(BeatGrid {
                anchor_sample:   anchor as u64,
                bpm,
                beats,
                downbeat_offset: row.get::<_, i64>(3)? as u8,
                confidence:      row.get::<_, f64>(4)? as f32,
                locked:          row.get::<_, i64>(5)? != 0,
            }))
        } else {
            Ok(None)
        }
    }

    // ── Cue points ────────────────────────────────────────────────────────────

    pub fn save_cue_map(&self, track_id: i64, cues: &CueMap) -> Result<()> {
        self.conn.execute("DELETE FROM cue_points WHERE track_id=?1", params![track_id])?;
        for cue in cues.hot_cues.iter().flatten() {
            self.conn.execute(
                "INSERT INTO cue_points (track_id,slot,position,color_r,color_g,color_b,label,kind)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    track_id, cue.slot as i64, cue.position as i64,
                    cue.color.r as i64, cue.color.g as i64, cue.color.b as i64,
                    &cue.label, kind_str(cue.kind),
                ],
            )?;
        }
        Ok(())
    }

    pub fn load_cue_map(&self, track_id: i64) -> Result<CueMap> {
        let mut stmt = self.conn.prepare(
            "SELECT slot,position,color_r,color_g,color_b,label,kind
             FROM cue_points WHERE track_id=?1"
        )?;
        let mut map = CueMap::default();
        let rows = stmt.query_map(params![track_id], |row| {
            let slot: i64 = row.get(0)?;
            Ok(CuePoint {
                slot: slot as u8,
                position: row.get::<_, i64>(1)? as u64,
                color: Rgb {
                    r: row.get::<_, i64>(2)? as u8,
                    g: row.get::<_, i64>(3)? as u8,
                    b: row.get::<_, i64>(4)? as u8,
                },
                label: row.get(5)?,
                kind: parse_kind(&row.get::<_, String>(6)?),
            })
        })?;
        for row in rows {
            let cue = row?;
            if (cue.slot as usize) < 8 {
                map.hot_cues[cue.slot as usize] = Some(cue);
            }
        }
        Ok(map)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn row_to_track(row: &rusqlite::Row) -> rusqlite::Result<TrackInfo> {
    use std::path::PathBuf;
    Ok(TrackInfo {
        id:              row.get(0)?,
        path:            PathBuf::from(row.get::<_, String>(1)?),
        title:           row.get(2)?,
        artist:          row.get(3)?,
        album:           row.get(4)?,
        duration_frames: row.get::<_, i64>(5)? as u64,
        sample_rate:     row.get::<_, i64>(6)? as u32,
        channels:        row.get::<_, i64>(7)? as u8,
        bpm:             row.get(8)?,
        key:             row.get(9)?,
    })
}

fn kind_str(k: CueKind) -> &'static str {
    match k {
        CueKind::HotCue  => "hot",
        CueKind::LoopIn  => "loop_in",
        CueKind::LoopOut => "loop_out",
        CueKind::FadeIn  => "fade_in",
        CueKind::FadeOut => "fade_out",
    }
}

fn parse_kind(s: &str) -> CueKind {
    match s {
        "loop_in"  => CueKind::LoopIn,
        "loop_out" => CueKind::LoopOut,
        "fade_in"  => CueKind::FadeIn,
        "fade_out" => CueKind::FadeOut,
        _          => CueKind::HotCue,
    }
}
