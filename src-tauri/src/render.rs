use crate::{
    db,
    error::{AppError, Result},
    models::{JobProgress, TimelineItem},
};
use chrono::DateTime;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Emitter};

fn escape_concat(path: &str) -> String {
    path.replace('\'', "'\\''")
}
fn has_audio(path: &str) -> bool {
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

fn local_seconds(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.naive_local().and_utc().timestamp())
}

fn location_titles(items: &[TimelineItem], root: &Path) -> Vec<Option<String>> {
    let Ok(conn) = db::connect(root) else {
        return vec![None; items.len()];
    };
    let Ok(mut statement) = conn.prepare(
        "SELECT m.id,COALESCE(m.manual_timestamp,m.captured_at),ml.label FROM media m LEFT JOIN media_location ml ON ml.media_id=m.id WHERE ml.label IS NOT NULL",
    ) else {
        return vec![None; items.len()];
    };
    let anchors = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map(|rows| rows.filter_map(|row| row.ok()).collect::<Vec<_>>())
        .unwrap_or_default();
    let assigned = items
        .iter()
        .map(|item| {
            anchors
                .iter()
                .find(|(id, _, _)| *id == item.media_id)
                .map(|(_, _, label)| label.clone())
                .or_else(|| {
                    let time = item.captured_at.as_deref().and_then(local_seconds)?;
                    anchors
                        .iter()
                        .filter_map(|(_, captured, label)| {
                            let distance = (time - local_seconds(captured.as_deref()?)?).abs();
                            (distance <= 7200).then_some((distance, label))
                        })
                        .min_by_key(|(distance, _)| *distance)
                        .map(|(_, label)| label.clone())
                })
        })
        .collect::<Vec<_>>();
    let mut stats: HashMap<&str, (usize, f64)> = HashMap::new();
    for (item, label) in items.iter().zip(&assigned) {
        if let Some(label) = label.as_deref() {
            let entry = stats.entry(label).or_default();
            entry.0 += 1;
            entry.1 += item.duration_seconds;
        }
    }
    let important = stats
        .into_iter()
        .filter(|(_, (count, duration))| *count >= 3 || *duration >= 12.0)
        .map(|(label, _)| label.to_string())
        .collect::<HashSet<_>>();
    let mut shown = HashSet::new();
    assigned
        .into_iter()
        .map(|label| label.filter(|value| important.contains(value) && shown.insert(value.clone())))
        .collect()
}

