use crate::{
    db,
    error::{AppError, Result},
    models::{CullingItem, JobProgress},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter};

const MODEL: &str = "qwen3.5:9b";
const ANALYZER_VERSION: &str = "qwen3.5:9b-story-v5-full-storyboard";
// Rich storyboards are intentionally processed in small batches. This is
// slower, but leaves enough unified memory for Qwen on a 16 GB Apple Silicon Mac.
const BATCH: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Observation {
    pub description: String,
    pub scene_type: String,
    pub setting: String,
    #[serde(default)]
    pub people_count: u32,
    #[serde(default)]
    pub interaction_score: f64,
    #[serde(default)]
    pub nature_score: f64,
    #[serde(default)]
    pub journey_score: f64,
    #[serde(default)]
    pub memorable_score: f64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub transcript: String,
    #[serde(default)]
    pub event_start_fraction: f64,
    #[serde(default = "one")]
    pub event_end_fraction: f64,
}

fn one() -> f64 {
    1.0
}

#[derive(Deserialize)]
struct BatchResponse {
    items: Vec<BatchItem>,
}

#[derive(Deserialize)]
struct BatchItem {
    index: usize,
    #[serde(flatten)]
    observation: Observation,
}

pub fn ensure_schema(root: &Path) -> Result<()> {
    db::connect(root)?.execute_batch(
        "CREATE TABLE IF NOT EXISTS semantic_analysis(
          media_id INTEGER PRIMARY KEY,model TEXT NOT NULL,description TEXT NOT NULL,scene_type TEXT NOT NULL,
          setting TEXT NOT NULL,people_count INTEGER NOT NULL,interaction_score REAL NOT NULL,nature_score REAL NOT NULL,
          journey_score REAL NOT NULL,memorable_score REAL NOT NULL,tags_json TEXT NOT NULL,confidence REAL NOT NULL,
          transcript TEXT NOT NULL DEFAULT '',event_start_fraction REAL NOT NULL DEFAULT 0,
          event_end_fraction REAL NOT NULL DEFAULT 1,analyzed_at TEXT NOT NULL,
          FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE);",
    )?;
    let conn = db::connect(root)?;
    let _ = conn.execute(
        "ALTER TABLE semantic_analysis ADD COLUMN event_start_fraction REAL NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE semantic_analysis ADD COLUMN event_end_fraction REAL NOT NULL DEFAULT 1",
        [],
    );
    Ok(())
}

/// Reports whether every analyzable, non-duplicate item has an observation
/// produced by the current model and prompt version.
pub fn cache_complete(root: &Path) -> bool {
    if ensure_schema(root).is_err() {
        return false;
    }
    let Ok(conn) = db::connect(root) else {
        return false;
    };
    let expected = conn
        .query_row(
            "SELECT COUNT(*) FROM media WHERE duplicate_of IS NULL AND (kind='photo' OR COALESCE(duration_seconds,0)>0)",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0);
    let cached = conn
        .query_row(
            "SELECT COUNT(*) FROM semantic_analysis WHERE model=?1 AND media_id IN (SELECT id FROM media WHERE duplicate_of IS NULL AND (kind='photo' OR COALESCE(duration_seconds,0)>0))",
            [ANALYZER_VERSION],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0);
    expected > 0 && cached >= expected
}

fn ollama_ready() -> bool {
    Command::new("curl")
        .args(["-fsS", "--max-time", "2", "http://127.0.0.1:11434/api/tags"])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("qwen3.5"))
        .unwrap_or(false)
}

fn ensure_ollama() -> bool {
    if ollama_ready() {
        return true;
    }
    let _ = Command::new("ollama")
        .arg("serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if ollama_ready() {
            return true;
        }
    }
    false
}

