use crate::{
    db,
    error::{AppError, Result},
    models::{CullingItem, CullingReport, JobProgress, MediaItem},
};
use chrono::{DateTime, Utc};
use image::{imageops::FilterType, DynamicImage};
use rayon::prelude::*;
use rusqlite::params;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    process::Command,
    sync::atomic::AtomicBool,
    sync::atomic::{AtomicUsize, Ordering},
};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug)]
struct VisualMetrics {
    hash: u64,
    sharpness: f64,
    exposure: f64,
    composition: f64,
}
fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}
fn hash_hex(v: u64) -> String {
    format!("{v:016x}")
}
#[cfg(test)]
fn parse_hash(s: &str) -> Option<u64> {
    u64::from_str_radix(s, 16).ok()
}

fn metrics(img: &DynamicImage) -> VisualMetrics {
    let gray = img.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut hash = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            hash <<= 1;
            if gray.get_pixel(x + 1, y)[0] > gray.get_pixel(x, y)[0] {
                hash |= 1
            }
        }
    }
    let sample = img.resize(640, 640, FilterType::Triangle).to_luma8();
    let (w, h) = sample.dimensions();
    let mut lap = Vec::with_capacity((w * h) as usize);
    let mut sum = 0f64;
    let mut clipped = 0usize;
    let mut thirds = 0f64;
    let mut edges = 0f64;
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let v = sample.get_pixel(x, y)[0] as f64;
            sum += v;
            if !(6.0..=249.0).contains(&v) {
                clipped += 1
            }
            let l = 4.0 * v
                - sample.get_pixel(x - 1, y)[0] as f64
                - sample.get_pixel(x + 1, y)[0] as f64
                - sample.get_pixel(x, y - 1)[0] as f64
                - sample.get_pixel(x, y + 1)[0] as f64;
            lap.push(l);
            let e = l.abs();
            edges += e;
            let nx = x as f64 / w as f64;
            let ny = y as f64 / h as f64;
            let d = ((nx - 1.0 / 3.0).abs().min((nx - 2.0 / 3.0).abs())
                + (ny - 1.0 / 3.0).abs().min((ny - 2.0 / 3.0).abs()))
            .max(0.02);
            thirds += e / d;
        }
    }
    let n = lap.len().max(1) as f64;
    let mean = lap.iter().sum::<f64>() / n;
    let sharpness = (lap.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n).sqrt();
    let brightness = sum / n;
    let clip = clipped as f64 / n;
    let exposure =
        ((1.0 - (brightness - 128.0).abs() / 128.0) * 100.0 - clip * 80.0).clamp(0.0, 100.0);
    let composition = if edges > 0.0 {
        (thirds / edges / 24.0 * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    VisualMetrics {
        hash,
        sharpness,
        exposure,
        composition,
    }
}
fn decode_image(source: &Path, cache: &Path) -> Result<DynamicImage> {
    if let Ok(img) = image::open(source) {
        return Ok(img);
    }
    let proxy = cache.join(format!("decode-{}.jpg", uuid::Uuid::new_v4()));
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            source.to_str().unwrap_or(""),
            "-frames:v",
            "1",
            proxy.to_str().unwrap(),
        ])
        .status()?;
    if !status.success() {
        return Err(AppError::Message(format!(
            "No se pudo decodificar {}",
            source.display()
        )));
    }
    let img = image::open(&proxy).map_err(|e| AppError::Message(e.to_string()))?;
    fs::remove_file(proxy).ok();
    Ok(img)
}
fn audio_present(path: &str) -> bool {
    Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
            path,
        ])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}
