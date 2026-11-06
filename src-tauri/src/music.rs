use crate::{
    db,
    error::{AppError, Result},
    models::{JobProgress, MusicEngineStatus, MusicRequest, MusicTrack},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};

fn size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}
fn source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
fn valid(p: &Path) -> bool {
    p.join(".venv/bin/python").exists() && p.join("checkpoints/acestep-v15-turbo").exists()
}
pub fn root(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(v) = std::env::var("CASI_CINE_ACESTEP_ROOT") {
        let p = PathBuf::from(v);
        if valid(&p) {
            return Some(p);
        }
    }
    let dev = source().join(".local-ai/ACE-Step-1.5");
    if valid(&dev) {
        return Some(dev);
    }
    app.path()
        .app_data_dir()
        .ok()
        .map(|p| p.join("ai/ACE-Step-1.5"))
        .filter(|p| valid(p))
}
pub fn status(app: &AppHandle) -> MusicEngineStatus {
    let r = root(app);
    MusicEngineStatus {
        installed: r.is_some(),
        model_bytes: r
            .as_ref()
            .map(|p| size(&p.join("checkpoints")))
            .unwrap_or(0),
        root: r.map(|p| p.to_string_lossy().into_owned()),
        engine: "ACE-Step 1.5 MLX".into(),
        license: "MIT. Verifica originalidad y respeta derechos de terceros.".into(),
        memory_note: "M1 Pro 16 GB: lote 1, DiT sin planificador y carga secuencial.".into(),
        download_note:
            "Instalación opcional de aproximadamente 10 GB. Internet solo durante la instalación."
                .into(),
    }
}
fn script(app: &AppHandle) -> PathBuf {
    let dev = source().join("scripts/acestep_generate.py");
    if dev.exists() {
        return dev;
    }
    app.path()
        .resource_dir()
        .unwrap_or_default()
        .join("scripts/acestep_generate.py")
}
pub fn install(app: &AppHandle) -> Result<MusicEngineStatus> {
    if root(app).is_some() {
        return Ok(status(app));
    }
    let target = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Message(e.to_string()))?
        .join("ai/ACE-Step-1.5");
    fs::create_dir_all(target.parent().unwrap())?;
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Instalando música local".into(),
            fraction: 0.05,
            message: "Descargando el motor opcional y sus modelos".into(),
        },
    )
    .ok();
    let installer = {
        let dev = source().join("scripts/install-acestep-macos.sh");
        if dev.exists() {
            dev
        } else {
            app.path()
                .resource_dir()
                .unwrap_or_default()
                .join("scripts/install-acestep-macos.sh")
        }
    };
    let result = Command::new("sh").arg(installer).arg(&target).output()?;
    if !result.status.success() {
        return Err(AppError::Message(format!(
            "No se pudo instalar ACE-Step: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Instalación terminada".into(),
            fraction: 1.0,
            message: "ACE-Step está listo para generar música sin conexión".into(),
        },
    )
    .ok();
    Ok(status(app))
}
fn make_prompt(mood: &str, duration: u32) -> String {
    let palette = match mood {
        "nostalgic" => {
            "nostalgic and tender, warm felt piano, gentle acoustic guitar, soft strings"
        }
        "adventurous" => {
            "adventurous and expansive, rhythmic acoustic guitar, hand percussion, warm strings"
        }
        "peaceful" => "peaceful and intimate, soft piano, airy pads, delicate guitar",
        "playful" => "playful and light, pizzicato strings, marimba, gentle hand percussion",
        _ => "cinematic and emotional, piano, acoustic guitar, restrained strings",
    };
    format!("Original instrumental family travel film score, {palette}, one continuous memorable theme with gradual development across {duration} seconds, natural dynamics, room for dialogue, no vocals, no choir, no spoken words")
}
pub fn generate(
    app: &AppHandle,
    project: &Path,
    req: &MusicRequest,
    cancel: &AtomicBool,
) -> Result<Vec<MusicTrack>> {
    let r = root(app).ok_or_else(|| {
        AppError::Message("ACE-Step no está instalado. La música con IA es opcional.".into())
    })?;
    let cache = db::app_dir(project).join("cache/music");
    fs::create_dir_all(&cache)?;
    let count = req.candidates.clamp(1, 3);
    let scene_note = crate::culling::load(project).ok().map(|report| {
        let selected = report.items.into_iter().filter(|item| item.user_override.unwrap_or(item.selected)).collect::<Vec<_>>();
        let photos = selected.iter().filter(|item| item.kind == "photo").count();
        let videos = selected.len().saturating_sub(photos);
        let audio = selected.iter().filter(|item| item.has_audio).count();
        format!(", shaped for a chronological edit containing {photos} photographs and {videos} video moments, with {audio} original-audio passages that need musical space")
    }).unwrap_or_default();
    let observations = crate::semantic::load(project);
    let (interaction, nature, journey) = if observations.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        let count = observations.len() as f64;
        let interaction = observations
            .values()
            .map(|value| value.interaction_score)
            .sum::<f64>()
            / count;
        let nature = observations
            .values()
            .map(|value| value.nature_score)
            .sum::<f64>()
            / count;
        let journey = observations
            .values()
            .map(|value| value.journey_score)
            .sum::<f64>()
            / count;
        (interaction, nature, journey)
    };
    let effective_mood = if req.mood == "automatic" {
        if journey >= 55.0 {
            "adventurous"
        } else if nature >= 55.0 && interaction < 55.0 {
            "peaceful"
        } else if interaction >= 65.0 {
            "playful"
        } else {
            "nostalgic"
        }
    } else {
        req.mood.as_str()
    };
    let semantic_direction = if observations.is_empty() {
        ", keep a warm restrained family-travel tone".to_string()
    } else {
        format!(", visual analysis suggests interaction {interaction:.0}/100, nature {nature:.0}/100 and travel movement {journey:.0}/100; use one shared melodic motif and tonal palette across all cues")
    };
    let mut tracks = Vec::new();
    for i in 0..count {
        let seed = req.seed.unwrap_or(20261103).wrapping_add(i as u32);
        let cue = match i {
            1 => ", scenic variation of the same theme, airier arrangement, slower pulse, leave space for natural ambience",
            2 => ", movement variation of the same theme, gently brighter rhythm for roads, arrivals and playful activity",
            _ => ", main intimate family theme, emotionally warm but understated",
        };
        let prompt = format!(
            "{}{}{}{}",
            make_prompt(effective_mood, req.duration_seconds),
            scene_note,
            semantic_direction,
            cue
        );
        let mut hash = Sha256::new();
        hash.update(format!("ace15|{prompt}|{}|{seed}", req.duration_seconds));
        let key = hex::encode(hash.finalize());
        let output = cache.join(format!("{}.wav", &key[..20]));
        let cached = output.exists();
        if !cached {
            app.emit(
                "job-progress",
                JobProgress {
                    stage: "Generando música local".into(),
                    fraction: i as f64 / count as f64,
                    message: match i {
                        1 => "Componiendo una variación para paisajes".into(),
                        2 => "Componiendo una variación para caminos y juego".into(),
                        _ => "Componiendo el tema principal del viaje".into(),
                    },
                },
            )
            .ok();
            let duration = req.duration_seconds.to_string();
            let seed_text = seed.to_string();
            let log_path = cache.join(format!("{}.log", &key[..20]));
            let log = fs::File::create(&log_path)?;
            let mut child = Command::new(r.join(".venv/bin/python"))
                .arg(script(app))
                .args([
                    "--sidecar-root",
                    r.to_str().unwrap(),
                    "--output",
                    output.to_str().unwrap(),
                    "--duration",
                    &duration,
                    "--prompt",
                    &prompt,
                    "--seed",
                    &seed_text,
                ])
                .stdout(Stdio::from(log.try_clone()?))
                .stderr(Stdio::from(log))
                .spawn()?;
            let started = Instant::now();
            let mut last_second = 0;
            let status = loop {
                if cancel.load(Ordering::Relaxed) {
                    child.kill().ok();
                    return Err(AppError::Message(
                        "Generación de música cancelada con seguridad".into(),
                    ));
                }
                if let Some(status) = child.try_wait()? {
                    break status;
                }
                let elapsed = started.elapsed().as_secs();
                if elapsed != last_second {
                    last_second = elapsed;
                    app.emit(
                        "job-progress",
                        JobProgress {
                            stage: "Generando música local".into(),
                            fraction: (elapsed as f64 / 180.0).min(0.92),
                            message: match i {
                                1 => "Dándole aire a la música de los paisajes".into(),
                                2 => "Añadiendo movimiento para caminos y juego".into(),
                                _ => "Afinando el tema principal del viaje".into(),
                            },
                        },
                    )
                    .ok();
                }
                std::thread::sleep(Duration::from_millis(300));
            };
            if !status.success() {
                let details = fs::read_to_string(&log_path)
                    .unwrap_or_else(|_| "No se generó un registro técnico.".into());
                let summary = if details.to_lowercase().contains("out of memory") {
                    "La generación musical agotó la memoria unificada. Cierra aplicaciones pesadas y vuelve a intentarlo; Casi Cine ahora usa segmentos de 30 segundos para reducir el consumo."
                } else {
                    "El motor local de música no pudo completar esta propuesta. Puedes continuar sin música o consultar el diagnóstico."
                };
                return Err(AppError::Message(format!(
                    "{summary}\n\nRegistro: {}\n\n{}",
                    log_path.display(),
                    details
                )));
            }
        }
        tracks.push(MusicTrack{path:output.to_string_lossy().into_owned(),prompt,mood:effective_mood.into(),duration_seconds:req.duration_seconds,seed,cached,engine:"ACE-Step 1.5 MLX".into(),disclaimer:"La música generada no tiene garantía de estar libre de reclamos de derechos o Content ID.".into()})
    }
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Música lista".into(),
            fraction: 1.0,
            message: "La banda sonora quedó guardada en caché".into(),
        },
    )
    .ok();
    Ok(tracks)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompts_are_instrumental_and_mood_specific() {
        let a = make_prompt("nostalgic", 300);
        let b = make_prompt("playful", 300);
        assert!(a.contains("no vocals"));
        assert_ne!(a, b);
    }
}