fn visual(item: &CullingItem, cache: &Path, source_duration: f64) -> Option<Vec<u8>> {
    let output = cache.join(format!("semantic-{}.jpg", item.media_id));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    if item.kind == "video" {
        let duration = source_duration.max(item.end_seconds).max(0.1);
        for index in 0..12 {
            let time = ((index as f64 + 0.5) / 12.0 * duration).min(duration - 0.02);
            cmd.args(["-ss", &format!("{time:.3}"), "-i", &item.source_path]);
        }
        let mut filters = Vec::new();
        for index in 0..12 {
            filters.push(format!("[{index}:v]scale=240:135:force_original_aspect_ratio=decrease,pad=240:135:(ow-iw)/2:(oh-ih)/2[p{index}]"));
        }
        for row in 0..3 {
            filters.push(format!(
                "[p{}][p{}][p{}][p{}]hstack=inputs=4[r{row}]",
                row * 4,
                row * 4 + 1,
                row * 4 + 2,
                row * 4 + 3
            ));
        }
        filters.push("[r0][r1][r2]vstack=inputs=3".into());
        cmd.args([
            "-filter_complex",
            &filters.join(";"),
            "-frames:v",
            "1",
            output.to_str()?,
        ]);
    } else {
        cmd.args([
            "-i", &item.source_path, "-vf",
            "scale=720:720:force_original_aspect_ratio=decrease,pad=720:720:(ow-iw)/2:(oh-ih)/2:color=black",
            "-frames:v", "1", output.to_str()?,
        ]);
    }
    if !cmd.status().ok()?.success() {
        return None;
    }
    let bytes = fs::read(&output).ok();
    fs::remove_file(output).ok();
    bytes
}

fn ask_vision(images: &[Vec<u8>], cancel: &AtomicBool) -> Result<Vec<Observation>> {
    let mut encoded = Vec::with_capacity(images.len());
    for image in images {
        let mut process = Command::new("base64")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        process
            .stdin
            .as_mut()
            .ok_or_else(|| AppError::Message("No se pudo codificar una imagen".into()))?
            .write_all(image)?;
        let output = process.wait_with_output()?;
        if !output.status.success() {
            return Err(AppError::Message("No se pudo codificar una imagen".into()));
        }
        encoded.push(String::from_utf8_lossy(&output.stdout).replace(['\n', '\r'], ""));
    }
    let prompt = format!(
        r#"Analyze {count} chronological travel-media images. A 12-panel storyboard is one whole source video, sampled uniformly and arranged left-to-right, top-to-bottom. Read all panels as a temporal story, not as separate images. Find the strongest complete meaningful event and include its preparation, action, payoff, and visible reaction instead of selecting a random technical peak. Return strict JSON only: {{"items":[{{"index":1,"description":"maximum 18 words describing the visible event or action arc","scene_type":"home|airport|flight|road|lodging|pool|beach|nature|city|meal|activity|return|other","setting":"maximum 8 words; never guess a city","people_count":0,"interaction_score":0,"nature_score":0,"journey_score":0,"memorable_score":0,"tags":["at most","four tags"],"confidence":0.0,"event_start_fraction":0.0,"event_end_fraction":1.0}}]}}. Include exactly {count} items in 1-based order. Fractions are between 0 and 1 across the entire source video and must bound the strongest complete event; for a still photo use 0 and 1. Scores are integers 0-100. interaction rewards visible adult-child or group interaction, play, conversation, affection, reactions, or shared activity. memorable rewards a distinctive action plus its consequence or reaction. Tag action_payoff when the storyboard visibly contains preparation, action, and result; tag meaningful_reaction when later panels show a response to earlier action. nature rewards scenery. journey rewards packing, transport, arrival or departure. Tag incidental_service when the visible event is only restaurant/hotel staff serving, placing condiments, clearing a table, payment, or another routine transaction; do not reward it as meaningful interaction. Add bad_start or bad_end only when that portion is visibly accidental/unusable (lens covered, fabric close-up, extreme blur, floor/ceiling). State only visible facts. Never infer identity, relationship, emotion or location. Use adult, child or people—not mother, father, son, daughter or family. Keep every string short."#,
        count = images.len()
    );
    let request = serde_json::json!({
        "model": MODEL,
        "prompt": prompt,
        "images": encoded,
        "stream": false,
        "format": "json",
        "think": false,
        "keep_alive": "10m",
        "options": {"temperature": 0.1, "num_ctx": 4096, "num_predict": 1000}
    });
    let mut child = Command::new("curl")
        .args([
            "-fsS",
            "--max-time",
            "180",
            "-H",
            "Content-Type: application/json",
            "-X",
            "POST",
            "http://127.0.0.1:11434/api/generate",
            "--data-binary",
            "@-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| AppError::Message("No se pudo contactar Ollama".into()))?;
    input.write_all(&serde_json::to_vec(&request)?)?;
    drop(input);
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            child.kill().ok();
            child.wait().ok();
            return Err(AppError::Message("Análisis cancelado con seguridad".into()));
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut output) = child.stdout.take() {
        output.read_to_end(&mut stdout)?;
    }
    if let Some(mut output) = child.stderr.take() {
        output.read_to_end(&mut stderr)?;
    }
    if !status.success() {
        return Err(AppError::Message(format!(
            "Ollama no pudo analizar imágenes: {}",
            String::from_utf8_lossy(&stderr)
        )));
    }
    let envelope: Value = serde_json::from_slice(&stdout)?;
    let content = envelope
        .get("response")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Message("Ollama devolvió una respuesta inválida".into()))?;
    let parsed: BatchResponse = serde_json::from_str(content)?;
    let mut result = vec![Observation::default(); images.len()];
    for item in parsed.items {
        if item.index > 0 && item.index <= result.len() {
            result[item.index - 1] = item.observation;
        }
    }
    Ok(result)
}