fn wrap_words(value: &str, width: usize, max_lines: usize) -> String {
    let mut lines = vec![String::new()];
    for word in value.split_whitespace() {
        let line = lines.last_mut().unwrap();
        if !line.is_empty() && line.chars().count() + word.chars().count() + 1 > width {
            if lines.len() >= max_lines {
                break;
            }
            lines.push(String::new());
        }
        let line = lines.last_mut().unwrap();
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.join("\n")
}

fn text_overlay(output: &Path, width: u32, height: u32, text: &str, _location: bool) -> bool {
    let binary = [
        "magick",
        "/opt/homebrew/bin/magick",
        "/usr/local/bin/magick",
    ]
    .into_iter()
    .find(|candidate| Command::new(candidate).arg("-version").output().is_ok());
    let Some(binary) = binary else { return false };
    let point = (height / 20).max(28);
    let x = (width / 18).max(28);
    let y = (height / 10).max(45);
    let clean = text.split(',').next().unwrap_or(text).trim();
    Command::new(binary)
        .args([
            "-size",
            &format!("{width}x{height}"),
            "xc:none",
            "-font",
            "/Library/Fonts/Arial Unicode.ttf",
            "-pointsize",
            &point.to_string(),
            "-kerning",
            "1.5",
            "-fill",
            "#000000A0",
            "-gravity",
            "southwest",
            "-annotate",
            &format!("+{}+{}", x + 2, y - 2),
            clean,
            "-fill",
            "white",
            "-annotate",
            &format!("+{x}+{y}"),
            clean,
            output.to_str().unwrap_or(""),
        ])
        .status()
        .is_ok_and(|status| status.success() && output.exists())
}

fn build_music_bed(
    work: &Path,
    tracks: &[PathBuf],
    duration: f64,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    let output = work.join("adaptive-music-bed.m4a");
    let segment = 30.0;
    let crossfade = 2.0;
    let count = (((duration - crossfade).max(1.0) / (segment - crossfade)).ceil() as usize).max(1);
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-y"]);
    for index in 0..count {
        command.args([
            "-stream_loop",
            "-1",
            "-t",
            &format!("{segment:.3}"),
            "-i",
            tracks[index % tracks.len()].to_str().unwrap_or(""),
        ]);
    }
    let mut filters = Vec::new();
    for index in 0..count {
        filters.push(format!(
            "[{index}:a]atrim=0:{segment:.3},asetpts=PTS-STARTPTS[m{index}]"
        ));
    }
    let final_label = if count == 1 {
        "m0".to_string()
    } else {
        let mut previous = "m0".to_string();
        for index in 1..count {
            let next = format!("mx{index}");
            filters.push(format!(
                "[{previous}][m{index}]acrossfade=d={crossfade}:c1=tri:c2=tri[{next}]"
            ));
            previous = next;
        }
        previous
    };
    command.args([
        "-filter_complex",
        &filters.join(";"),
        "-map",
        &format!("[{final_label}]"),
        "-t",
        &format!("{duration:.3}"),
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        output.to_str().unwrap_or(""),
    ]);
    let mut child = command.stdout(Stdio::null()).spawn()?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            child.kill().ok();
            return Err(AppError::Message(
                "Exportación cancelada con seguridad".into(),
            ));
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(AppError::Message(
                    "FFmpeg no pudo construir la banda sonora por capítulos".into(),
                ));
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(output)
}

fn run_music_mix(
    base: &Path,
    music: &Path,
    output: &Path,
    filter: &str,
    log_path: &Path,
    cancel: &AtomicBool,
) -> std::result::Result<(), String> {
    let log = fs::File::create(log_path).map_err(|error| error.to_string())?;
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
            base.to_str().unwrap_or(""),
            "-i",
            music.to_str().unwrap_or(""),
            "-filter_complex",
            filter,
            "-map",
            "0:v:0",
            "-map",
            "[a]",
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
            "-shortest",
            output.to_str().unwrap_or(""),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|error| error.to_string())?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            child.kill().ok();
            child.wait().ok();
            return Err("cancelled".into());
        }
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) if status.success() => return Ok(()),
            Some(_) => {
                return Err(fs::read_to_string(log_path)
                    .unwrap_or_else(|_| "FFmpeg terminó sin diagnóstico".into()))
            }
            None => std::thread::sleep(std::time::Duration::from_millis(200)),
        }
    }
}