fn capture_seconds(m: &MediaItem) -> Option<i64> {
    m.manual_timestamp
        .as_ref()
        .or(m.captured_at.as_ref())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.naive_local().and_utc().timestamp())
}
fn score(v: &VisualMetrics) -> f64 {
    let sharp = (v.sharpness / 35.0 * 100.0).clamp(0.0, 100.0);
    sharp * 0.45 + v.exposure * 0.35 + v.composition * 0.20
}
fn target_ratio(value: &str) -> f64 {
    match value {
        "9:16" => 9.0 / 16.0,
        "1:1" => 1.0,
        _ => 16.0 / 9.0,
    }
}
fn aspect_bonus(media: &MediaItem, target: &str) -> f64 {
    match (media.width, media.height) {
        (Some(w), Some(h)) if h > 0 => {
            let delta = ((w as f64 / h as f64).ln() - target_ratio(target).ln()).abs();
            (18.0 - delta * 18.0).clamp(0.0, 18.0)
        }
        _ => 0.0,
    }
}
fn photo_reason(v: &VisualMetrics, selected: bool) -> String {
    if selected {
        let mut why = Vec::new();
        if v.sharpness > 18.0 {
            why.push("buena nitidez")
        }
        if v.exposure > 68.0 {
            why.push("exposición equilibrada")
        }
        if v.composition > 45.0 {
            why.push("composición clara")
        }
        if why.is_empty() {
            why.push("mejor equilibrio visual del grupo")
        }
        format!("Seleccionada: {}.", why.join(", "))
    } else if v.sharpness < 10.0 {
        "Rechazada: menor nitidez que otra toma similar.".into()
    } else if v.exposure < 45.0 {
        "Rechazada: exposición menos equilibrada que la toma elegida.".into()
    } else {
        "Rechazada: toma casi idéntica; se conservó la de mayor calidad medida.".into()
    }
}

fn sample_video(m: &MediaItem, cache: &Path) -> Result<(Vec<VisualMetrics>, Vec<f64>)> {
    let duration = m.duration_seconds.unwrap_or(0.0);
    if duration <= 0.1 {
        return Ok((vec![], vec![]));
    }
    let count = if duration > 60.0 { 10 } else { 6 };
    let mut out = Vec::new();
    let mut times = Vec::new();
    for i in 0..count {
        let t = ((i + 1) as f64 / (count + 1) as f64 * duration).min((duration - 0.05).max(0.0));
        let p = cache.join(format!("v{}-{i}.jpg", m.id));
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-ss",
                &format!("{t:.3}"),
                "-i",
                &m.path,
                "-frames:v",
                "1",
                "-vf",
                "scale=640:-2",
                p.to_str().unwrap(),
            ])
            .status()?;
        if status.success() {
            if let Ok(img) = image::open(&p) {
                out.push(metrics(&img));
                times.push(t)
            }
        }
        fs::remove_file(p).ok();
    }
    Ok((out, times))
}