fn normalize_scores(obs: &mut Observation) {
    for score in [
        &mut obs.interaction_score,
        &mut obs.nature_score,
        &mut obs.journey_score,
        &mut obs.memorable_score,
    ] {
        if *score > 0.0 && *score <= 10.0 {
            *score *= 10.0;
        }
        *score = score.clamp(0.0, 100.0);
    }
    obs.event_start_fraction = obs.event_start_fraction.clamp(0.0, 1.0);
    obs.event_end_fraction = obs.event_end_fraction.clamp(0.0, 1.0);
    if obs.event_end_fraction <= obs.event_start_fraction {
        obs.event_start_fraction = 0.0;
        obs.event_end_fraction = 1.0;
    }
}

fn save_observation(conn: &rusqlite::Connection, id: i64, obs: &Observation) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO semantic_analysis(media_id,model,description,scene_type,setting,people_count,interaction_score,nature_score,journey_score,memorable_score,tags_json,confidence,transcript,event_start_fraction,event_end_fraction,analyzed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",rusqlite::params![id,ANALYZER_VERSION,obs.description,obs.scene_type,obs.setting,obs.people_count,obs.interaction_score,obs.nature_score,obs.journey_score,obs.memorable_score,serde_json::to_string(&obs.tags)?,obs.confidence,obs.transcript,obs.event_start_fraction,obs.event_end_fraction,Utc::now().to_rfc3339()])?;
    Ok(())
}

fn apply_event_window(item: &mut CullingItem, source_duration: f64, obs: &Observation) {
    if item.kind != "video" || source_duration < 1.0 || obs.confidence < 0.35 {
        return;
    }
    let start_fraction = obs.event_start_fraction.clamp(0.0, 1.0);
    let end_fraction = obs.event_end_fraction.clamp(0.0, 1.0);
    if end_fraction <= start_fraction || end_fraction - start_fraction > 0.96 {
        return;
    }
    let mut start = (start_fraction * source_duration - 1.25).max(0.0);
    let mut end = (end_fraction * source_duration + 1.75).min(source_duration);
    if end - start < 5.0 {
        let center = (start + end) / 2.0;
        start = (center - 2.5).max(0.0);
        end = (start + 5.0).min(source_duration);
        start = (end - 5.0).max(0.0);
    }
    if end - start > 30.0 {
        let center = (start + end) / 2.0;
        start = (center - 15.0).max(0.0);
        end = (start + 30.0).min(source_duration);
        start = (end - 30.0).max(0.0);
    }
    item.start_seconds = start;
    item.end_seconds = end;
    item.duration_seconds = end - start;
}

