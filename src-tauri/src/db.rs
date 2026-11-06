use crate::models::CleanupResult;
use crate::{
    error::{AppError, Result},
    models::{MediaItem, Project},
};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const PROJECT_DOCUMENT: &str = "Casi Cine Project.casicine";

pub fn app_dir(root: &Path) -> PathBuf {
    root.join(".casi-cine")
}
pub fn db_path(root: &Path) -> PathBuf {
    app_dir(root).join("project.sqlite3")
}
pub fn project_document_path(root: &Path) -> PathBuf {
    root.join(PROJECT_DOCUMENT)
}

pub fn write_project_document(root: &Path) -> Result<()> {
    let conn = connect(root)?;
    let project = conn.query_row(
        "SELECT id,name,root,created_at,updated_at FROM project LIMIT 1",
        [],
        |row| {
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "root": row.get::<_, String>(2)?,
                "created_at": row.get::<_, String>(3)?,
                "updated_at": row.get::<_, String>(4)?,
            }))
        },
    )?;
    let (media_count, total_bytes, videos, photos): (i64, i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*),COALESCE(SUM(size_bytes),0),COALESCE(SUM(kind='video'),0),COALESCE(SUM(kind='photo'),0) FROM media",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let document = serde_json::json!({
        "format": "casi-cine-project",
        "schema_version": 1,
        "project": project,
        "settings": {
            "aspect_ratio": setting(&conn, "aspect_ratio").unwrap_or_else(|| "16:9".into()),
            "format_confirmed": setting(&conn, "format_confirmed").as_deref() == Some("true")
        },
        "media_summary": {
            "items": media_count,
            "photos": photos,
            "videos": videos,
            "referenced_bytes": total_bytes
        },
        "database": ".casi-cine/project.sqlite3",
        "notice": "Este archivo no contiene copias de tus fotos o videos. Conserva esta carpeta y los originales en sus ubicaciones."
    });
    fs::write(
        project_document_path(root),
        serde_json::to_vec_pretty(&document)?,
    )?;
    Ok(())
}

pub fn connect(root: &Path) -> Result<Connection> {
    fs::create_dir_all(app_dir(root).join("cache"))?;
    fs::create_dir_all(app_dir(root).join("renders"))?;
    let conn = Connection::open(db_path(root))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
      CREATE TABLE IF NOT EXISTS project(id TEXT PRIMARY KEY,name TEXT NOT NULL,root TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS media(
        id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE,filename TEXT NOT NULL,kind TEXT NOT NULL,size_bytes INTEGER NOT NULL,
        fingerprint TEXT NOT NULL,duration_seconds REAL,width INTEGER,height INTEGER,captured_at TEXT,timestamp_source TEXT NOT NULL,
        timestamp_uncertain INTEGER NOT NULL DEFAULT 0,duplicate_of INTEGER,manual_timestamp TEXT,
        FOREIGN KEY(duplicate_of) REFERENCES media(id));
      CREATE INDEX IF NOT EXISTS media_chrono ON media(COALESCE(manual_timestamp,captured_at),path);")?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS culling(
      media_id INTEGER PRIMARY KEY,selected INTEGER NOT NULL,user_override INTEGER,group_id TEXT NOT NULL,group_kind TEXT NOT NULL,
      score REAL NOT NULL,reason TEXT NOT NULL,start_seconds REAL NOT NULL,end_seconds REAL NOT NULL,duration_seconds REAL NOT NULL,
      sharpness REAL,exposure REAL,visual_hash TEXT,has_audio INTEGER NOT NULL DEFAULT 0,analyzed_at TEXT NOT NULL,
      FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE);")?;
    Ok(conn)
}

pub fn create_project(name: &str, root: &Path) -> Result<Project> {
    if !root.exists() {
        fs::create_dir_all(root)?;
    }
    let conn = connect(root)?;
    let now = Utc::now().to_rfc3339();
    let p = Project {
        id: Uuid::new_v4().to_string(),
        name: name.to_string(),
        root: root.to_string_lossy().into_owned(),
        created_at: now.clone(),
        updated_at: now,
        aspect_ratio: "16:9".into(),
        format_confirmed: false,
    };
    conn.execute(
        "INSERT OR REPLACE INTO project(id,name,root,created_at,updated_at) VALUES(?1,?2,?3,?4,?5)",
        params![p.id, p.name, p.root, p.created_at, p.updated_at],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO settings(key,value) VALUES('aspect_ratio','16:9')",
        [],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO settings(key,value) VALUES('format_confirmed','false')",
        [],
    )?;
    write_project_document(root)?;
    Ok(p)
}