pub fn analyze(app: &AppHandle, root: &Path, cancel: &AtomicBool) -> Result<CullingReport> {
    crate::media::repair_filename_timestamps(root)?;
    crate::location::analyze(app, root)?;
    let target_aspect = db::aspect_ratio(root);
    let media = db::list_media(root)?;
    if media.is_empty() {
        return Ok(CullingReport {
            items: vec![],
            selected_count: 0,
            rejected_count: 0,
            groups: 0,
        });
    }
    let cache = db::app_dir(root).join("cache").join("culling");
    fs::create_dir_all(&cache)?;
    let conn = db::connect(root)?;
    let cached_count = conn
        .query_row("SELECT COUNT(*) FROM culling", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0);
    // Duration, style, music, and aspect-ratio changes do not alter what is in
    // the source media. Reuse the complete content analysis and only rebuild
    // the plan/render on subsequent runs.
    if cached_count >= media.len() as i64 && crate::semantic::cache_complete(root) {
        app.emit(
            "job-progress",
            JobProgress {
                stage: "Análisis recuperado".into(),
                fraction: 0.97,
                message: "Reutilizando lo que Casi Cine ya aprendió de tus recuerdos".into(),
            },
        )
        .ok();
        return load(root);
    }
    let mut items = Vec::new();
    let mut photo_metrics: HashMap<i64, VisualMetrics> = HashMap::new();
    let mut video_frames: HashMap<i64, Vec<VisualMetrics>> = HashMap::new();
    let mut video_times: HashMap<i64, Vec<f64>> = HashMap::new();
    let analyzable = media
        .iter()
        .filter(|item| item.duplicate_of.is_none())
        .collect::<Vec<_>>();
    let completed = AtomicUsize::new(0);
    let analyzed = analyzable
        .par_iter()
        .map(|m| {
            if cancel.load(Ordering::Relaxed) {
                return (m.id, None, None);
            }
            let photo = if m.kind == "photo" {
                decode_image(Path::new(&m.path), &cache)
                    .ok()
                    .map(|image| metrics(&image))
            } else {
                None
            };
            let video = if m.kind == "video" {
                sample_video(m, &cache).ok()
            } else {
                None
            };
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            app.emit(
                "job-progress",
                JobProgress {
                    stage: "Analizando calidad en paralelo".into(),
                    fraction: done as f64 / analyzable.len().max(1) as f64 * 0.65,
                    message: format!("Revisando recuerdo {} de {}", done, analyzable.len()),
                },
            )
            .ok();
            (m.id, photo, video)
        })
        .collect::<Vec<_>>();
    if cancel.load(Ordering::Relaxed) {
        return Err(AppError::Message("Análisis cancelado con seguridad".into()));
    }
    for (id, photo, video) in analyzed {
        if let Some(metrics) = photo {
            photo_metrics.insert(id, metrics);
        }
        if let Some((frames, times)) = video {
            video_frames.insert(id, frames);
            video_times.insert(id, times);
        }
    }
    let mut assigned: HashSet<i64> = HashSet::new();
    let photos = media
        .iter()
        .filter(|m| m.kind == "photo" && m.duplicate_of.is_none())
        .collect::<Vec<_>>();
    for p in &photos {
        if assigned.contains(&p.id) {
            continue;
        }
        let Some(pm) = photo_metrics.get(&p.id) else {
            continue;
        };
        let mut group = vec![*p];
        assigned.insert(p.id);
        for q in &photos {
            if assigned.contains(&q.id) {
                continue;
            }
            let Some(qm) = photo_metrics.get(&q.id) else {
                continue;
            };
            let dt = match (capture_seconds(p), capture_seconds(q)) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => 99999,
            };
            let dist = hamming(pm.hash, qm.hash);
            if dist <= 7 || (dt <= 12 && dist <= 18) {
                group.push(*q);
                assigned.insert(q.id);
            }
        }
        let gid = format!("photo-{}", p.id);
        let best = group
            .iter()
            .max_by(|a, b| {
                (score(photo_metrics.get(&a.id).unwrap()) + aspect_bonus(a, &target_aspect))
                    .total_cmp(
                        &(score(photo_metrics.get(&b.id).unwrap())
                            + aspect_bonus(b, &target_aspect)),
                    )
            })
            .unwrap()
            .id;
        for g in group {
            let v = photo_metrics.get(&g.id).unwrap();
            items.push(CullingItem {
                media_id: g.id,
                source_path: g.path.clone(),
                filename: g.filename.clone(),
                kind: g.kind.clone(),
                selected: g.id == best,
                user_override: None,
                group_id: gid.clone(),
                group_kind: if hamming(pm.hash, v.hash) <= 7 {
                    "Fotos similares".into()
                } else {
                    "Ráfaga".into()
                },
                score: score(v),
                reason: photo_reason(v, g.id == best),
                start_seconds: 0.0,
                end_seconds: 3.0,
                duration_seconds: 3.0,
                captured_at: g.manual_timestamp.clone().or(g.captured_at.clone()),
                sharpness: Some(v.sharpness),
                exposure: Some(v.exposure),
                visual_hash: Some(hash_hex(v.hash)),
                has_audio: false,
            });
        }
    }
    for p in &photos {
        if !assigned.contains(&p.id) {
            items.push(CullingItem {
                media_id: p.id,
                source_path: p.path.clone(),
                filename: p.filename.clone(),
                kind: p.kind.clone(),
                selected: true,
                user_override: None,
                group_id: format!("unanalyzed-{}", p.id),
                group_kind: "Formato sin análisis visual".into(),
                score: 50.0,
                reason: "Seleccionada por cobertura cronológica; el formato no pudo decodificarse para comparar su calidad.".into(),
                start_seconds: 0.0,
                end_seconds: 3.0,
                duration_seconds: 3.0,
                captured_at: p.manual_timestamp.clone().or(p.captured_at.clone()),
                sharpness: None,
                exposure: None,
                visual_hash: None,
                has_audio: false,
            });
        }
    }
    // Exact duplicates are always grouped behind their canonical original.
    for m in media.iter().filter(|m| m.duplicate_of.is_some()) {
        let parent = m.duplicate_of.unwrap();
        items.push(CullingItem {
            media_id: m.id,
            source_path: m.path.clone(),
            filename: m.filename.clone(),
            kind: m.kind.clone(),
            selected: false,
            user_override: None,
            group_id: format!("exact-{parent}"),
            group_kind: "Duplicados exactos".into(),
            score: 0.0,
            reason: "Rechazado: contenido idéntico a otro archivo importado.".into(),
            start_seconds: 0.0,
            end_seconds: 0.0,
            duration_seconds: 0.0,
            captured_at: m.manual_timestamp.clone().or(m.captured_at.clone()),
            sharpness: None,
            exposure: None,
            visual_hash: None,
            has_audio: false,
        });
    }
    let videos = media
        .iter()
        .filter(|m| m.kind == "video" && m.duplicate_of.is_none())
        .collect::<Vec<_>>();
    let mut video_assigned = HashSet::new();
    for v in &videos {
        if video_assigned.contains(&v.id) {
            continue;
        }
        let frames = video_frames.get(&v.id).cloned().unwrap_or_default();
        let signature = frames.first().map(|x| x.hash);
        let mut group = vec![*v];
        video_assigned.insert(v.id);
        for q in &videos {
            if video_assigned.contains(&q.id) {
                continue;
            }
            let qf = video_frames.get(&q.id).cloned().unwrap_or_default();
            let similar = signature
                .zip(qf.first().map(|x| x.hash))
                .is_some_and(|(a, b)| hamming(a, b) <= 9);
            let dt = match (capture_seconds(v), capture_seconds(q)) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => 99999,
            };
            if similar && dt < 180 {
                group.push(*q);
                video_assigned.insert(q.id);
            }
        }
        let gid = format!("video-{}", v.id);
        let group_size = group.len();
        let mut ranked = group
            .iter()
            .map(|m| {
                let fs = video_frames.get(&m.id).cloned().unwrap_or_default();
                let avg = if fs.is_empty() {
                    0.0
                } else {
                    fs.iter().map(score).sum::<f64>() / fs.len() as f64
                };
                let diversity = fs
                    .windows(2)
                    .map(|w| hamming(w[0].hash, w[1].hash) as f64)
                    .sum::<f64>()
                    / (fs.len().saturating_sub(1).max(1) as f64);
                let audio = audio_present(&m.path);
                (
                    *m,
                    avg + diversity.min(20.0) * 0.55 + if audio { 5.0 } else { 0.0 },
                    audio,
                )
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| {
            (b.1 + aspect_bonus(b.0, &target_aspect))
                .total_cmp(&(a.1 + aspect_bonus(a.0, &target_aspect)))
        });
        let best = ranked.first().map(|x| x.0.id);
        for (m, total, audio) in ranked {
            let fs = video_frames.get(&m.id).cloned().unwrap_or_default();
            let ts = video_times.get(&m.id).cloned().unwrap_or_default();
            let best_i = fs
                .iter()
                .enumerate()
                .max_by(|a, b| score(a.1).total_cmp(&score(b.1)))
                .map(|x| x.0)
                .unwrap_or(0);
            let center = ts.get(best_i).copied().unwrap_or(0.0);
            let full = m.duration_seconds.unwrap_or(0.0);
            let length = if audio {
                full.min(15.0).max(5.0)
            } else {
                full.min(8.0).max(3.0)
            };
            let start = (center - length / 2.0).clamp(0.0, (full - length).max(0.0));
            let selected = Some(m.id) == best;
            let avg_sharp = if fs.is_empty() {
                None
            } else {
                Some(fs.iter().map(|x| x.sharpness).sum::<f64>() / fs.len() as f64)
            };
            let reason = if selected {
                format!(
                    "Seleccionado: mejor tramo visual{}; se conserva el audio original.",
                    if audio { " y audio disponible" } else { "" }
                )
            } else {
                "Rechazado: grabación repetida; otra toma ofrece mejor calidad y variedad visual."
                    .into()
            };
            items.push(CullingItem {
                media_id: m.id,
                source_path: m.path.clone(),
                filename: m.filename.clone(),
                kind: m.kind.clone(),
                selected,
                user_override: None,
                group_id: gid.clone(),
                group_kind: if group_size > 1 {
                    "Videos del mismo momento".into()
                } else {
                    "Video único".into()
                },
                score: total,
                reason,
                start_seconds: start,
                end_seconds: (start + length).min(full),
                duration_seconds: length.min(full),
                captured_at: m.manual_timestamp.clone().or(m.captured_at.clone()),
                sharpness: avg_sharp,
                exposure: if fs.is_empty() {
                    None
                } else {
                    Some(fs.iter().map(|x| x.exposure).sum::<f64>() / fs.len() as f64)
                },
                visual_hash: signature.map(hash_hex),
                has_audio: audio,
            });
        }
    }
    // Semantic analysis is deliberately separate from the technical pass. If
    // Ollama or Whisper is unavailable, the measured local fallback remains
    // usable and no semantic scores are fabricated.
    crate::semantic::analyze(app, root, &mut items, cancel)?;
    items.sort_by(|a, b| {
        a.captured_at
            .cmp(&b.captured_at)
            .then(a.media_id.cmp(&b.media_id))
    });
    let now = Utc::now().to_rfc3339();
    for i in &items {
        conn.execute("INSERT INTO culling(media_id,selected,user_override,group_id,group_kind,score,reason,start_seconds,end_seconds,duration_seconds,sharpness,exposure,visual_hash,has_audio,analyzed_at)VALUES(?1,?2,NULL,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)ON CONFLICT(media_id)DO UPDATE SET selected=excluded.selected,group_id=excluded.group_id,group_kind=excluded.group_kind,score=excluded.score,reason=excluded.reason,start_seconds=excluded.start_seconds,end_seconds=excluded.end_seconds,duration_seconds=excluded.duration_seconds,sharpness=excluded.sharpness,exposure=excluded.exposure,visual_hash=excluded.visual_hash,has_audio=excluded.has_audio,analyzed_at=excluded.analyzed_at",params![i.media_id,i.selected as i64,i.group_id,i.group_kind,i.score,i.reason,i.start_seconds,i.end_seconds,i.duration_seconds,i.sharpness,i.exposure,i.visual_hash,i.has_audio as i64,now])?;
    }
    report(items)
}
fn report(items: Vec<CullingItem>) -> Result<CullingReport> {
    let selected_count = items
        .iter()
        .filter(|i| i.user_override.unwrap_or(i.selected))
        .count();
    let groups = items
        .iter()
        .map(|i| &i.group_id)
        .collect::<HashSet<_>>()
        .len();
    Ok(CullingReport {
        rejected_count: items.len() - selected_count,
        selected_count,
        groups,
        items,
    })
}
pub fn load(root: &Path) -> Result<CullingReport> {
    let conn = db::connect(root)?;
    let mut s=conn.prepare("SELECT c.media_id,m.path,m.filename,m.kind,c.selected,c.user_override,c.group_id,c.group_kind,c.score,c.reason,c.start_seconds,c.end_seconds,c.duration_seconds,COALESCE(m.manual_timestamp,m.captured_at),c.sharpness,c.exposure,c.visual_hash,c.has_audio FROM culling c JOIN media m ON m.id=c.media_id ORDER BY COALESCE(m.manual_timestamp,m.captured_at),m.id")?;
    let rows = s
        .query_map([], |r| {
            Ok(CullingItem {
                media_id: r.get(0)?,
                source_path: r.get(1)?,
                filename: r.get(2)?,
                kind: r.get(3)?,
                selected: r.get::<_, i64>(4)? != 0,
                user_override: r.get::<_, Option<i64>>(5)?.map(|v| v != 0),
                group_id: r.get(6)?,
                group_kind: r.get(7)?,
                score: r.get(8)?,
                reason: r.get(9)?,
                start_seconds: r.get(10)?,
                end_seconds: r.get(11)?,
                duration_seconds: r.get(12)?,
                captured_at: r.get(13)?,
                sharpness: r.get(14)?,
                exposure: r.get(15)?,
                visual_hash: r.get(16)?,
                has_audio: r.get::<_, i64>(17)? != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    report(rows)
}
pub fn override_item(root: &Path, id: i64, selected: bool) -> Result<CullingReport> {
    db::connect(root)?.execute(
        "UPDATE culling SET user_override=?1 WHERE media_id=?2",
        params![selected as i64, id],
    )?;
    load(root)
}
pub fn reselect_for_aspect(root: &Path) -> Result<()> {
    let target = db::aspect_ratio(root);
    let media = db::list_media(root)?
        .into_iter()
        .map(|m| (m.id, m))
        .collect::<HashMap<_, _>>();
    let report = load(root)?;
    let mut groups: HashMap<String, Vec<CullingItem>> = HashMap::new();
    for item in report.items {
        if item.user_override.is_none() && !item.group_kind.contains("Duplicados exactos") {
            groups.entry(item.group_id.clone()).or_default().push(item)
        }
    }
    let conn = db::connect(root)?;
    for items in groups.values() {
        if let Some(best) = items.iter().max_by(|a, b| {
            let sa = a.score
                + media
                    .get(&a.media_id)
                    .map(|m| aspect_bonus(m, &target))
                    .unwrap_or(0.0);
            let sb = b.score
                + media
                    .get(&b.media_id)
                    .map(|m| aspect_bonus(m, &target))
                    .unwrap_or(0.0);
            sa.total_cmp(&sb)
        }) {
            for item in items {
                conn.execute(
                    "UPDATE culling SET selected=?1 WHERE media_id=?2",
                    params![(item.media_id == best.media_id) as i64, item.media_id],
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use image::GrayImage;
    use rusqlite::params;
    #[test]
    fn perceptual_distance_is_stable() {
        assert_eq!(hamming(0, 0), 0);
        assert_eq!(hamming(0, u64::MAX), 64)
    }
    #[test]
    fn metrics_reward_exposure() {
        let good = DynamicImage::ImageLuma8(GrayImage::from_pixel(32, 32, image::Luma([128])));
        let dark = DynamicImage::ImageLuma8(GrayImage::from_pixel(32, 32, image::Luma([0])));
        assert!(metrics(&good).exposure > metrics(&dark).exposure)
    }
    #[test]
    fn hashes_round_trip() {
        let n = 0xdeadbeef_u64;
        assert_eq!(parse_hash(&hash_hex(n)), Some(n));
    }
    #[test]
    fn user_override_restores_without_touching_media() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::connect(dir.path()).unwrap();
        conn.execute("INSERT INTO media(path,filename,kind,size_bytes,fingerprint,captured_at,timestamp_source,timestamp_uncertain)VALUES('/source/foto.jpg','foto.jpg','photo',42,'hash','2024-01-01T10:00:00Z','test',0)", []).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute("INSERT INTO culling(media_id,selected,group_id,group_kind,score,reason,start_seconds,end_seconds,duration_seconds,has_audio,analyzed_at)VALUES(?1,0,'g','Fotos similares',50,'duplicada',0,3,3,0,'now')",params![id]).unwrap();
        let restored = override_item(dir.path(), id, true).unwrap();
        assert_eq!(restored.selected_count, 1);
        assert_eq!(restored.items[0].user_override, Some(true));
        assert_eq!(restored.items[0].source_path, "/source/foto.jpg");
    }
}