fn whisper_model() -> Option<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(
            "Library/Application Support/openscreen/stt-models/whisper-ggml/ggml-small-q8_0.bin",
        ));
    }
    paths.extend([
        PathBuf::from("/opt/homebrew/share/whisper-cpp/models/ggml-small.bin"),
        PathBuf::from("/opt/homebrew/share/whisper-cpp/models/ggml-base.bin"),
    ]);
    paths.into_iter().find(|p| p.exists())
}

fn clean_transcript(value: &str) -> String {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    let unique = text
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect::<std::collections::HashSet<_>>()
        .len();
    if letters < 3 || unique < 3 {
        String::new()
    } else {
        text
    }
}

fn transcribe(item: &CullingItem, cache: &Path, model: &Path) -> String {
    if item.kind != "video" || !item.has_audio {
        return String::new();
    }
    let stem = cache.join(format!("speech-{}", item.media_id));
    let wav = stem.with_extension("wav");
    let extracted = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            &format!("{:.3}", item.start_seconds),
            "-t",
            &format!("{:.3}", item.duration_seconds.min(30.0)),
            "-i",
            &item.source_path,
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            wav.to_str().unwrap_or(""),
        ])
        .status()
        .is_ok_and(|s| s.success());
    if !extracted {
        return String::new();
    }
    let ok = Command::new("whisper-cli")
        .args([
            "-ng",
            "-m",
            model.to_str().unwrap_or(""),
            "-f",
            wav.to_str().unwrap_or(""),
            "-l",
            "auto",
            "-oj",
            "-of",
            stem.to_str().unwrap_or(""),
            "-np",
            "-t",
            "6",
        ])
        .status()
        .is_ok_and(|s| s.success());
    fs::remove_file(wav).ok();
    if !ok {
        return String::new();
    }
    let json_path = stem.with_extension("json");
    let text = fs::read_to_string(&json_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .map(|v| {
            v.get("transcription")
                .and_then(Value::as_array)
                .map(|segments| {
                    segments
                        .iter()
                        .filter_map(|s| s.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    fs::remove_file(json_path).ok();
    clean_transcript(text.trim())
}

pub fn analyze(
    app: &AppHandle,
    root: &Path,
    items: &mut [CullingItem],
    cancel: &AtomicBool,
) -> Result<()> {
    ensure_schema(root)?;
    if !ensure_ollama() {
        return Ok(());
    }
    let conn = db::connect(root)?;
    let mut cached = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT media_id,description,scene_type,setting,people_count,interaction_score,nature_score,journey_score,memorable_score,tags_json,confidence,transcript,event_start_fraction,event_end_fraction FROM semantic_analysis WHERE model=?1")?;
        let rows = stmt.query_map([ANALYZER_VERSION], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                Observation {
                    description: r.get(1)?,
                    scene_type: r.get(2)?,
                    setting: r.get(3)?,
                    people_count: r.get::<_, i64>(4)? as u32,
                    interaction_score: r.get(5)?,
                    nature_score: r.get(6)?,
                    journey_score: r.get(7)?,
                    memorable_score: r.get(8)?,
                    tags: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
                    confidence: r.get(10)?,
                    transcript: r.get(11)?,
                    event_start_fraction: r.get(12)?,
                    event_end_fraction: r.get(13)?,
                },
            ))
        })?;
        for row in rows {
            let (id, obs) = row?;
            cached.insert(id, obs);
        }
    }
    let cache_dir = db::app_dir(root).join("cache/semantic");
    fs::create_dir_all(&cache_dir)?;
    let source_durations = db::list_media(root)?
        .into_iter()
        .map(|media| (media.id, media.duration_seconds.unwrap_or(0.0)))
        .collect::<HashMap<_, _>>();
    let candidates = items
        .iter()
        .filter(|i| i.duration_seconds > 0.0 && !i.group_kind.contains("Duplicados exactos"))
        .map(|i| i.media_id)
        .collect::<Vec<_>>();
    let pending = candidates
        .iter()
        .filter(|id| !cached.contains_key(id))
        .copied()
        .collect::<Vec<_>>();
    for (batch_no, ids) in pending.chunks(BATCH).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::Message("Análisis cancelado con seguridad".into()));
        }
        let batch_items = ids
            .iter()
            .filter_map(|id| items.iter().find(|i| i.media_id == *id))
            .collect::<Vec<_>>();
        let visuals = batch_items
            .iter()
            .filter_map(|i| {
                visual(
                    i,
                    &cache_dir,
                    source_durations
                        .get(&i.media_id)
                        .copied()
                        .unwrap_or(i.end_seconds),
                )
                .map(|v| (i.media_id, v))
            })
            .collect::<Vec<_>>();
        if visuals.is_empty() {
            continue;
        }
        app.emit(
            "job-progress",
            JobProgress {
                stage: "Entendiendo la historia del viaje".into(),
                fraction: 0.65
                    + 0.22 * (batch_no as f64 / pending.chunks(BATCH).len().max(1) as f64),
                message: format!(
                    "Reconociendo escenas {}–{} de {}",
                    batch_no * BATCH + 1,
                    (batch_no * BATCH + visuals.len()).min(pending.len()),
                    pending.len()
                ),
            },
        )
        .ok();
        let images = visuals.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>();
        let observations = match ask_vision(&images, cancel) {
            Ok(values) => visuals
                .iter()
                .map(|(id, _)| *id)
                .zip(values)
                .collect::<Vec<_>>(),
            Err(error) => {
                if cancel.load(Ordering::Relaxed) {
                    return Err(error);
                }
                // A slow or unusually complex image must not discard the rest
                // of the journey. Retry each member independently and skip only
                // the one that still fails.
                let mut recovered = Vec::new();
                for (id, image) in &visuals {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(AppError::Message("Análisis cancelado con seguridad".into()));
                    }
                    if let Ok(mut values) = ask_vision(std::slice::from_ref(image), cancel) {
                        if let Some(value) = values.pop() {
                            recovered.push((*id, value));
                        }
                    }
                }
                recovered
            }
        };
        for (id, mut obs) in observations {
            if obs.description.trim().is_empty() || obs.scene_type.trim().is_empty() {
                continue;
            }
            normalize_scores(&mut obs);
            // Commit after every successful model response. Closing the app,
            // cancellation or a later timeout can never erase prior progress.
            save_observation(&conn, id, &obs)?;
            cached.insert(id, obs);
        }
    }
    // Align the clip to the complete visible event before transcription, so
    // dialogue and scoring describe the moment that will actually be edited.
    for item in items.iter_mut() {
        if let Some(obs) = cached.get(&item.media_id) {
            let source_duration = source_durations
                .get(&item.media_id)
                .copied()
                .unwrap_or(item.end_seconds);
            apply_event_window(item, source_duration, obs);
        }
    }
    let model = whisper_model();
    for (index, id) in candidates.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::Message("Análisis cancelado con seguridad".into()));
        }
        let Some(item) = items.iter().find(|i| i.media_id == *id) else {
            continue;
        };
        if let (Some(obs), Some(model)) = (cached.get_mut(id), model.as_deref()) {
            if obs.transcript.is_empty() {
                obs.transcript = transcribe(item, &cache_dir, model);
            }
        }
        if index % 8 == 0 {
            app.emit(
                "job-progress",
                JobProgress {
                    stage: "Escuchando conversaciones".into(),
                    fraction: 0.87 + 0.1 * (index as f64 / candidates.len().max(1) as f64),
                    message: format!("Revisando audio {} de {}", index + 1, candidates.len()),
                },
            )
            .ok();
        }
    }
    for (id, obs) in &cached {
        save_observation(&conn, *id, obs)?;
    }
    for item in items.iter_mut() {
        if let Some(obs) = cached.get(&item.media_id) {
            let semantic_bonus = obs.interaction_score * 0.22
                + obs.memorable_score * 0.16
                + obs.nature_score * 0.10
                + obs.journey_score * 0.12
                + if obs.tags.iter().any(|tag| tag == "action_payoff") {
                    18.0
                } else {
                    0.0
                }
                + if obs.tags.iter().any(|tag| tag == "meaningful_reaction") {
                    14.0
                } else {
                    0.0
                };
            item.score += semantic_bonus - obstruction_penalty(obs) - irrelevance_penalty(obs);
            if item.kind == "video" && item.duration_seconds > 5.0 {
                let trim = (item.duration_seconds * 0.25).min(4.0);
                if obs
                    .tags
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case("bad_start"))
                {
                    item.start_seconds += trim;
                    item.duration_seconds -= trim;
                }
                if obs
                    .tags
                    .iter()
                    .any(|tag| tag.eq_ignore_ascii_case("bad_end"))
                {
                    item.end_seconds = (item.end_seconds - trim).max(item.start_seconds + 1.0);
                    item.duration_seconds = item.end_seconds - item.start_seconds;
                }
            }
            let speech = if obs.transcript.is_empty() {
                String::new()
            } else {
                format!(
                    " Diálogo detectado: \"{}\".",
                    obs.transcript.chars().take(100).collect::<String>()
                )
            };
            item.reason = format!(
                "{} Escena: {}. {}{}",
                item.reason, obs.scene_type, obs.description, speech
            );
        }
    }
    // Semantic value participates in best-shot selection only after every
    // unique member of the group has been observed. Exact duplicates remain
    // rejected without wasting model time.
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if !item.group_kind.contains("Duplicados exactos") {
            groups.entry(item.group_id.clone()).or_default().push(index);
        }
    }
    for indices in groups.values() {
        if indices
            .iter()
            .any(|index| items[*index].user_override == Some(true))
        {
            for index in indices {
                if items[*index].user_override.is_none() {
                    items[*index].selected = false;
                }
            }
            continue;
        }
        let best = indices
            .iter()
            .copied()
            .filter(|index| items[*index].user_override != Some(false))
            .max_by(|a, b| items[*a].score.total_cmp(&items[*b].score));
        for index in indices {
            items[*index].selected = Some(*index) == best;
        }
    }
    for item in items.iter_mut() {
        if item.user_override.is_none()
            && cached
                .get(&item.media_id)
                .is_some_and(|obs| irrelevance_penalty(obs) >= 80.0)
        {
            item.selected = false;
            item.reason =
                "Rechazada: documento, menú o captura con poco valor narrativo para la película."
                    .into();
        }
    }
    let mut kept_photos: Vec<usize> = Vec::new();
    for index in 0..items.len() {
        if items[index].kind != "photo"
            || !items[index].selected
            || items[index].user_override.is_some()
        {
            continue;
        }
        let Some(current_obs) = cached.get(&items[index].media_id) else {
            continue;
        };
        let duplicate = kept_photos.iter().position(|previous| {
            let Some(previous_obs) = cached.get(&items[*previous].media_id) else {
                return false;
            };
            let close_in_time = match (
                items[index]
                    .captured_at
                    .as_deref()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok()),
                items[*previous]
                    .captured_at
                    .as_deref()
                    .and_then(|value| DateTime::parse_from_rfc3339(value).ok()),
            ) {
                (Some(a), Some(b)) => {
                    (a.naive_local() - b.naive_local()).num_seconds().abs() <= 1800
                }
                _ => index.abs_diff(*previous) <= 6,
            };
            close_in_time && semantically_similar(current_obs, previous_obs)
        });
        if let Some(position) = duplicate {
            let previous = kept_photos[position];
            let meaningful =
                current_obs.interaction_score >= 70.0 || current_obs.memorable_score >= 82.0;
            if meaningful {
                kept_photos.push(index);
            } else if items[index].score > items[previous].score {
                items[previous].selected = false;
                items[previous].reason="Rechazada: otra foto cercana cuenta el mismo momento con mayor valor visual o narrativo.".into();
                kept_photos[position] = index;
            } else {
                items[index].selected = false;
                items[index].reason =
                    "Rechazada: repite semánticamente una foto cercana ya elegida.".into();
            }
        } else {
            kept_photos.push(index);
        }
    }
    Ok(())
}