pub fn open_project(root: &Path) -> Result<Project> {
    if !db_path(root).exists() {
        return Err(AppError::Message(
            "Esta carpeta no contiene un proyecto de Casi Cine".into(),
        ));
    }
    let conn = connect(root)?;
    let project_result = conn.query_row(
        "SELECT id,name,root,created_at,updated_at FROM project LIMIT 1",
        [],
        |r| {
            Ok(Project {
                id: r.get(0)?,
                name: r.get(1)?,
                root: r.get(2)?,
                created_at: r.get(3)?,
                updated_at: r.get(4)?,
                aspect_ratio: "16:9".into(),
                format_confirmed: false,
            })
        },
    );
    let mut project = match project_result {
        Ok(project) => project,
        Err(_) => {
            // Recover projects created by an interrupted/older build when their
            // media database still exists, without ever touching source files.
            let now = Utc::now().to_rfc3339();
            let project = Project {
                id: Uuid::new_v4().to_string(),
                name: root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Mi película")
                    .to_string(),
                root: root.to_string_lossy().into_owned(),
                created_at: now.clone(),
                updated_at: now,
                aspect_ratio: "16:9".into(),
                format_confirmed: false,
            };
            conn.execute(
                "INSERT INTO project(id,name,root,created_at,updated_at) VALUES(?1,?2,?3,?4,?5)",
                params![
                    project.id,
                    project.name,
                    project.root,
                    project.created_at,
                    project.updated_at
                ],
            )?;
            project
        }
    };
    project.aspect_ratio = setting(&conn, "aspect_ratio").unwrap_or_else(|| "16:9".into());
    project.format_confirmed = setting(&conn, "format_confirmed").as_deref() == Some("true");
    if !project_document_path(root).exists() {
        drop(conn);
        write_project_document(root)?;
    }
    Ok(project)
}

