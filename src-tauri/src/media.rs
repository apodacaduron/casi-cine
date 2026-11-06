use crate::{
    db,
    error::{AppError, Result},
    models::{JobProgress, MediaItem},
};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

const VIDEO_EXT: &[&str] = &[
    "mov", "mp4", "m4v", "mts", "m2ts", "avi", "mkv", "mpg", "mpeg", "3gp",
];
const PHOTO_EXT: &[&str] = &["jpg", "jpeg", "png", "heic", "heif"];

pub fn discover(inputs: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for input in inputs {
        let p = Path::new(input);
        if p.is_file() {
            if supported(p) {
                out.push(p.to_path_buf())
            }
        } else if p.is_dir() {
            for e in WalkDir::new(p)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if e.file_type().is_file() && supported(e.path()) {
                    out.push(e.into_path())
                }
            }
        }
    }
    out.sort();
    out
}
fn supported(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_lowercase();
            VIDEO_EXT.contains(&e.as_str()) || PHOTO_EXT.contains(&e.as_str())
        })
        .unwrap_or(false)
}
fn kind(p: &Path) -> &'static str {
    let e = p
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_lowercase();
    if VIDEO_EXT.contains(&e.as_str()) {
        "video"
    } else {
        "photo"
    }
}
fn fingerprint_with_progress(
    p: &Path,
    cancel: &AtomicBool,
    mut on_bytes: impl FnMut(u64),
) -> Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    let mut read = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::Message(
                "Importación cancelada con seguridad".into(),
            ));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        read += n as u64;
        on_bytes(read);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn timestamp_from_exif_row(row: &Value) -> Option<(String, String, bool)> {
    for (key, label) in [
        ("DateTimeOriginal", "EXIF DateTimeOriginal"),
        ("MediaCreateDate", "QuickTime MediaCreateDate"),
        ("CreateDate", "embedded CreateDate"),
    ] {
        if let Some(raw) = row.get(key).and_then(Value::as_str) {
            if let Some(ts) = parse_exif(raw) {
                let uncertain = !has_zone(raw);
                return Some((ts, label.into(), uncertain));
            }
        }
    }
    None
}

fn exif_timestamps(files: &[PathBuf]) -> HashMap<String, (String, String, bool)> {
    let mut timestamps = HashMap::new();
    for chunk in files.chunks(200) {
        let mut command = Command::new("exiftool");
        command.args([
            "-j",
            "-api",
            "QuickTimeUTC=1",
            "-DateTimeOriginal",
            "-CreateDate",
            "-MediaCreateDate",
        ]);
        command.args(chunk);
        let Ok(output) = command.output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let Ok(rows) = serde_json::from_slice::<Vec<Value>>(&output.stdout) else {
            continue;
        };
        for row in rows {
            let Some(source) = row.get("SourceFile").and_then(Value::as_str) else {
                continue;
            };
            if let Some(timestamp) = timestamp_from_exif_row(&row) {
                let key = Path::new(source)
                    .canonicalize()
                    .unwrap_or_else(|_| PathBuf::from(source))
                    .to_string_lossy()
                    .into_owned();
                timestamps.insert(key, timestamp);
            }
        }
    }
    timestamps
}
fn has_zone(s: &str) -> bool {
    s.ends_with('Z')
        || s.rfind('+').is_some_and(|i| i > 10)
        || s.get(10..).is_some_and(|x| x.contains('-'))
}
fn timestamp_from_filename(filename: &str) -> Option<String> {
    filename_datetime(filename).map(|value| value.and_utc().to_rfc3339())
}

fn filename_datetime(filename: &str) -> Option<NaiveDateTime> {
    let digits = filename.char_indices().find_map(|(index, _)| {
        let tail = &filename[index..];
        (tail.len() >= 15
            && tail.as_bytes().get(8) == Some(&b'_')
            && tail[..8].chars().all(|c| c.is_ascii_digit())
            && tail[9..15].chars().all(|c| c.is_ascii_digit()))
        .then(|| format!("{}{}", &tail[..8], &tail[9..15]))
    })?;
    NaiveDateTime::parse_from_str(&digits, "%Y%m%d%H%M%S").ok()
}

