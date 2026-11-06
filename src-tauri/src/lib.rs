mod culling;
mod db;
mod error;
mod location;
mod media;
mod models;
mod music;
mod render;
mod semantic;
mod timeline;
use models::{
    CleanupResult, CullingReport, Dependency, MediaItem, MusicEngineStatus, MusicRequest,
    MusicTrack, Project, TimelineItem,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tauri::{Emitter, Manager, State};

struct JobState(Arc<AtomicBool>);
fn recent_file(app: &tauri::AppHandle) -> error::Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| error::AppError::Message(e.to_string()))?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("recent-projects.json"))
}

fn remember_project(app: &tauri::AppHandle, root: &str) -> error::Result<()> {
    let file = recent_file(app)?;
    let mut roots: Vec<String> = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    roots.retain(|value| value != root);
    roots.insert(0, root.to_string());
    roots.truncate(8);
    std::fs::write(file, serde_json::to_vec_pretty(&roots)?)?;
    Ok(())
}
fn configure_tool_path() {
    let existing = std::env::var_os("PATH").unwrap_or_else(OsString::new);
    let mut paths = vec![
        "/opt/homebrew/bin".to_string(),
        "/opt/homebrew/sbin".to_string(),
        "/usr/local/bin".to_string(),
        "/usr/local/sbin".to_string(),
        "/Applications/Ollama.app/Contents/Resources".to_string(),
    ];
    paths.extend(existing.to_string_lossy().split(':').map(str::to_string));
    let combined = paths
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(":");
    std::env::set_var("PATH", combined);
}

fn find(name: &str) -> Dependency {
    let out = Command::new("which").arg(name).output();
    let path = out
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());
    Dependency {
        name: match name {
            "ffmpeg" => "FFmpeg",
            "ffprobe" => "ffprobe",
            "exiftool" => "ExifTool",
            "magick" => "ImageMagick",
            "ollama" => "Ollama",
            "whisper-cli" => "whisper.cpp",
            x => x,
        }
        .into(),
        available: path.is_some(),
        path,
        detail: if matches!(name, "ffmpeg" | "ffprobe" | "exiftool" | "magick") {
            "Esencial para importar y renderizar"
        } else {
            "Opcional; habilita funciones de IA adicionales"
        }
        .into(),
    }
}

#[tauri::command]
fn check_dependencies() -> Vec<Dependency> {
    [
        "ffmpeg",
        "ffprobe",
        "exiftool",
        "magick",
        "ollama",
        "whisper-cli",
    ]
    .iter()
    .map(|x| find(x))
    .collect()
}
#[tauri::command]
fn create_project(app: tauri::AppHandle, name: String, root: String) -> error::Result<Project> {
    let project = db::create_project(&name, Path::new(&root))?;
    remember_project(&app, &project.root)?;
    Ok(project)
}
#[tauri::command]
fn create_default_project(app: tauri::AppHandle) -> error::Result<Project> {
    let base = app
        .path()
        .video_dir()
        .or_else(|_| app.path().document_dir())
        .map_err(|e| error::AppError::Message(e.to_string()))?
        .join("Casi Cine Projects");
    let folder = base.join(format!(
        "Mi película {}",
        chrono::Local::now().format("%Y-%m-%d %H-%M-%S")
    ));
    let project = db::create_project("Mi película", &folder)?;
    remember_project(&app, &project.root)?;
    Ok(project)
}
#[tauri::command]
fn open_project(app: tauri::AppHandle, root: String) -> error::Result<Project> {
    let project = db::open_project(Path::new(&root))?;
    remember_project(&app, &project.root)?;
    Ok(project)
}
#[tauri::command]
fn open_project_document(app: tauri::AppHandle, document_path: String) -> error::Result<Project> {
    let document = Path::new(&document_path);
    if document.extension().and_then(|value| value.to_str()) != Some("casicine") {
        return Err(error::AppError::Message(
            "Selecciona un archivo .casicine".into(),
        ));
    }
    let root = document
        .parent()
        .ok_or_else(|| error::AppError::Message("Documento de proyecto inválido".into()))?;
    let project = db::open_project(root)?;
    remember_project(&app, &project.root)?;
    Ok(project)
}
#[tauri::command]
fn recent_projects(app: tauri::AppHandle) -> error::Result<Vec<Project>> {
    let file = recent_file(&app)?;
    let roots: Vec<String> = std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let projects = roots
        .iter()
        .filter_map(|root| db::open_project(Path::new(root)).ok())
        .collect();
    Ok(projects)
}
#[tauri::command]
fn set_project_format(project_root: String, aspect_ratio: String) -> error::Result<Project> {
    let project = db::set_aspect_ratio(Path::new(&project_root), &aspect_ratio)?;
    culling::reselect_for_aspect(Path::new(&project_root))?;
    Ok(project)
}
#[tauri::command]
fn list_media(project_root: String) -> error::Result<Vec<MediaItem>> {
    db::list_media(Path::new(&project_root))
}
#[tauri::command]
fn clear_project_media(project_root: String) -> error::Result<()> {
    db::clear_media(Path::new(&project_root))
}
#[tauri::command]
fn cleanup_project(project_root: String) -> error::Result<CleanupResult> {
    db::cleanup_project(Path::new(&project_root))
}
#[tauri::command]
async fn import_media(
    app: tauri::AppHandle,
    state: State<'_, JobState>,
    project_root: String,
    paths: Vec<String>,
) -> error::Result<Vec<MediaItem>> {
    state.0.store(false, Ordering::Relaxed);
    let cancel = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        media::import(&app, Path::new(&project_root), &paths, &cancel)
    })
    .await
    .map_err(|e| error::AppError::Message(e.to_string()))?
}
#[tauri::command]
fn set_manual_timestamp(
    project_root: String,
    media_id: i64,
    timestamp: Option<String>,
) -> error::Result<()> {
    db::set_manual_timestamp(Path::new(&project_root), media_id, timestamp.as_deref())
}
#[tauri::command]
fn build_timeline(project_root: String, target_seconds: f64) -> error::Result<Vec<TimelineItem>> {
    timeline::build(Path::new(&project_root), target_seconds)
}
#[tauri::command]
async fn analyze_culling(
    app: tauri::AppHandle,
    state: State<'_, JobState>,
    project_root: String,
) -> error::Result<CullingReport> {
    state.0.store(false, Ordering::Relaxed);
    let cancel = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        culling::analyze(&app, Path::new(&project_root), &cancel)
    })
    .await
    .map_err(|e| error::AppError::Message(e.to_string()))?
}