fn setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
        r.get(0)
    })
    .ok()
}
pub fn aspect_ratio(root: &Path) -> String {
    connect(root)
        .ok()
        .and_then(|c| setting(&c, "aspect_ratio"))
        .unwrap_or_else(|| "16:9".into())
}
pub fn set_aspect_ratio(root: &Path, value: &str) -> Result<Project> {
    if !matches!(value, "16:9" | "9:16" | "1:1") {
        return Err(AppError::Message("Formato de película no válido".into()));
    }
    let conn = connect(root)?;
    conn.execute(
        "INSERT OR REPLACE INTO settings(key,value)VALUES('aspect_ratio',?1)",
        [value],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO settings(key,value)VALUES('format_confirmed','true')",
        [],
    )?;
    let project = open_project(root)?;
    write_project_document(root)?;
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aspect_ratio_is_required_and_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let created = create_project("Viaje", dir.path()).unwrap();
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(project_document_path(dir.path())).unwrap()).unwrap();
        assert_eq!(document["format"], "casi-cine-project");
        assert_eq!(document["project"]["id"], created.id);
        assert_eq!(created.aspect_ratio, "16:9");
        assert!(!created.format_confirmed);
        let updated = set_aspect_ratio(dir.path(), "9:16").unwrap();
        assert_eq!(updated.aspect_ratio, "9:16");
        assert!(updated.format_confirmed);
        assert_eq!(open_project(dir.path()).unwrap().aspect_ratio, "9:16");
        assert!(set_aspect_ratio(dir.path(), "4:3").is_err());
    }

    #[test]
    fn clearing_media_removes_dependents_but_preserves_sources() {
        let dir = tempfile::tempdir().unwrap();
        create_project("Viaje", dir.path()).unwrap();
        let source = dir.path().join("original.mp4");
        fs::write(&source, b"original").unwrap();
        let conn = connect(dir.path()).unwrap();
        conn.execute("INSERT INTO media(path,filename,kind,size_bytes,fingerprint,timestamp_source) VALUES(?1,'original.mp4','video',8,'abc','test')", [source.to_string_lossy().as_ref()]).unwrap();
        let media_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO culling(media_id,selected,group_id,group_kind,score,reason,start_seconds,end_seconds,duration_seconds,has_audio,analyzed_at) VALUES(?1,1,'g','unique',1,'test',0,1,1,0,'now')", [media_id]).unwrap();
        fs::write(app_dir(dir.path()).join("cache/item.tmp"), b"cache").unwrap();
        drop(conn);

        clear_media(dir.path()).unwrap();

        let conn = connect(dir.path()).unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM media", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM culling", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert!(app_dir(dir.path())
            .join("cache")
            .read_dir()
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn opening_recovers_an_interrupted_project_record() {
        let dir = tempfile::tempdir().unwrap();
        let conn = connect(dir.path()).unwrap();
        conn.execute("DELETE FROM project", []).unwrap();
        drop(conn);

        let recovered = open_project(dir.path()).unwrap();
        assert_eq!(recovered.root, dir.path().to_string_lossy());
        assert!(!recovered.id.is_empty());
        assert!(project_document_path(dir.path()).exists());
        assert_eq!(open_project(dir.path()).unwrap().id, recovered.id);
    }

    #[test]
    fn cleanup_removes_only_regenerable_files() {
        let dir = tempfile::tempdir().unwrap();
        create_project("Viaje", dir.path()).unwrap();
        let source = dir.path().join("familia.jpg");
        fs::write(&source, b"memory").unwrap();
        let conn = connect(dir.path()).unwrap();
        conn.execute("INSERT INTO media(path,filename,kind,size_bytes,fingerprint,timestamp_source) VALUES(?1,'familia.jpg','photo',6,'photo','test')", [source.to_string_lossy().as_ref()]).unwrap();
        drop(conn);
        fs::write(app_dir(dir.path()).join("cache/proxy.jpg"), vec![0; 128]).unwrap();
        fs::write(app_dir(dir.path()).join("renders/temp.mp4"), vec![0; 256]).unwrap();

        let result = cleanup_project(dir.path()).unwrap();

        assert_eq!(result.bytes_freed, 384);
        assert_eq!(result.files_removed, 2);
        assert_eq!(list_media(dir.path()).unwrap().len(), 1);
        assert_eq!(fs::read(source).unwrap(), b"memory");
    }
}

pub fn list_media(root: &Path) -> Result<Vec<MediaItem>> {
    let conn = connect(root)?;
    let mut stmt=conn.prepare("SELECT id,path,filename,kind,size_bytes,duration_seconds,width,height,captured_at,timestamp_source,timestamp_uncertain,duplicate_of,manual_timestamp FROM media ORDER BY COALESCE(manual_timestamp,captured_at) IS NULL,COALESCE(manual_timestamp,captured_at),path")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(MediaItem {
                id: r.get(0)?,
                path: r.get(1)?,
                filename: r.get(2)?,
                kind: r.get(3)?,
                size_bytes: r.get(4)?,
                duration_seconds: r.get(5)?,
                width: r.get(6)?,
                height: r.get(7)?,
                captured_at: r.get(8)?,
                timestamp_source: r.get(9)?,
                timestamp_uncertain: r.get::<_, i64>(10)? != 0,
                duplicate_of: r.get(11)?,
                manual_timestamp: r.get(12)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn set_manual_timestamp(root: &Path, id: i64, value: Option<&str>) -> Result<()> {
    connect(root)?.execute(
        "UPDATE media SET manual_timestamp=?1 WHERE id=?2",
        params![value, id],
    )?;
    Ok(())
}

pub fn clear_media(root: &Path) -> Result<()> {
    let mut conn = connect(root)?;
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM culling", [])?;
    tx.execute("UPDATE media SET duplicate_of=NULL", [])?;
    tx.execute("DELETE FROM media", [])?;
    tx.commit()?;

    for directory in [app_dir(root).join("cache"), app_dir(root).join("renders")] {
        if directory.exists() {
            fs::remove_dir_all(&directory)?;
        }
        fs::create_dir_all(directory)?;
    }
    write_project_document(root)?;
    Ok(())
}

fn directory_usage(path: &Path) -> (u64, u64) {
    if !path.exists() {
        return (0, 0);
    }
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .fold((0, 0), |(bytes, files), entry| {
            (
                bytes + entry.metadata().map(|meta| meta.len()).unwrap_or(0),
                files + 1,
            )
        })
}

pub fn cleanup_project(root: &Path) -> Result<CleanupResult> {
    // These directories contain only reproducible derivatives. Source media
    // paths, project settings and user decisions live in SQLite and are kept.
    let directories = [app_dir(root).join("cache"), app_dir(root).join("renders")];
    let (bytes_freed, files_removed) = directories
        .iter()
        .map(|path| directory_usage(path))
        .fold((0, 0), |(bytes, files), value| {
            (bytes + value.0, files + value.1)
        });
    for directory in directories {
        if directory.exists() {
            fs::remove_dir_all(&directory)?;
        }
        fs::create_dir_all(directory)?;
    }

    let conn = connect(root)?;
    let repaired_duplicates = conn.execute(
        "UPDATE media SET duplicate_of=NULL WHERE duplicate_of IS NOT NULL AND duplicate_of NOT IN (SELECT id FROM media)",
        [],
    )? as u64;
    let removed_culling = conn.execute(
        "DELETE FROM culling WHERE media_id NOT IN (SELECT id FROM media)",
        [],
    )? as u64;
    conn.execute_batch("PRAGMA optimize; VACUUM;")?;
    write_project_document(root)?;

    Ok(CleanupResult {
        bytes_freed,
        files_removed,
        repaired_references: repaired_duplicates + removed_culling,
    })
}