pub fn repair_filename_timestamps(root: &Path) -> Result<usize> {
    let conn = db::connect(root)?;
    let mut stmt = conn.prepare(
        "SELECT id,filename,kind,captured_at,timestamp_source FROM media WHERE manual_timestamp IS NULL",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    // Pixel filenames use a UTC-like camera clock while EXIF photos contain
    // local wall time. Learn the offset from nearby Pixel photos so companion
    // MP4s (including Motion Photos with zeroed QuickTime dates) inherit the
    // correct local position instead of appearing hours later.
    let pixel_samples = rows
        .iter()
        .filter(|(_, filename, kind, _, _)| kind == "photo" && filename.starts_with("PXL_"))
        .filter_map(|(_, filename, _, captured, _)| {
            let file_time = filename_datetime(filename)?;
            let captured = DateTime::parse_from_rfc3339(captured.as_deref()?)
                .ok()?
                .naive_local();
            Some((file_time, file_time - captured))
        })
        .collect::<Vec<_>>();
    let mut repaired = 0;
    for (id, filename, kind, _captured, source) in rows {
        let candidate = filename_datetime(&filename);
        let correction = if kind == "video" && filename.starts_with("VID_") {
            candidate.map(|value| {
                (
                    value.and_utc().to_rfc3339(),
                    "camera filename local time",
                    0_i64,
                )
            })
        } else if kind == "video" && filename.starts_with("PXL_") {
            candidate.and_then(|value| {
                pixel_samples
                    .iter()
                    .min_by_key(|(sample, _)| (value - *sample).num_seconds().abs())
                    .filter(|(sample, _)| (value - *sample).num_hours().abs() <= 12)
                    .map(|(sample, offset)| {
                        let exact = (value - *sample).num_seconds().abs() <= 2;
                        (
                            (value - *offset).and_utc().to_rfc3339(),
                            if exact {
                                "paired Pixel capture"
                            } else {
                                "inferred Pixel local time"
                            },
                            if exact { 0 } else { 1 },
                        )
                    })
            })
        } else if source == "filesystem modified" {
            candidate.map(|value| (value.and_utc().to_rfc3339(), "filename timestamp", 1_i64))
        } else {
            None
        };
        if let Some((timestamp, label, uncertain)) = correction {
            repaired += conn.execute(
                "UPDATE media SET captured_at=?1,timestamp_source=?2,timestamp_uncertain=?3 WHERE id=?4",
                params![timestamp, label, uncertain, id],
            )?;
        }
    }
    if repaired > 0 {
        db::write_project_document(root)?;
    }
    Ok(repaired)
}
pub fn parse_exif(raw: &str) -> Option<String> {
    let clean = raw.trim();
    for fmt in ["%Y:%m:%d %H:%M:%S%:z", "%Y-%m-%dT%H:%M:%S%:z"] {
        if let Ok(dt) = DateTime::parse_from_str(clean, fmt) {
            return Some(dt.to_rfc3339());
        }
    }
    for fmt in ["%Y:%m:%d %H:%M:%S", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(clean, fmt) {
            return Some(dt.and_utc().to_rfc3339());
        }
    }
    None
}
fn probe(path: &Path) -> (Option<f64>, Option<i64>, Option<i64>) {
    let args = [
        "-v",
        "error",
        "-show_entries",
        "format=duration:stream=width,height:stream_tags=rotate:stream_side_data=rotation",
        "-of",
        "json",
        path.to_str().unwrap_or(""),
    ];
    let out = [
        PathBuf::from("ffprobe"),
        PathBuf::from("/opt/homebrew/bin/ffprobe"),
        PathBuf::from("/usr/local/bin/ffprobe"),
    ]
    .into_iter()
    .find_map(|binary| Command::new(binary).args(args).output().ok());
    let Some(out) = out else {
        return (None, None, None);
    };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
        return (None, None, None);
    };
    let dur = v
        .pointer("/format/duration")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok());
    let stream = v
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|x| x.get("width").is_some()));
    let mut width = stream.and_then(|s| s.get("width")).and_then(Value::as_i64);
    let mut height = stream.and_then(|s| s.get("height")).and_then(Value::as_i64);
    let rotation = stream
        .and_then(|s| {
            s.pointer("/tags/rotate")
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<i64>().ok())
                .or_else(|| {
                    s.get("side_data_list")
                        .and_then(Value::as_array)
                        .and_then(|a| {
                            a.iter()
                                .find_map(|x| x.get("rotation").and_then(Value::as_i64))
                        })
                })
        })
        .unwrap_or(0)
        .abs()
        % 180;
    if rotation == 90 {
        std::mem::swap(&mut width, &mut height)
    }
    (dur, width, height)
}