pub fn obstruction_penalty(obs: &Observation) -> f64 {
    let text = format!("{} {}", obs.description, obs.tags.join(" ")).to_lowercase();
    if [
        "lens covered",
        "camera covered",
        "fabric close-up",
        "shirt close-up",
        "extreme blur",
        "accidental footage",
        "bad_start",
        "bad_end",
    ]
    .iter()
    .any(|term| text.contains(term))
    {
        90.0
    } else {
        0.0
    }
}

pub fn irrelevance_penalty(obs: &Observation) -> f64 {
    let text = format!("{} {}", obs.description, obs.tags.join(" ")).to_lowercase();
    if [
        "incidental_service",
        "placing condiment",
        "places condiment",
        "server places",
        "waiter places",
        "waitress places",
    ]
    .iter()
    .any(|term| text.contains(term))
    {
        return 95.0;
    }
    if obs.interaction_score >= 45.0 || obs.memorable_score >= 72.0 || obs.journey_score >= 65.0 {
        return 0.0;
    }
    if [
        "menu",
        "receipt",
        "invoice",
        "price list",
        "qr code",
        "screenshot",
        "text document",
        "document page",
        "restaurant menu",
    ]
    .iter()
    .any(|term| text.contains(term))
    {
        110.0
    } else {
        0.0
    }
}