pub fn export(
    app: &AppHandle,
    root: &Path,
    output: &Path,
    items: &[TimelineItem],
    width: u32,
    height: u32,
    music: &[PathBuf],
    show_location_titles: bool,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    if items.is_empty() {
        return Err(AppError::Message("La línea de tiempo está vacía".into()));
    }
    let work = db::app_dir(root)
        .join("renders")
        .join(format!("job-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&work)?;
    let total = items.len();
    let titles = if show_location_titles {
        location_titles(items, root)
    } else {
        vec![None; items.len()]
    };
    let mut normalized = Vec::new();
    let mut rendered_items = Vec::new();
    let mut skipped = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::Message(
                "Exportación cancelada con seguridad".into(),
            ));
        }
        app.emit(
            "job-progress",
            JobProgress {
                stage: "Preparando clips".into(),
                fraction: (idx as f64 / total as f64) * 0.65,
                message: format!("Preparando {} de {}", idx + 1, total),
            },
        )
        .ok();
        let dest = work.join(format!("clip-{idx:05}.mp4"));
        let location_overlay = titles[idx].as_ref().and_then(|label| {
            let path = work.join(format!("location-{idx:05}.png"));
            text_overlay(&path, width, height, &wrap_words(label, 28, 2), true).then_some(path)
        });
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
        let needs_silence = item.kind == "photo" || !has_audio(&item.source_path);
        if item.kind == "photo" {
            cmd.args([
                "-loop",
                "1",
                "-t",
                &format!("{:.3}", item.duration_seconds),
                "-i",
                &item.source_path,
            ]);
        } else {
            cmd.args([
                "-i",
                &item.source_path,
                // Output-side seeking is slower than input-side seeking, but
                // it cuts audio and video on the exact same decoded instant.
                // Fast keyframe seeking can leave phone footage about a second
                // out of sync when its streams have different start times.
                "-ss",
                &format!("{:.3}", item.start_seconds),
                "-t",
                &format!("{:.3}", item.duration_seconds),
            ]);
        }
        if needs_silence {
            cmd.args([
                "-f",
                "lavfi",
                "-t",
                &format!("{:.3}", item.duration_seconds),
                "-i",
                "anullsrc=channel_layout=stereo:sample_rate=48000",
            ]);
        }
        let mut overlay_inputs = Vec::new();
        if let Some(path) = location_overlay.as_ref() {
            overlay_inputs.push((path, true));
        }
        for (path, _) in &overlay_inputs {
            cmd.args([
                "-loop",
                "1",
                "-framerate",
                "30",
                "-i",
                path.to_str().unwrap_or(""),
            ]);
        }
        let base_vf = if item.framing_mode == "fill" {
            format!("scale={width}:{height}:force_original_aspect_ratio=increase,crop={width}:{height}:(iw-ow)*{:.3}:(ih-oh)*{:.3},setsar=1,fps=30",item.focal_x.clamp(0.0,1.0),item.focal_y.clamp(0.0,1.0))
        } else {
            format!("split=2[bg][fg];[bg]scale={width}:{height}:force_original_aspect_ratio=increase,crop={width}:{height},boxblur=20:2[soft];[fg]scale={width}:{height}:force_original_aspect_ratio=decrease[front];[soft][front]overlay=(W-w)/2:(H-h)/2,setsar=1,fps=30")
        };
        let mut vf = format!("{base_vf},setpts=PTS-STARTPTS");
        if idx == 0 {
            vf.push_str(",fade=t=in:st=0:d=0.65");
        }
        if idx + 1 == total {
            vf.push_str(&format!(
                ",fade=t=out:st={:.3}:d=1.2",
                (item.duration_seconds - 1.2).max(0.0)
            ));
        }
        let mut audio_filter = if needs_silence {
            "asetpts=PTS-STARTPTS".to_string()
        } else {
            match item.audio_policy.as_str() {
                "mute" => "asetpts=PTS-STARTPTS,volume=0".to_string(),
                "ambient" => "asetpts=PTS-STARTPTS,highpass=f=45,volume=0.55,alimiter=limit=0.85".to_string(),
                _ => "asetpts=PTS-STARTPTS,highpass=f=60,acompressor=threshold=-18dB:ratio=3:attack=20:release=250:makeup=2,loudnorm=I=-16:TP=-1.5:LRA=9".to_string(),
            }
        };
        if !needs_silence && item.audio_policy != "mute" {
            audio_filter.push_str(",afade=t=in:st=0:d=0.4");
        }
        if idx + 1 == total {
            audio_filter.push_str(&format!(
                ",afade=t=out:st={:.3}:d=1.2",
                (item.duration_seconds - 1.2).max(0.0)
            ));
        }
        let audio_map = if needs_silence { "1:a:0" } else { "0:a:0" };
        if overlay_inputs.is_empty() {
            cmd.args(["-map", "0:v:0", "-vf", &vf]);
        } else {
            let mut filters = vec![format!("[0:v]{vf}[v0]")];
            let mut previous = "v0".to_string();
            let first_overlay_index = if needs_silence { 2 } else { 1 };
            for (overlay_index, (_, location)) in overlay_inputs.iter().enumerate() {
                let next = format!("v{}", overlay_index + 1);
                let enable = if *location {
                    format!("between(t,0,{:.3})", item.duration_seconds.min(4.0))
                } else {
                    format!(
                        "between(t,0.25,{:.3})",
                        (item.duration_seconds - 0.2).max(0.25)
                    )
                };
                filters.push(format!(
                    "[{previous}][{}:v]overlay=0:0:enable='{enable}'[{next}]",
                    first_overlay_index + overlay_index
                ));
                previous = next;
            }
            cmd.args([
                "-filter_complex",
                &filters.join(";"),
                "-map",
                &format!("[{previous}]"),
            ]);
        }
        cmd.args([
            "-map",
            audio_map,
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-r",
            "30",
            "-af",
            &audio_filter,
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-shortest",
            dest.to_str().unwrap(),
        ]);
        let result = cmd.output()?;
        if !result.status.success() {
            app.emit(
                "job-progress",
                JobProgress {
                    stage: "Reintentando un recuerdo".into(),
                    fraction: (idx as f64 / total as f64) * 0.65,
                    message: format!(
                        "Intentando una conversión segura para {}",
                        item.source_path.split('/').next_back().unwrap_or("archivo")
                    ),
                },
            )
            .ok();
            let mut fallback = Command::new("ffmpeg");
            fallback.args(["-hide_banner", "-loglevel", "error", "-y"]);
            if item.kind == "photo" {
                fallback.args([
                    "-loop",
                    "1",
                    "-t",
                    &format!("{:.3}", item.duration_seconds),
                    "-i",
                    &item.source_path,
                ]);
            } else {
                fallback.args([
                    "-i",
                    &item.source_path,
                    "-ss",
                    &format!("{:.3}", item.start_seconds),
                    "-t",
                    &format!("{:.3}", item.duration_seconds),
                ]);
            }
            fallback.args(["-f","lavfi","-t",&format!("{:.3}",item.duration_seconds),"-i","anullsrc=channel_layout=stereo:sample_rate=48000","-map","0:v:0","-map","1:a:0","-vf",&format!("scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps=30"),"-c:v","libx264","-preset","veryfast","-crf","22","-pix_fmt","yuv420p","-r","30","-c:a","aac","-ar","48000","-ac","2","-shortest",dest.to_str().unwrap_or("")]);
            let retry = fallback.output()?;
            if !retry.status.success() {
                skipped.push(format!(
                    "{}\nPrimer intento: {}\nReintento seguro: {}",
                    item.source_path,
                    String::from_utf8_lossy(&result.stderr).trim(),
                    String::from_utf8_lossy(&retry.stderr).trim()
                ));
                continue;
            }
        }
        normalized.push(dest);
        rendered_items.push(item.clone());
    }
    if normalized.is_empty() {
        return Err(AppError::Message("Ningún archivo pudo convertirse a video. Revisa espacio disponible y la instalación de FFmpeg.".into()));
    }
    if !skipped.is_empty() {
        fs::write(work.join("skipped-media.log"), skipped.join("\n\n---\n\n"))?;
        fs::write(output.with_extension("skipped-media.txt"),format!("Casi Cine omitió {} archivo(s) después de dos intentos para poder terminar la película.\n\n{}",skipped.len(),skipped.iter().map(|entry|entry.lines().next().unwrap_or("")).collect::<Vec<_>>().join("\n")))?;
    }
    let list = work.join("concat.txt");
    let mut file = fs::File::create(&list)?;
    for p in &normalized {
        writeln!(file, "file '{}'", escape_concat(&p.to_string_lossy()))?
    }
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Uniendo la película".into(),
            fraction: 0.72,
            message: "Uniendo los momentos en orden".into(),
        },
    )
    .ok();
    let base_output = if !music.is_empty() {
        work.join("base.mp4")
    } else {
        output.to_path_buf()
    };
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "concat",
            "-safe",
            "0",
            "-i",
            list.to_str().unwrap(),
            "-c:v",
            "copy",
            // Re-encode the joined audio once and follow the movie clock.
            // This removes AAC priming gaps that otherwise accumulate at
            // every clip boundary while leaving the video untouched.
            "-af",
            "aresample=async=1:first_pts=0",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
            base_output
                .to_str()
                .ok_or_else(|| AppError::Message("Ruta de salida inválida".into()))?,
        ])
        .stdout(Stdio::null())
        .spawn()?;
    loop {
        if cancel.load(Ordering::Relaxed) {
            child.kill().ok();
            return Err(AppError::Message(
                "Exportación cancelada con seguridad".into(),
            ));
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                return Err(AppError::Message("FFmpeg no pudo finalizar el MP4".into()));
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    if !music.is_empty() {
        app.emit(
            "job-progress",
            JobProgress {
                stage: "Mezclando música".into(),
                fraction: 0.88,
                message: "Ajustando música debajo del audio original".into(),
            },
        )
        .ok();
        let total_duration = rendered_items
            .iter()
            .map(|item| item.duration_seconds)
            .sum::<f64>();
        let music_path = build_music_bed(&work, music, total_duration, cancel)?;
        let mut cursor = 0.0;
        let mut levels: Vec<(f64, f64, f64)> = Vec::new();
        for item in &rendered_items {
            let start = cursor;
            cursor += item.duration_seconds;
            let level = match item.audio_policy.as_str() {
                "dialogue" => 0.10,
                "ambient" => 0.16,
                "mute" => 0.25,
                _ => 0.14,
            };
            if let Some(previous) = levels
                .last_mut()
                .filter(|previous| (previous.2 - level).abs() < 0.001)
            {
                previous.1 = cursor;
            } else {
                levels.push((start, cursor, level));
            }
        }
        let mut expression = "0.20".to_string();
        for (start, end, level) in levels.iter().copied().rev() {
            expression = format!("if(between(t,{start:.3},{end:.3}),{level:.2},{expression})");
        }
        let fade_out = (total_duration - 2.0).max(0.0);
        let adaptive=format!("[1:a]volume='{expression}':eval=frame,afade=t=in:st=0:d=1.5,afade=t=out:st={fade_out:.3}:d=2[bg];[bg][0:a]sidechaincompress=threshold=0.025:ratio=10:attack=80:release=900[ducked];[0:a][ducked]amix=inputs=2:duration=first:normalize=0,loudnorm=I=-16:TP=-1.5:LRA=11[a]");
        let simple=format!("[1:a]volume=0.18,afade=t=in:st=0:d=1.5,afade=t=out:st={fade_out:.3}:d=2[bg];[bg][0:a]sidechaincompress=threshold=0.025:ratio=10:attack=80:release=900[ducked];[0:a][ducked]amix=inputs=2:duration=first:normalize=0,loudnorm=I=-16:TP=-1.5:LRA=11[a]");
        let mut diagnostics = Vec::new();
        let adaptive_result = if levels.len() <= 64 {
            run_music_mix(
                &base_output,
                &music_path,
                output,
                &adaptive,
                &work.join("mix-adaptive.log"),
                cancel,
            )
        } else {
            Err(format!("Se omitió la mezcla adaptativa: {} cambios de nivel exceden el límite seguro de 64.",levels.len()))
        };
        if let Err(error) = adaptive_result {
            if error == "cancelled" {
                return Err(AppError::Message(
                    "Exportación cancelada con seguridad".into(),
                ));
            }
            diagnostics.push(format!("Intento adaptativo:\n{error}"));
            app.emit(
                "job-progress",
                JobProgress {
                    stage: "Reintentando la mezcla".into(),
                    fraction: 0.94,
                    message: "Simplificando la música sin perder el render".into(),
                },
            )
            .ok();
            if let Err(simple_error) = run_music_mix(
                &base_output,
                &music_path,
                output,
                &simple,
                &work.join("mix-simple.log"),
                cancel,
            ) {
                if simple_error == "cancelled" {
                    return Err(AppError::Message(
                        "Exportación cancelada con seguridad".into(),
                    ));
                }
                diagnostics.push(format!("Intento simplificado:\n{simple_error}"));
                app.emit(
                    "job-progress",
                    JobProgress {
                        stage: "Finalizando sin música".into(),
                        fraction: 0.97,
                        message: "Conservando la película y su audio original".into(),
                    },
                )
                .ok();
                fs::copy(&base_output, output)?;
                fs::write(
                    output.with_extension("render-warning.txt"),
                    "La película se exportó correctamente con su audio original, pero sin banda sonora porque FFmpeg rechazó dos intentos de mezcla. Los detalles técnicos permanecen en la caché del proyecto.",
                )?;
            }
        }
        if !diagnostics.is_empty() {
            fs::write(work.join("mix-attempts.log"), diagnostics.join("\n\n"))?;
        }
    }
    let timeline_path = output.with_extension("timeline.json");
    fs::write(&timeline_path, serde_json::to_vec_pretty(&rendered_items)?)?;
    app.emit(
        "job-progress",
        JobProgress {
            stage: "Terminada".into(),
            fraction: 1.0,
            message: "Tu película está lista".into(),
        },
    )
    .ok();
    Ok(output.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[test]
    fn concat_escape_handles_quotes() {
        assert_eq!(escape_concat("a'b"), "a'\\''b");
    }

    #[test]
    fn ffmpeg_creates_valid_h264_mp4() {
        if Command::new("ffmpeg").arg("-version").output().is_err()
            || Command::new("ffprobe").arg("-version").output().is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("render.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=#34505a:s=320x180:d=0.4:r=30",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=channel_layout=stereo:sample_rate=48000",
                "-shortest",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
                output.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height",
                "-of",
                "json",
                output.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(probe.status.success());
        let json: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
        assert_eq!(
            json.pointer("/streams/0/codec_name")
                .and_then(|v| v.as_str()),
            Some("h264")
        );
        assert_eq!(
            json.pointer("/streams/0/width").and_then(|v| v.as_i64()),
            Some(320)
        );
        assert!(fs::metadata(output).unwrap().len() > 1_000);
    }

    #[test]
    fn local_text_overlay_can_be_burned_into_video() {
        if Command::new("ffmpeg").arg("-version").output().is_err()
            || Command::new("magick").arg("-version").output().is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let title = dir.path().join("title.png");
        assert!(text_overlay(
            &title,
            640,
            360,
            "Bacalar, Quintana Roo",
            true
        ));
        let output = dir.path().join("title.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=#34505a:s=640x360:d=0.5:r=30",
                "-loop",
                "1",
                "-framerate",
                "30",
                "-i",
                title.to_str().unwrap(),
                "-filter_complex",
                "[0:v][1:v]overlay=0:0:enable='between(t,0,0.4)'[v]",
                "-map",
                "[v]",
                "-t",
                "0.5",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                output.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        assert!(fs::metadata(output).unwrap().len() > 1_000);
    }

    #[test]
    fn ffmpeg_sidechain_mix_produces_normalized_audio() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("mix.m4a");
        let filter="[1:a]volume=0.20[bg];[bg][0:a]sidechaincompress=threshold=0.025:ratio=10:attack=80:release=900[ducked];[0:a][ducked]amix=inputs=2:duration=first:normalize=0,loudnorm=I=-16:TP=-1.5:LRA=11[a]";
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=220:duration=1:sample_rate=48000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1:sample_rate=48000",
                "-filter_complex",
                filter,
                "-map",
                "[a]",
                "-c:a",
                "aac",
                output.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        assert!(fs::metadata(output).unwrap().len() > 1_000);
    }

    #[test]
    fn builds_reusable_crossfaded_music_bed() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut tracks = Vec::new();
        for (index, frequency) in [220, 330].iter().enumerate() {
            let path = dir.path().join(format!("theme-{index}.wav"));
            let status = Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={frequency}:duration=2:sample_rate=48000"),
                    path.to_str().unwrap(),
                ])
                .status()
                .unwrap();
            assert!(status.success());
            tracks.push(path);
        }
        let output = build_music_bed(dir.path(), &tracks, 40.0, &AtomicBool::new(false)).unwrap();
        assert!(fs::metadata(output).unwrap().len() > 1_000);
    }
}