#[tauri::command]
async fn export_preview(
    app: tauri::AppHandle,
    state: State<'_, JobState>,
    project_root: String,
    items: Vec<TimelineItem>,
    width: u32,
    height: u32,
    music_paths: Vec<String>,
    location_titles: bool,
) -> error::Result<String> {
    state.0.store(false, Ordering::Relaxed);
    let cancel = state.0.clone();
    let output = db::app_dir(Path::new(&project_root)).join("renders/preview.mp4");
    let result = tauri::async_runtime::spawn_blocking(move || {
        render::export(
            &app,
            Path::new(&project_root),
            &output,
            &items,
            width,
            height,
            &music_paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
            location_titles,
            &cancel,
        )
    })
    .await
    .map_err(|e| error::AppError::Message(e.to_string()))??;
    Ok(result.to_string_lossy().into_owned())
}
#[tauri::command]
fn load_culling(project_root: String) -> error::Result<CullingReport> {
    culling::load(Path::new(&project_root))
}
#[tauri::command]
fn suggest_project_title(project_root: String) -> String {
    location::suggest_title(Path::new(&project_root))
}
#[tauri::command]
fn semantic_journey(project_root: String) -> Vec<serde_json::Value> {
    semantic::journey(Path::new(&project_root))
}
#[tauri::command]
fn override_culling(
    project_root: String,
    media_id: i64,
    selected: bool,
) -> error::Result<CullingReport> {
    culling::override_item(Path::new(&project_root), media_id, selected)
}
#[tauri::command]
fn cancel_job(state: State<JobState>) {
    state.0.store(true, Ordering::Relaxed)
}
#[tauri::command]
fn music_engine_status(app: tauri::AppHandle) -> MusicEngineStatus {
    music::status(&app)
}
#[tauri::command]
async fn install_music_engine(app: tauri::AppHandle) -> error::Result<MusicEngineStatus> {
    tauri::async_runtime::spawn_blocking(move || music::install(&app))
        .await
        .map_err(|e| error::AppError::Message(e.to_string()))?
}
#[tauri::command]
async fn generate_music(
    app: tauri::AppHandle,
    state: State<'_, JobState>,
    project_root: String,
    request: MusicRequest,
) -> error::Result<Vec<MusicTrack>> {
    state.0.store(false, Ordering::Relaxed);
    let cancel = state.0.clone();
    tauri::async_runtime::spawn_blocking(move || {
        music::generate(&app, Path::new(&project_root), &request, &cancel)
    })
    .await
    .map_err(|e| error::AppError::Message(e.to_string()))?
}
#[tauri::command]
async fn export_movie(
    app: tauri::AppHandle,
    state: State<'_, JobState>,
    project_root: String,
    output_path: String,
    items: Vec<TimelineItem>,
    width: u32,
    height: u32,
    music_paths: Vec<String>,
    location_titles: bool,
) -> error::Result<String> {
    state.0.store(false, Ordering::Relaxed);
    let cancel = state.0.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        render::export(
            &app,
            Path::new(&project_root),
            Path::new(&output_path),
            &items,
            width,
            height,
            &music_paths.iter().map(PathBuf::from).collect::<Vec<_>>(),
            location_titles,
            &cancel,
        )
    })
    .await
    .map_err(|e| error::AppError::Message(e.to_string()))??;
    Ok(result.to_string_lossy().into_owned())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    configure_tool_path();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(JobState(Arc::new(AtomicBool::new(false))))
        .invoke_handler(tauri::generate_handler![
            check_dependencies,
            create_project,
            create_default_project,
            open_project,
            open_project_document,
            recent_projects,
            set_project_format,
            list_media,
            clear_project_media,
            cleanup_project,
            import_media,
            set_manual_timestamp,
            analyze_culling,
            load_culling,
            suggest_project_title,
            semantic_journey,
            override_culling,
            build_timeline,
            music_engine_status,
            install_music_engine,
            generate_music,
            export_movie,
            export_preview,
            cancel_job
        ])
        .build(tauri::generate_context!())
        .expect("error while building Casi Cine");
    app.run(|handle, event| {
        if let tauri::RunEvent::Opened { urls } = event {
            for url in urls {
                if let Ok(path) = url.to_file_path() {
                    if path.extension().and_then(|value| value.to_str()) == Some("casicine") {
                        handle
                            .emit("open-project-document", path.to_string_lossy().to_string())
                            .ok();
                    }
                }
            }
        }
    });
}