pub(crate) fn semantically_similar(a: &Observation, b: &Observation) -> bool {
    if a.scene_type != b.scene_type {
        return false;
    }
    let words = |value: &Observation| {
        format!(
            "{} {} {}",
            value.description,
            value.setting,
            value.tags.join(" ")
        )
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() >= 4)
        .filter(|word| !["with", "from", "this", "that", "visible", "people"].contains(word))
        .map(str::to_string)
        .collect::<std::collections::HashSet<_>>()
    };
    let left = words(a);
    let right = words(b);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let intersection = left.intersection(&right).count() as f64;
    let union = left.union(&right).count() as f64;
    intersection / union >= 0.48
}

pub fn load(root: &Path) -> HashMap<i64, Observation> {
    let Ok(conn) = db::connect(root) else {
        return HashMap::new();
    };
    let Ok(mut stmt)=conn.prepare("SELECT media_id,description,scene_type,setting,people_count,interaction_score,nature_score,journey_score,memorable_score,tags_json,confidence,transcript,event_start_fraction,event_end_fraction FROM semantic_analysis") else{return HashMap::new()};
    stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            Observation {
                description: r.get(1)?,
                scene_type: r.get(2)?,
                setting: r.get(3)?,
                people_count: r.get::<_, i64>(4)? as u32,
                interaction_score: r.get(5)?,
                nature_score: r.get(6)?,
                journey_score: r.get(7)?,
                memorable_score: r.get(8)?,
                tags: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
                confidence: r.get(10)?,
                transcript: r.get(11)?,
                event_start_fraction: r.get(12)?,
                event_end_fraction: r.get(13)?,
            },
        ))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn journey(root: &Path) -> Vec<serde_json::Value> {
    let observations = load(root);
    let Ok(media) = db::list_media(root) else {
        return vec![];
    };
    let labels = HashMap::from([
        ("home", "Casa"),
        ("airport", "Aeropuerto"),
        ("flight", "Vuelo"),
        ("road", "Camino"),
        ("lodging", "Hospedaje"),
        ("pool", "Alberca"),
        ("beach", "Playa"),
        ("nature", "Naturaleza"),
        ("city", "Ciudad"),
        ("meal", "Comida"),
        ("activity", "Actividad"),
        ("return", "Regreso"),
        ("other", "Otro momento"),
    ]);
    let mut result: Vec<serde_json::Value> = Vec::new();
    for item in media {
        let Some(obs) = observations.get(&item.id) else {
            continue;
        };
        if obs.scene_type.is_empty() {
            continue;
        }
        if let Some(last) = result.last_mut() {
            if last.get("scene_type").and_then(Value::as_str) == Some(obs.scene_type.as_str()) {
                if let Some(count) = last.get("count").and_then(Value::as_u64) {
                    last["count"] = Value::from(count + 1);
                }
                continue;
            }
        }
        result.push(serde_json::json!({
            "scene_type": obs.scene_type,
            "label": labels.get(obs.scene_type.as_str()).copied().unwrap_or("Momento"),
            "description": obs.description,
            "count": 1
        }));
    }
    if result.len() > 18 {
        let last = result.len() - 1;
        (0..18)
            .map(|index| result[index * last / 17].clone())
            .collect()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_event_window, clean_transcript, irrelevance_penalty, semantically_similar,
        Observation,
    };
    use crate::models::CullingItem;

    #[test]
    fn rejects_non_speech_model_noise() {
        assert_eq!(clean_transcript("ʕ ʕ ʕ ʕ ʕ"), "");
        assert_eq!(clean_transcript("  Vamos a la playa  "), "Vamos a la playa");
    }
    #[test]
    fn menus_are_irrelevant_but_travel_documents_can_add_context() {
        let menu = Observation {
            description: "Close-up of a restaurant menu and prices".into(),
            ..Default::default()
        };
        let boarding = Observation {
            description: "Airline boarding pass".into(),
            journey_score: 90.0,
            ..Default::default()
        };
        assert!(irrelevance_penalty(&menu) > 100.0);
        assert_eq!(irrelevance_penalty(&boarding), 0.0);
    }
    #[test]
    fn recognizes_semantically_repeated_photos() {
        let a = Observation {
            description: "Child playing in a blue swimming pool".into(),
            setting: "outdoor pool".into(),
            scene_type: "pool".into(),
            tags: vec!["child".into(), "swimming".into()],
            ..Default::default()
        };
        let b = Observation {
            description: "Child swimming and playing in the blue pool".into(),
            setting: "outdoor pool".into(),
            scene_type: "pool".into(),
            tags: vec!["swimming".into(), "child".into()],
            ..Default::default()
        };
        assert!(semantically_similar(&a, &b));
    }

    #[test]
    fn event_window_keeps_preparation_action_and_reaction() {
        let mut item = CullingItem {
            media_id: 1,
            source_path: "video.mp4".into(),
            filename: "video.mp4".into(),
            kind: "video".into(),
            selected: true,
            user_override: None,
            group_id: "1".into(),
            group_kind: String::new(),
            score: 0.0,
            reason: String::new(),
            start_seconds: 0.0,
            end_seconds: 10.0,
            duration_seconds: 10.0,
            captured_at: None,
            sharpness: None,
            exposure: None,
            visual_hash: None,
            has_audio: true,
        };
        let observation = Observation {
            confidence: 0.9,
            event_start_fraction: 0.4,
            event_end_fraction: 0.5,
            ..Default::default()
        };
        apply_event_window(&mut item, 120.0, &observation);
        assert!((item.start_seconds - 46.75).abs() < 0.01);
        assert!((item.end_seconds - 61.75).abs() < 0.01);
        assert!(item.duration_seconds >= 5.0);
    }
}