pub fn import(
    app: &AppHandle,
    project_root: &Path,
    inputs: &[String],
    cancel: &AtomicBool,
) -> Result<Vec<MediaItem>> {
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Descubriendo archivos".into(),
            fraction: 0.01,
            message: "Recorriendo carpetas y subcarpetas…".into(),
        },
    )
    .ok();
    let conn = db::connect(project_root)?;
    let files = discover(inputs);
    let total_files = files.len();
    let total_bytes = files
        .iter()
        .filter_map(|path| path.metadata().ok().map(|meta| meta.len()))
        .sum::<u64>()
        .max(1);
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Preparando importación".into(),
            fraction: 0.04,
            message: format!("Encontramos {total_files} fotos y videos"),
        },
    )
    .ok();
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Leyendo fechas y cámaras".into(),
            fraction: 0.045,
            message: format!("Leyendo metadatos de {total_files} archivos en lotes"),
        },
    )
    .ok();
    let timestamps = exif_timestamps(&files);
    let mut known: HashMap<String, i64> = HashMap::new();
    {
        let mut s = conn.prepare("SELECT fingerprint,id FROM media ORDER BY id")?;
        for row in s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (f, id) = row?;
            known.entry(f).or_insert(id);
        }
    }
    let mut processed_bytes = 0u64;
    for (index, path) in files.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::Message(
                "Importación cancelada con seguridad".into(),
            ));
        }
        let canonical = path.canonicalize()?;
        let path_s = canonical.to_string_lossy().into_owned();
        let existing = conn
            .query_row(
                "SELECT id,kind,duration_seconds FROM media WHERE path=?1",
                [&path_s],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<f64>>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((id, existing_kind, duration)) = existing {
            if existing_kind == "video" && duration.is_none() {
                let (duration, width, height) = probe(&canonical);
                conn.execute(
                    "UPDATE media SET duration_seconds=?1,width=?2,height=?3 WHERE id=?4",
                    params![duration, width, height, id],
                )?;
            }
            processed_bytes += canonical.metadata().map(|m| m.len()).unwrap_or(0);
            continue;
        }
        let meta = canonical.metadata()?;
        let filename = canonical
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("archivo")
            .to_string();
        let base_bytes = processed_bytes;
        let mut last_report = 0u64;
        let fp = fingerprint_with_progress(&canonical, cancel, |file_bytes| {
            if file_bytes.saturating_sub(last_report) >= 16 * 1024 * 1024
                || file_bytes == meta.len()
            {
                last_report = file_bytes;
                app.emit(
                    "job-progress",
                    JobProgress {
                        stage: "Importando material".into(),
                        fraction: 0.05
                            + ((base_bytes + file_bytes) as f64 / total_bytes as f64) * 0.9,
                        message: format!("Archivo {} de {} · {}", index + 1, total_files, filename),
                    },
                )
                .ok();
            }
        })?;
        let dup = known.get(&fp).copied();
        let (duration, width, height) = probe(&canonical);
        let (captured, source, uncertain) = match timestamps.get(&path_s).cloned() {
            Some((ts, source, uncertain)) => (Some(ts), source, uncertain),
            None => {
                if let Some(ts) = timestamp_from_filename(&filename) {
                    (Some(ts), "filename timestamp".into(), true)
                } else {
                    let ts = meta
                        .modified()
                        .ok()
                        .map(DateTime::<Utc>::from)
                        .map(|d| d.to_rfc3339());
                    (ts, "filesystem modified".into(), true)
                }
            }
        };
        conn.execute("INSERT INTO media(path,filename,kind,size_bytes,fingerprint,duration_seconds,width,height,captured_at,timestamp_source,timestamp_uncertain,duplicate_of) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![path_s,canonical.file_name().and_then(|x|x.to_str()).unwrap_or("media"),kind(&canonical),meta.len() as i64,fp,duration,width,height,captured,source,uncertain as i64,dup])?;
        if dup.is_none() {
            known.insert(fp, conn.last_insert_rowid());
        }
        processed_bytes += meta.len();
    }
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Importación completa".into(),
            fraction: 1.0,
            message: format!("{total_files} archivos listos para organizar"),
        },
    )
    .ok();
    db::write_project_document(project_root)?;
    repair_filename_timestamps(project_root)?;
    db::list_media(project_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_zoned_and_naive_exif() {
        assert!(parse_exif("2024:06:12 14:30:00-06:00")
            .unwrap()
            .contains("-06:00"));
        let naive = parse_exif("2024:06:12 14:30:00").unwrap();
        assert!(naive.ends_with('Z') || naive.ends_with("+00:00"));
    }
    #[test]
    fn parses_pixel_and_camera_filenames() {
        assert_eq!(
            timestamp_from_filename("PXL_20260831_164410209.mp4").as_deref(),
            Some("2026-08-31T16:44:10+00:00")
        );
        assert_eq!(
            timestamp_from_filename("VID_20260825_125358.mp4").as_deref(),
            Some("2026-08-25T12:53:58+00:00")
        );
    }
    #[test]
    fn repairs_mixed_phone_clocks_to_local_capture_order() {
        let dir = tempfile::tempdir().unwrap();
        db::create_project("test", dir.path()).unwrap();
        let conn = db::connect(dir.path()).unwrap();
        conn.execute("INSERT INTO media(path,filename,kind,size_bytes,fingerprint,captured_at,timestamp_source,timestamp_uncertain) VALUES('/pixel.jpg','PXL_20260825_144243798.jpg','photo',1,'a','2026-08-25T08:42:43+00:00','EXIF DateTimeOriginal',1)", []).unwrap();
        conn.execute("INSERT INTO media(path,filename,kind,size_bytes,fingerprint,captured_at,timestamp_source,timestamp_uncertain) VALUES('/pixel.mp4','PXL_20260825_144244558.mp4','video',1,'b','2026-08-25T14:42:44+00:00','filename timestamp',1)", []).unwrap();
        conn.execute("INSERT INTO media(path,filename,kind,size_bytes,fingerprint,captured_at,timestamp_source,timestamp_uncertain) VALUES('/poco.mp4','VID_20260825_143853.mp4','video',1,'c','2026-08-25T13:39:13-06:00','QuickTime MediaCreateDate',0)", []).unwrap();
        drop(conn);

        assert_eq!(repair_filename_timestamps(dir.path()).unwrap(), 2);
        let media = db::list_media(dir.path()).unwrap();
        let pixel = media.iter().find(|item| item.path == "/pixel.mp4").unwrap();
        let poco = media.iter().find(|item| item.path == "/poco.mp4").unwrap();
        assert_eq!(
            pixel.captured_at.as_deref(),
            Some("2026-08-25T08:42:44+00:00")
        );
        assert_eq!(pixel.timestamp_source, "paired Pixel capture");
        assert_eq!(
            poco.captured_at.as_deref(),
            Some("2026-08-25T14:38:53+00:00")
        );
        assert_eq!(poco.timestamp_source, "camera filename local time");
    }
    #[test]
    fn filters_extensions() {
        assert!(supported(Path::new("x.MOV")));
        assert!(supported(Path::new("x.heic")));
        assert!(!supported(Path::new("x.txt")));
    }
}
