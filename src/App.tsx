import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow, ProgressBarStatus } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  Check,
  ChevronRight,
  CircleAlert,
  Clock3,
  Film,
  FolderOpen,
  HardDrive,
  Image,
  MapPin,
  LoaderCircle,
  Play,
  Plus,
  RotateCcw,
  Settings2,
  Sparkles,
  Trash2,
  Volume2,
  X,
} from "lucide-react";
import { api, formatBytes, formatDuration } from "./lib";
import type {
  CullingItem,
  CullingReport,
  Dependency,
  ExportProgress,
  MediaItem,
  MusicEngineStatus,
  MusicTrack,
  Project,
  TimelineItem,
  JourneyChapter,
} from "./types";

type Screen =
  "welcome" | "import" | "format" | "create" | "progress" | "preview";

function humanizeError(details: string) {
  const value = details.toLowerCase();
  if (value.includes("out of memory") || value.includes("mps backend"))
    return "La Mac se quedó sin memoria disponible durante la generación musical. Cierra aplicaciones pesadas o continúa sin música y vuelve a intentarlo.";
  if (value.includes("no contiene un proyecto"))
    return "No encontramos los datos del proyecto en esa carpeta. Selecciona su archivo .casicine o la carpeta correcta.";
  if (value.includes("cancelad"))
    return "La operación se canceló de forma segura. Tus archivos originales no fueron modificados.";
  const firstLine = details
    .split("\n")
    .find((line) => line.trim())
    ?.trim();
  return firstLine && firstLine.length <= 220
    ? firstLine
    : "Ocurrió un problema local inesperado. Puedes copiar el diagnóstico para revisarlo sin perder tu proyecto.";
}

function recommendedDuration(media: MediaItem[]) {
  const unique = media.filter((item) => !item.duplicate_of);
  const videoSeconds = unique.reduce(
    (total, item) =>
      total + (item.kind === "video" ? item.duration_seconds || 0 : 0),
    0,
  );
  const photos = unique.filter((item) => item.kind === "photo").length;
  // A family film should preserve a generous share of good source material.
  // Similar-shot culling happens later, so raw library size informs the upper
  // bound without forcing every repeated photo into the movie.
  const seconds = videoSeconds * 0.28 + Math.min(photos, 200) * 1.25;
  return Math.round(Math.max(3, Math.min(45, seconds / 60)) * 2) / 2;
}

export default function App() {
  const [screen, setScreen] = useState<Screen>("welcome");
  const [project, setProject] = useState<Project | null>(null);
  const [media, setMedia] = useState<MediaItem[]>([]);
  const [timeline, setTimeline] = useState<TimelineItem[]>([]);
  const [culling, setCulling] = useState<CullingReport | null>(null);
  const [deps, setDeps] = useState<Dependency[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [errorDetails, setErrorDetails] = useState("");
  const [showErrorDetails, setShowErrorDetails] = useState(false);
  const [targetMinutes, setTargetMinutes] = useState(5);
  const [durationTouched, setDurationTouched] = useState(false);
  const [progress, setProgress] = useState<ExportProgress>({
    stage: "",
    fraction: 0,
    message: "",
  });
  const [exported, setExported] = useState("");
  const [musicStatus, setMusicStatus] = useState<MusicEngineStatus | null>(
    null,
  );
  const [musicMode, setMusicMode] = useState<"none" | "generated" | "local">(
    "none",
  );
  const [localMusicPath, setLocalMusicPath] = useState("");
  const [musicMood, setMusicMood] = useState("automatic");
  const [musicTracks, setMusicTracks] = useState<MusicTrack[]>([]);
  const [aspectSelection, setAspectSelection] = useState("16:9");
  const [formatNext, setFormatNext] = useState<"create" | "preview">("create");
  const [exportFormats, setExportFormats] = useState<string[]>(["16:9"]);
  const [activeJob, setActiveJob] = useState<"import" | "movie">("movie");
  const [recentProjects, setRecentProjects] = useState<Project[]>([]);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [cleanupMessage, setCleanupMessage] = useState("");
  const [previewPath, setPreviewPath] = useState("");
  const [journey, setJourney] = useState<JourneyChapter[]>([]);
  const [suggestedTitle, setSuggestedTitle] = useState("Nuestro viaje");
  const [cancelReturn, setCancelReturn] = useState<Screen>("create");
  const [appVersion, setAppVersion] = useState("");
  const [locationTitles, setLocationTitles] = useState(true);

  useEffect(() => {
    getVersion().then(setAppVersion).catch(() => setAppVersion(""));
    api
      .dependencies()
      .then(setDeps)
      .catch(() => setDeps([]));
    api
      .musicStatus()
      .then(setMusicStatus)
      .catch(() => setMusicStatus(null));
    api
      .recentProjects()
      .then(setRecentProjects)
      .catch(() => setRecentProjects([]));
  }, []);
  useEffect(() => {
    const window = getCurrentWindow();
    if (screen === "progress" && progress.stage !== "done") {
      window
        .setProgressBar({
          status: ProgressBarStatus.Normal,
          progress: Math.max(
            0,
            Math.min(100, Math.round(progress.fraction * 100)),
          ),
        })
        .catch(() => undefined);
    } else {
      window
        .setProgressBar({ status: ProgressBarStatus.None, progress: 0 })
        .catch(() => undefined);
    }
  }, [screen, progress.fraction, progress.stage]);
  async function refreshDependencies() {
    await guarded(async () => setDeps(await api.dependencies()));
  }
  useEffect(() => {
    let off: (() => void) | undefined;
    listen<ExportProgress>("job-progress", (event) => {
      const next = event.payload;
      const fraction = next.stage.includes("Generando música")
        ? 0.76 + next.fraction * 0.08
        : next.stage === "Preparando clips"
          ? 0.84 + next.fraction * 0.1
          : next.stage === "Uniendo la película"
            ? 0.95
            : next.stage === "Mezclando música"
              ? 0.97
              : next.fraction;
      setProgress({ ...next, fraction });
    }).then((v) => (off = v));
    return () => off?.();
  }, []);
  useEffect(() => {
    let off: (() => void) | undefined;
    listen<string>("open-project-document", async (event) => {
      await guarded(async () =>
        activateProject(await api.openProjectDocument(event.payload)),
      );
    }).then((value) => (off = value));
    return () => off?.();
  }, []);
  useEffect(() => {
    const win = getCurrentWindow();
    let off: (() => void) | undefined;
    win
      .onDragDropEvent(async (event) => {
        if (event.payload.type === "drop" && project)
          await ingest(event.payload.paths);
      })
      .then((v) => (off = v));
    return () => off?.();
  }, [project]);

  const totals = useMemo(
    () => ({
      size: media.reduce((n, m) => n + m.size_bytes, 0),
      duration: media.reduce((n, m) => n + (m.duration_seconds || 0), 0),
      duplicates: media.filter((m) => m.duplicate_of).length,
      uncertain: media.filter((m) => m.timestamp_uncertain).length,
      photos: media.filter((m) => m.kind === "photo").length,
      videos: media.filter((m) => m.kind === "video").length,
      unprobedVideos: media.filter(
        (m) => m.kind === "video" && m.duration_seconds == null,
      ).length,
    }),
    [media],
  );

  async function guarded(fn: () => Promise<void>) {
    setBusy(true);
    setError("");
    setErrorDetails("");
    setShowErrorDetails(false);
    try {
      await fn();
    } catch (e) {
      const details = String(e);
      if (details.toLowerCase().includes("cancelad")) {
        setProgress({
          stage: "cancelled",
          fraction: 0,
          message: "Proceso cancelado",
        });
        return;
      }
      setErrorDetails(details);
      setError(humanizeError(details));
    } finally {
      setBusy(false);
    }
  }
  async function activateProject(p: Project) {
    setProject(p);
    setAspectSelection(p.aspect_ratio);
    setExportFormats([p.aspect_ratio]);
    const loaded = await api.listMedia(p.root);
    setMedia(loaded);
    setJourney(await api.semanticJourney(p.root));
    if (!durationTouched && loaded.length)
      setTargetMinutes(recommendedDuration(loaded));
    setRecentProjects(await api.recentProjects());
    setScreen("import");
  }
  async function newProject() {
    await guarded(async () =>
      activateProject(await api.createDefaultProject()),
    );
  }
  async function newProjectCustom() {
    const root = await open({
      directory: true,
      multiple: false,
      title: "Elige dónde guardar el proyecto",
    });
    if (!root) return;
    await guarded(async () => {
      const p = await api.createProject("Mi película", root);
      await activateProject(p);
    });
  }
  async function openExisting() {
    const root = await open({
      directory: false,
      multiple: false,
      title: "Abrir un proyecto de Casi Cine",
      filters: [{ name: "Proyecto de Casi Cine", extensions: ["casicine"] }],
    });
    if (!root) return;
    await guarded(async () => {
      const p = await api.openProjectDocument(root);
      await activateProject(p);
    });
  }
  async function openRecent(p: Project) {
    await guarded(async () => activateProject(await api.openProject(p.root)));
  }
  async function ingest(paths?: string[]) {
    if (!project) return;
    let picked = paths;
    if (!picked) {
      const value = await open({
        directory: true,
        multiple: true,
        title: "Selecciona carpetas o archivos",
      });
      if (!value) return;
      picked = Array.isArray(value) ? value : [value];
    }
    setActiveJob("import");
    setCancelReturn("import");
    setProgress({
      stage: "Importación",
      fraction: 0.01,
      message: "Preparando tus carpetas…",
    });
    setScreen("progress");
    await guarded(async () => {
      const imported = await api.importMedia(project.root, picked!);
      setMedia(imported);
      if (!durationTouched) setTargetMinutes(recommendedDuration(imported));
      // Import is not a review task. Continue directly into the required
      // format choice; the material inventory remains available optionally.
      setFormatNext("create");
      setScreen("format");
    });
  }
  async function clearImportedMedia() {
    if (!project) return;
    await guarded(async () => {
      await api.clearProjectMedia(project.root);
      setMedia([]);
      setTimeline([]);
      setCulling(null);
    });
  }
  async function cleanupCurrentProject() {
    if (!project) return;
    await guarded(async () => {
      const result = await api.cleanupProject(project.root);
      setCleanupMessage(
        `${formatBytes(result.bytes_freed)} liberados · ${result.files_removed} archivos temporales eliminados${
          result.repaired_references
            ? ` · ${result.repaired_references} referencias reparadas`
            : " · referencias verificadas"
        }`,
      );
    });
  }
  async function plan() {
    if (!project) return;
    if (!exportFormats.length) {
      setError("Elige al menos un formato para crear tu película.");
      return;
    }
    setActiveJob("movie");
    setCancelReturn("create");
    setScreen("progress");
    setExported("");
    setProgress({
      stage: "Análisis local",
      fraction: 0,
      message: "Comparando fotos y videos",
    });
    await guarded(async () => {
      const report = await api.analyzeCulling(project.root);
      setCulling(report);
      setJourney(await api.semanticJourney(project.root));
      const filmTitle = await api.suggestProjectTitle(project.root);
      setSuggestedTitle(filmTitle);
      const plannedTimeline = await api.buildTimeline(
        project.root,
        targetMinutes * 60,
      );
      setTimeline(plannedTimeline);
      setPreviewPath("");
      let chosenMusicPaths =
        musicMode === "local" && localMusicPath ? [localMusicPath] : [];
      if (musicMode === "generated" && musicStatus?.installed) {
        setProgress({
          stage: "Música local",
          fraction: 0.76,
          message: "Buscando el sonido de este viaje",
        });
        const tracks = await api.generateMusic(project.root, {
          mood: musicMood,
          duration_seconds: 35,
          candidates: 3,
          seed: null,
        });
        setMusicTracks(tracks);
        chosenMusicPaths = tracks.map((track) => track.path);
      }
      const workingFormat = project.aspect_ratio;
      const exportedPaths: string[] = [];
      try {
        for (let index = 0; index < exportFormats.length; index += 1) {
          const format = exportFormats[index];
          setProgress({
            stage: "Montando la película",
            fraction: 0.82 + (index / exportFormats.length) * 0.16,
            message: `Dando forma a la versión ${format}`,
          });
          await api.setProjectFormat(project.root, format);
          const formatTimeline = await api.buildTimeline(
            project.root,
            targetMinutes * 60,
          );
          const [width, height] =
            format === "9:16"
              ? [1080, 1920]
              : format === "1:1"
                ? [1080, 1080]
                : [1920, 1080];
          const safeTitle = filmTitle.replace(/[\\/:*?"<>|]/g, "-");
          const output = `${project.root}/${safeTitle} ${format.replace(":", "x")}.mp4`;
          exportedPaths.push(
            await api.exportMovie(
              project.root,
              output,
              formatTimeline.filter((item) => !item.excluded),
              width,
              height,
              chosenMusicPaths,
              locationTitles,
            ),
          );
        }
      } finally {
        const restored = await api.setProjectFormat(
          project.root,
          workingFormat,
        );
        setProject(restored);
      }
      setExported(exportedPaths[0] || "");
      setProgress({
        stage: "done",
        fraction: 1,
        message:
          exportedPaths.length === 1
            ? "Tu película está lista"
            : `${exportedPaths.length} versiones están listas`,
      });
    });
  }
  async function regenerateMusic() {
    if (!project) return;
    setActiveJob("movie");
    setCancelReturn("preview");
    setScreen("progress");
    setProgress({
      stage: "Música local",
      fraction: 0,
      message: "Creando una nueva propuesta",
    });
    await guarded(async () => {
      setMusicTracks(
        await api.generateMusic(project.root, {
          mood: musicMood,
          duration_seconds: targetMinutes * 60,
          candidates: 1,
          seed: Math.floor(Math.random() * 2_000_000_000),
        }),
      );
      setPreviewPath("");
      setScreen("preview");
    });
  }
  async function installMusic() {
    setActiveJob("movie");
    setCancelReturn("create");
    setScreen("progress");
    setProgress({
      stage: "Instalación opcional",
      fraction: 0.02,
      message: "Preparando ACE-Step. Descarga aproximada: 10 GB",
    });
    await guarded(async () => {
      const status = await api.installMusic();
      setMusicStatus(status);
      setScreen("create");
    });
  }
  async function pickLocalMusic() {
    const selected = await open({
      multiple: false,
      directory: false,
      title: "Elige una canción de esta Mac",
      filters: [
        { name: "Audio", extensions: ["mp3", "m4a", "wav", "aac", "flac"] },
      ],
    });
    if (!selected || Array.isArray(selected)) return;
    setLocalMusicPath(selected);
    setMusicMode("local");
    setPreviewPath("");
  }
  const soundtrackPaths =
    musicMode === "generated"
      ? musicTracks.map((track) => track.path)
      : musicMode === "local"
        ? localMusicPath
          ? [localMusicPath]
          : []
        : [];
  async function overrideSelection(mediaId: number, selected: boolean) {
    if (!project) return;
    await guarded(async () => {
      setCulling(await api.overrideCulling(project.root, mediaId, selected));
      setTimeline(await api.buildTimeline(project.root, targetMinutes * 60));
      setPreviewPath("");
    });
  }
  async function confirmFormat() {
    if (!project) return;
    await guarded(async () => {
      const updated = await api.setProjectFormat(project.root, aspectSelection);
      setProject(updated);
      setExportFormats((current) =>
        current.includes(aspectSelection)
          ? current
          : [...current, aspectSelection],
      );
      if (formatNext === "preview")
        setTimeline(await api.buildTimeline(project.root, targetMinutes * 60));
      setPreviewPath("");
      setScreen(formatNext);
    });
  }
  async function render() {
    if (!project) return;
    if (!exportFormats.length) {
      setError("Elige al menos un formato para exportar.");
      return;
    }
    const outputFolder = await open({
      directory: true,
      multiple: false,
      defaultPath: project.root,
      title: "Elige dónde guardar las versiones de tu película",
    });
    if (!outputFolder || Array.isArray(outputFolder)) return;
    setScreen("progress");
    setActiveJob("movie");
    setCancelReturn("preview");
    setExported("");
    setProgress({ stage: "render", fraction: 0, message: "Preparando clips" });
    await guarded(async () => {
      const workingFormat = project.aspect_ratio;
      const exportedPaths: string[] = [];
      try {
        for (let index = 0; index < exportFormats.length; index += 1) {
          const format = exportFormats[index];
          setProgress({
            stage: "render",
            fraction: index / exportFormats.length,
            message: `Preparando versión ${format}`,
          });
          await api.setProjectFormat(project.root, format);
          const formatTimeline = await api.buildTimeline(
            project.root,
            targetMinutes * 60,
          );
          const manualFraming = new Map(
            timeline.map((item) => [item.source_path, item]),
          );
          const prepared = formatTimeline.map((item) => {
            const manual = manualFraming.get(item.source_path);
            return manual
              ? {
                  ...item,
                  excluded: manual.excluded,
                  framing_mode: manual.framing_mode,
                  focal_x: manual.focal_x,
                  focal_y: manual.focal_y,
                }
              : item;
          });
          const [width, height] =
            format === "9:16"
              ? [1080, 1920]
              : format === "1:1"
                ? [1080, 1080]
                : [1920, 1080];
          const suffix = format.replace(":", "x");
          const output = `${outputFolder}/Casi Cine ${suffix}.mp4`;
          exportedPaths.push(
            await api.exportMovie(
              project.root,
              output,
              prepared.filter((i) => !i.excluded),
              width,
              height,
              soundtrackPaths,
              locationTitles,
            ),
          );
        }
      } finally {
        const restored = await api.setProjectFormat(
          project.root,
          workingFormat,
        );
        setProject(restored);
      }
      setExported(exportedPaths[0] || "");
      setProgress({
        stage: "done",
        fraction: 1,
        message:
          exportedPaths.length === 1
            ? "Tu película está lista"
            : `${exportedPaths.length} versiones están listas`,
      });
    });
  }
  async function generatePreview() {
    if (!project) return;
    const [width, height] =
      project.aspect_ratio === "9:16"
        ? [360, 640]
        : project.aspect_ratio === "1:1"
          ? [540, 540]
          : [640, 360];
    setActiveJob("movie");
    setCancelReturn("preview");
    setScreen("progress");
    setProgress({
      stage: "Vista previa",
      fraction: 0,
      message: "Preparando una vista previa ligera",
    });
    await guarded(async () => {
      const path = await api.exportPreview(
        project.root,
        timeline.filter((item) => !item.excluded),
        width,
        height,
        soundtrackPaths,
        locationTitles,
      );
      setPreviewPath(path);
      setScreen("preview");
    });
  }

  return (
    <main className="app-shell">
      <header className="titlebar" data-tauri-drag-region>
        <button className="brand" onClick={() => setScreen("welcome")}>
          <span className="brand-mark">
            <Film size={18} />
          </span>
          <span>Casi Cine</span>
        </button>
        {project && screen !== "progress" && (
          <nav className="project-nav" aria-label="Proyecto">
            <button onClick={() => setScreen("import")}>Material</button>
            <button onClick={() => setScreen("create")}>Película</button>
          </nav>
        )}
        {project && (
          <span className="project-name" data-tauri-drag-region>
            {project.name}
          </span>
        )}
        <button
          className="icon-button"
          aria-label="Configuración"
          aria-expanded={settingsOpen}
          onClick={() => setSettingsOpen((value) => !value)}
        >
          <Settings2 size={18} />
        </button>
        {settingsOpen && (
          <div className="settings-menu">
            <div className="settings-menu-heading">
              <strong>Mantenimiento</strong>
              <small>
                {project ? project.name : "Abre un proyecto para administrarlo"}
              </small>
            </div>
            <button
              className="settings-cleanup"
              onClick={cleanupCurrentProject}
              disabled={!project || busy}
            >
              {busy ? <LoaderCircle className="spin" /> : <Trash2 />}
              <span>
                <strong>Limpiar caché del proyecto</strong>
                <small>
                  Elimina proxies y renders temporales regenerables.
                </small>
              </span>
            </button>
            <p className="settings-safety">
              <Check /> Nunca elimina tus fotos o videos originales.
            </p>
            {cleanupMessage && (
              <div className="cleanup-result">{cleanupMessage}</div>
            )}
            <div className="settings-version">
              <span>Casi Cine</span>
              <span>{appVersion ? `Versión ${appVersion}` : "Aplicación local"}</span>
            </div>
          </div>
        )}
      </header>
      {error && (
        <div className="error-banner">
          <CircleAlert size={18} />
          <span>
            <strong>No pudimos completar esta acción</strong>
            <small>{error}</small>
            <span className="error-actions">
              <button onClick={() => setShowErrorDetails((value) => !value)}>
                {showErrorDetails ? "Ocultar detalles" : "Ver detalles"}
              </button>
              <button
                onClick={() => navigator.clipboard.writeText(errorDetails)}
              >
                Copiar diagnóstico
              </button>
            </span>
            {showErrorDetails && <pre>{errorDetails}</pre>}
          </span>
          <button className="error-close" onClick={() => setError("")}>
            <X size={16} />
          </button>
        </div>
      )}
      {screen === "welcome" && (
        <Welcome
          deps={deps}
          busy={busy}
          onNew={newProject}
          onNewCustom={newProjectCustom}
          onOpen={openExisting}
          recentProjects={recentProjects}
          onOpenRecent={openRecent}
          onRefresh={refreshDependencies}
        />
      )}
      {screen === "import" && (
        <ImportView
          media={media}
          suggestedTitle={suggestedTitle}
          totals={totals}
          busy={busy}
          onImport={() => ingest()}
          onClear={clearImportedMedia}
          onBack={() => setScreen("welcome")}
          onNext={() => {
            setFormatNext("create");
            setScreen("format");
          }}
        />
      )}
      {screen === "format" && (
        <FormatView
          selected={aspectSelection}
          setSelected={setAspectSelection}
          busy={busy}
          onBack={() =>
            setScreen(formatNext === "preview" ? "preview" : "import")
          }
          onConfirm={confirmFormat}
        />
      )}
      {screen === "create" && (
        <CreateView
          minutes={targetMinutes}
          setMinutes={(value: number) => {
            setDurationTouched(true);
            setTargetMinutes(value);
          }}
          recommendedMinutes={recommendedDuration(media)}
          musicStatus={musicStatus}
          musicMode={musicMode}
          aspectRatio={project?.aspect_ratio || "16:9"}
          setMusicMode={setMusicMode}
          musicMood={musicMood}
          setMusicMood={setMusicMood}
          localMusicPath={localMusicPath}
          locationTitles={locationTitles}
          setLocationTitles={setLocationTitles}
          onPickLocalMusic={pickLocalMusic}
          onInstallMusic={installMusic}
          onBack={() => {
            setFormatNext("create");
            setScreen("format");
          }}
          onCreate={plan}
          busy={busy}
        />
      )}
      {screen === "preview" && (
        <PreviewView
          items={timeline}
          setItems={(value: any) => {
            setTimeline(value);
            setPreviewPath("");
          }}
          culling={culling}
          musicTracks={musicTracks}
          musicMode={musicMode}
          localMusicPath={localMusicPath}
          onPickLocalMusic={pickLocalMusic}
          onRegenerateMusic={regenerateMusic}
          onOverride={overrideSelection}
          onChangeFormat={() => {
            setFormatNext("preview");
            setScreen("format");
          }}
          aspectRatio={project?.aspect_ratio || "16:9"}
          exportFormats={exportFormats}
          setExportFormats={setExportFormats}
          onBack={() => setScreen("create")}
          onRender={render}
          previewPath={previewPath}
          onGeneratePreview={generatePreview}
          journey={journey}
        />
      )}
      {screen === "progress" && (
        <ProgressView
          progress={progress}
          exported={exported}
          purpose={activeJob}
          media={media}
          onCancel={async () => {
            await api.cancel();
            setProgress({
              stage: "cancelling",
              fraction: progress.fraction,
              message: "Cancelando de forma segura…",
            });
          }}
          onReveal={() => exported && revealItemInDir(exported)}
          onBack={() => setScreen(cancelReturn)}
        />
      )}
    </main>
  );
}

function Welcome({
  deps,
  busy,
  onNew,
  onNewCustom,
  onOpen,
  recentProjects,
  onOpenRecent,
  onRefresh,
}: {
  deps: Dependency[];
  busy: boolean;
  onNew: () => void;
  onNewCustom: () => void;
  onOpen: () => void;
  recentProjects: Project[];
  onOpenRecent: (project: Project) => void;
  onRefresh: () => void;
}) {
  const ready = deps
    .filter((d) => ["FFmpeg", "ffprobe", "ExifTool", "ImageMagick"].includes(d.name))
    .every((d) => d.available);
  return (
    <section className="welcome page-enter">
      <div className="welcome-copy">
        <span className="eyebrow">Editor local para tus recuerdos</span>
        <h1>
          Tus recuerdos,
          <br />
          <em>hechos película.</em>
        </h1>
        <p>
          Crea una película familiar con tus fotos y videos. Todo sucede en esta
          Mac.
        </p>
        <div className="actions">
          <button className="primary" onClick={onNew} disabled={busy}>
            <Plus size={19} />
            Nuevo proyecto
          </button>
          <button className="secondary" onClick={onNewCustom}>
            <FolderOpen size={18} />
            Elegir ubicación
          </button>
          <button className="text-action" onClick={onOpen}>
            <FolderOpen size={18} />
            Abrir archivo .casicine
          </button>
        </div>
        {recentProjects.length > 0 && (
          <div className="recent-projects">
            <strong>Proyectos recientes</strong>
            {recentProjects.slice(0, 4).map((recent) => (
              <button key={recent.id} onClick={() => onOpenRecent(recent)}>
                <span className="recent-icon">
                  <Film />
                </span>
                <span>
                  <strong>{recent.name}</strong>
                  <small>{recent.root}</small>
                </span>
                <ChevronRight />
              </button>
            ))}
          </div>
        )}
      </div>
      <div className="memory-frame" aria-hidden="true">
        <div className="frame-photo">
          <div className="sun" />
          <div className="mountains" />
          <div className="road" />
        </div>
        <div className="frame-caption">
          <span>El viaje merece una historia</span>
          <Film size={22} />
        </div>
      </div>
      <div className={`setup-status ${ready ? "ready" : "warning"}`}>
        <span>{ready ? <Check size={17} /> : <CircleAlert size={17} />}</span>
        <div>
          <strong>
            {ready
              ? "Herramientas esenciales listas"
              : "Faltan herramientas esenciales"}
          </strong>
          <small>
            {deps.length ? (
              <>
                <span>
                  Esenciales:{" "}
                  {deps
                    .filter((d) =>
                      ["FFmpeg", "ffprobe", "ExifTool", "ImageMagick"].includes(d.name),
                    )
                    .map(
                      (d) =>
                        `${d.name}: ${d.available ? "listo" : "no encontrado"}`,
                    )
                    .join(" · ")}
                </span>
                <span>
                  IA opcional:{" "}
                  {deps
                    .filter((d) => ["Ollama", "whisper.cpp"].includes(d.name))
                    .map(
                      (d) =>
                        `${d.name}: ${d.available ? "listo" : "no instalado"}`,
                    )
                    .join(" · ")}
                </span>
              </>
            ) : (
              "Comprobando esta Mac..."
            )}
          </small>
          <button className="dependency-refresh" onClick={onRefresh}>
            <RotateCcw /> Revisar de nuevo
          </button>
        </div>
      </div>
    </section>
  );
}

function ImportView({
  media,
  totals,
  busy,
  onImport,
  onClear,
  onBack,
  onNext,
}: any) {
  const [visible, setVisible] = useState(250);
  const [confirmClear, setConfirmClear] = useState(false);
  const [showDetails, setShowDetails] = useState(false);
  return (
    <section className="workspace page-enter">
      <div className="page-heading">
        <div>
          <span className="step">Tu material</span>
          <h2>{media.length ? "Tus recuerdos están listos" : "Importa tus recuerdos"}</h2>
          <p>
            {media.length
              ? "Casi Cine se encargará de ordenarlos, entender el viaje y elegir los mejores momentos."
              : "Arrastra una carpeta completa. Casi Cine encontrará archivos dentro de todas sus subcarpetas."}
          </p>
        </div>
        <div className="page-actions">
          <button className="secondary" onClick={onBack}>
            Volver
          </button>
          <button className="primary" onClick={onImport} disabled={busy}>
            {busy ? (
              <LoaderCircle className="spin" />
            ) : (
              <FolderOpen size={18} />
            )}
            {media.length ? "Añadir más" : "Importar"}
          </button>
        </div>
      </div>
      {media.length === 0 ? (
        <button className="dropzone" onClick={onImport}>
          <div className="drop-icon">
            <Image size={28} />
          </div>
          <strong>Suelta aquí tus fotos y videos</strong>
          <span>MOV, MP4, JPG, JPEG, PNG y HEIC</span>
        </button>
      ) : (
        <>
          <div className="summary-grid">
            <Metric
              icon={<Image />}
              value={media.length}
              label={`${totals.photos} fotos · ${totals.videos} videos`}
            />
            <Metric
              icon={<Clock3 />}
              value={formatDuration(totals.duration)}
              label="de video"
            />
            <Metric
              icon={<HardDrive />}
              value={formatBytes(totals.size)}
              label="en disco"
            />
            <Metric
              icon={<RotateCcw />}
              value={totals.duplicates}
              label="copias exactas"
            />
          </div>
          {totals.unprobedVideos > 0 && (
            <div className="metadata-warning">
              <CircleAlert />
              <span>
                <strong>
                  No se pudo leer la duración de {totals.unprobedVideos} videos.
                </strong>
                Vuelve a importar la misma carpeta para reparar sus metadatos;
                no se crearán copias adicionales.
              </span>
            </div>
          )}
          <div className="import-ready-action">
            <div>
              <strong>No necesitas revisar archivo por archivo</strong>
              <span>
                Las tomas parecidas, fechas dudosas y videos problemáticos se
                resolverán automáticamente al crear la película.
              </span>
            </div>
            <button className="text-action" onClick={() => setShowDetails((value) => !value)}>
              {showDetails ? "Ocultar archivos" : "Ver archivos importados"}
              <ChevronRight className={showDetails ? "disclosure-open" : ""} />
            </button>
          </div>
          {showDetails && (
            <>
              <div className="import-explainer">
                <span>
                  <strong>Copias exactas:</strong> detectadas durante la importación.
                </span>
                <span>
                  <strong>Tomas parecidas:</strong> se agruparán durante el análisis visual.
                </span>
              </div>
              <div className="media-list">
                <div className="list-head">
                  <span>Archivos importados</span>
                  <span>
                    {totals.uncertain
                      ? `${totals.uncertain} fechas estimadas automáticamente`
                      : "Fechas verificadas"}
                  </span>
                </div>
                {media.slice(0, visible).map((m: MediaItem) => (
              <div
                className={`media-row ${m.duplicate_of ? "duplicate" : ""}`}
                key={m.id}
              >
                <span className="media-icon">
                  {m.kind === "video" ? <Film /> : <Image />}
                </span>
                <span className="media-name">
                  <strong>{m.filename}</strong>
                  <small>
                    {m.captured_at
                      ? new Date(
                          m.manual_timestamp || m.captured_at,
                        ).toLocaleString("es-MX")
                      : "Fecha desconocida"}{" "}
                    · {m.timestamp_source}
                  </small>
                </span>
                <span>
                  {m.duration_seconds
                    ? formatDuration(m.duration_seconds)
                    : m.kind === "video"
                      ? "Video · duración pendiente"
                      : "Foto"}
                </span>
                <span>{formatBytes(m.size_bytes)}</span>
                {m.timestamp_uncertain && <span className="uncertain">Estimada</span>}
              </div>
            ))}
            {visible < media.length && (
              <button
                className="load-more"
                onClick={() => setVisible((value) => value + 250)}
              >
                Mostrar 250 más · quedan {media.length - visible}
              </button>
            )}
              </div>
            </>
          )}
          <div className="clear-project">
            {!confirmClear ? (
              <button onClick={() => setConfirmClear(true)}>
                <Trash2 /> Descartar todo lo importado
              </button>
            ) : (
              <div className="clear-confirmation">
                <div>
                  <strong>¿Vaciar este proyecto?</strong>
                  <small>
                    Se borrarán referencias, análisis, caché y renders del
                    proyecto. Tus archivos originales nunca se tocarán.
                  </small>
                </div>
                <button
                  className="secondary compact"
                  onClick={() => setConfirmClear(false)}
                >
                  Cancelar
                </button>
                <button
                  className="danger compact"
                  onClick={async () => {
                    await onClear();
                    setConfirmClear(false);
                  }}
                  disabled={busy}
                >
                  Sí, vaciar proyecto
                </button>
              </div>
            )}
          </div>
          <div className="bottom-bar">
            <button className="secondary" onClick={onBack}>
              Volver a proyectos
            </button>
            <button className="primary" onClick={onNext}>
              Elegir formato
              <ChevronRight size={18} />
            </button>
          </div>
        </>
      )}
    </section>
  );
}

function Metric({ icon, value, label }: any) {
  return (
    <div className="metric">
      <span>{icon}</span>
      <strong>{value}</strong>
      <small>{label}</small>
    </div>
  );
}

function FormatView({ selected, setSelected, busy, onBack, onConfirm }: any) {
  const formats = [
    {
      id: "16:9",
      title: "Horizontal",
      description: "Ideal para YouTube, televisores y computadoras.",
      resolution: "1920 × 1080",
    },
    {
      id: "9:16",
      title: "Vertical",
      description: "Ideal para YouTube Shorts, Instagram Reels y TikTok.",
      resolution: "1080 × 1920",
    },
    {
      id: "1:1",
      title: "Cuadrado",
      description: "Ideal para publicaciones en redes sociales.",
      resolution: "1080 × 1080",
    },
  ];
  return (
    <section className="workspace format-page page-enter">
      <div className="page-heading format-heading">
        <div>
          <span className="step">Formato de tu película</span>
          <h2>Elige dónde quieres verla</h2>
          <p>
            Casi Cine adaptará la selección, el encuadre y la exportación sin
            deformar tus recuerdos.
          </p>
        </div>
      </div>
      <div className="format-grid">
        {formats.map((format) => (
          <button
            key={format.id}
            className={
              selected === format.id ? "format-card selected" : "format-card"
            }
            onClick={() => setSelected(format.id)}
            aria-pressed={selected === format.id}
          >
            <span
              className={`ratio-preview ratio-${format.id.replace(":", "")}`}
            >
              <span>{format.id}</span>
            </span>
            <span className="format-copy">
              <strong>
                {format.id} <span>{format.title}</span>
              </strong>
              <small>{format.description}</small>
              <em>{format.resolution}</em>
            </span>
            {selected === format.id && (
              <span className="format-check">
                <Check size={16} />
              </span>
            )}
          </button>
        ))}
      </div>
      <div className="format-note">
        <Image />
        <span>
          <strong>Primero, conservar el recuerdo.</strong>
          <small>
            Si llenar el cuadro implica cortar contenido importante, usaremos un
            fondo suave y mantendremos la imagen completa.
          </small>
        </span>
      </div>
      <div className="bottom-bar">
        <button className="secondary" onClick={onBack}>
          Volver
        </button>
        <button className="primary large" onClick={onConfirm} disabled={busy}>
          {busy ? <LoaderCircle className="spin" /> : <Check />}Confirmar
          formato
        </button>
      </div>
    </section>
  );
}

function CreateView({
  minutes,
  setMinutes,
  recommendedMinutes,
  musicStatus,
  musicMode,
  setMusicMode,
  musicMood,
  setMusicMood,
  localMusicPath,
  onPickLocalMusic,
  onInstallMusic,
  locationTitles,
  setLocationTitles,
  onBack,
  onCreate,
  busy,
}: any) {
  const choices = [5, 10, 15, 20, 30];
  const moods = [
    ["automatic", "Recomendada"],
    ["nostalgic", "Nostálgica"],
    ["cinematic", "Cinemática"],
    ["adventurous", "Aventura"],
    ["peaceful", "Tranquila"],
    ["playful", "Juguetona"],
  ];
  return (
    <section className="workspace narrow page-enter">
      <div className="page-heading">
        <div>
          <span className="step">La película</span>
          <h2>¿Cómo quieres recordarlo?</h2>
          <p>
            Puedes cambiar estas decisiones y regenerar la edición sin analizar
            tus archivos otra vez.
          </p>
        </div>
      </div>
      <div className="settings-section">
        <h3>Duración recomendada</h3>
        <button
          className="duration-recommendation"
          onClick={() => setMinutes(recommendedMinutes)}
        >
          <span>
            <Sparkles /> Casi Cine recomienda
          </span>
          <strong>{recommendedMinutes} min</strong>
          <small>
            Calculado según la cantidad de video, fotos y etapas disponibles.
          </small>
        </button>
        <label className="duration-custom">
          <span>
            <strong>Tu duración: {minutes} minutos</strong>
            <small>Puedes elegir cualquier valor entre 1 y 45 minutos.</small>
          </span>
          <input
            type="range"
            min="1"
            max="45"
            step="0.5"
            value={minutes}
            onChange={(event) => setMinutes(Number(event.target.value))}
          />
        </label>
        <div className="choice-grid">
          {choices.map((n) => (
            <button
              key={n}
              className={minutes === n ? "choice active" : "choice"}
              onClick={() => setMinutes(n)}
            >
              <strong>{n}</strong>
              <span>minutos</span>
            </button>
          ))}
        </div>
      </div>
      <div className="settings-section">
        <h3>Estilo</h3>
        <div className="style-list">
          <label className="style-choice selected">
            <input type="radio" defaultChecked name="style" />
            <Sparkles />
            <span>
              <strong>Recuerdos familiares</strong>
              <small>Conversaciones, reacciones y momentos compartidos</small>
            </span>
          </label>
          <label className="style-choice">
            <input type="radio" name="style" />
            <Film />
            <span>
              <strong>Documental cronológico</strong>
              <small>Un relato fiel al orden de cada día</small>
            </span>
          </label>
        </div>
      </div>
      <div className="settings-section">
        <h3>Banda sonora</h3>
        <div className="style-list">
          <label
            className={
              musicMode === "local" ? "style-choice selected" : "style-choice"
            }
          >
            <input
              type="radio"
              name="music"
              checked={musicMode === "local"}
              onChange={onPickLocalMusic}
            />
            <FolderOpen />
            <span>
              <strong>Una canción de mi Mac</strong>
              <small>
                {localMusicPath
                  ? localMusicPath.split("/").pop()
                  : "MP3, M4A, WAV, AAC o FLAC. Se repetirá suavemente si hace falta."}
              </small>
            </span>
          </label>
          <label
            className={
              musicMode === "none" ? "style-choice selected" : "style-choice"
            }
          >
            <input
              type="radio"
              name="music"
              checked={musicMode === "none"}
              onChange={() => setMusicMode("none")}
            />
            <Volume2 />
            <span>
              <strong>Sin música</strong>
              <small>Solo conversaciones y sonido ambiente original</small>
            </span>
          </label>
          <label
            className={
              musicMode === "generated"
                ? "style-choice selected"
                : "style-choice"
            }
          >
            <input
              type="radio"
              name="music"
              checked={musicMode === "generated"}
              disabled={!musicStatus?.installed}
              onChange={() => setMusicMode("generated")}
            />
            <Sparkles />
            <span>
              <strong>Música original local</strong>
              <small>
                {musicStatus?.installed
                  ? `${musicStatus.engine}. Crea un tema principal y variaciones para paisajes y movimiento, todo en esta Mac.`
                  : "ACE-Step es opcional. La instalación explícita ocupa aproximadamente 10 GB."}
              </small>
            </span>
          </label>
        </div>
        {!musicStatus?.installed && (
          <div className="optional-install">
            <div>
              <strong>Instalación opcional</strong>
              <small>
                ACE-Step 1.5, licencia MIT. Requiere Python 3.12, unos 10 GB de
                disco y conexión solo durante la instalación.
              </small>
            </div>
            <button className="secondary" onClick={onInstallMusic}>
              Instalar música local
            </button>
          </div>
        )}
        {musicMode === "generated" && (
          <div className="mood-picker">
            {moods.map(([value, label]) => (
              <button
                key={value}
                className={musicMood === value ? "active" : ""}
                onClick={() => setMusicMood(value)}
              >
                {label}
              </button>
            ))}
            <p>
              Instrumental por defecto. No existe garantía frente a reclamos de
              derechos o Content ID.
            </p>
          </div>
        )}
      </div>
      <div className="settings-section">
        <h3>Ubicaciones en la película</h3>
        <div className="style-list">
          <label className={locationTitles ? "style-choice selected" : "style-choice"}>
            <input
              type="checkbox"
              checked={locationTitles}
              onChange={(event) => setLocationTitles(event.target.checked)}
            />
            <MapPin />
            <span>
              <strong>Títulos de lugares</strong>
              <small>Muestra cada parada importante la primera vez que aparece.</small>
            </span>
          </label>
        </div>
      </div>
      <div className="bottom-bar">
        <button className="secondary" onClick={onBack}>
          Volver
        </button>
        <button className="primary large" onClick={onCreate} disabled={busy}>
          {busy ? <LoaderCircle className="spin" /> : <Sparkles />}Crear y
          guardar mi película
        </button>
      </div>
    </section>
  );
}

function PreviewView({
  items,
  setItems,
  culling,
  musicTracks,
  musicMode,
  localMusicPath,
  onPickLocalMusic,
  aspectRatio,
  exportFormats,
  setExportFormats,
  onRegenerateMusic,
  onOverride,
  onChangeFormat,
  onBack,
  onRender,
  previewPath,
  onGeneratePreview,
  journey,
}: any) {
  const [tab, setTab] = useState<"timeline" | "selected" | "rejected">(
    "timeline",
  );
  const [showAdvanced, setShowAdvanced] = useState(false);
  const duration = items
    .filter((i: TimelineItem) => !i.excluded)
    .reduce((n: number, i: TimelineItem) => n + i.duration_seconds, 0);
  const toggle = (id: string) =>
    setItems((old: TimelineItem[]) =>
      old.map((i) => (i.id === id ? { ...i, excluded: !i.excluded } : i)),
    );
  const updateFraming = (id: string, patch: Partial<TimelineItem>) =>
    setItems((old: TimelineItem[]) =>
      old.map((item) => (item.id === id ? { ...item, ...patch } : item)),
    );
  const toggleExportFormat = (format: string) =>
    setExportFormats((current: string[]) =>
      current.includes(format)
        ? current.filter((value) => value !== format)
        : [...current, format],
    );
  const selected = (culling?.items || []).filter(
    (i: CullingItem) => i.user_override ?? i.selected,
  );
  const rejected = (culling?.items || []).filter(
    (i: CullingItem) => !(i.user_override ?? i.selected),
  );
  return (
    <section className="workspace page-enter">
      <div className="page-heading">
        <div>
          <span className="step">Selección automática</span>
          <h2>Una historia sin repeticiones</h2>
          <p>
            Cada decisión se basa en señales medidas localmente. Puedes
            restaurar cualquier recuerdo.
          </p>
        </div>
        <div className="preview-meta">
          <button className="format-badge" onClick={onChangeFormat}>
            <span
              className={`mini-ratio ratio-${aspectRatio.replace(":", "")}`}
            />
            {aspectRatio}
            <small>Cambiar</small>
          </button>
          <div className="duration-badge">
            <Clock3 /> {formatDuration(duration)}
          </div>
        </div>
      </div>
      {musicMode === "generated" && musicTracks[0] && (
        <div className="soundtrack-strip">
          <span>
            <Volume2 />
            <span>
              <strong>Banda sonora original</strong>
              <small>
                {musicTracks[0].mood}, semilla {musicTracks[0].seed}
                {musicTracks[0].cached ? ", desde caché" : ""}
              </small>
            </span>
          </span>
          <button className="secondary compact" onClick={onRegenerateMusic}>
            <RotateCcw /> Regenerar música
          </button>
        </div>
      )}
      {musicMode === "local" && localMusicPath && (
        <div className="soundtrack-strip">
          <span>
            <Volume2 />
            <span>
              <strong>Tu canción</strong>
              <small>{localMusicPath.split("/").pop()}</small>
            </span>
          </span>
          <button className="secondary compact" onClick={onPickLocalMusic}>
            <FolderOpen /> Cambiar canción
          </button>
        </div>
      )}
      {journey.length > 0 && (
        <section className="journey-strip" aria-label="Historia detectada">
          <header>
            <span>
              <Sparkles /> Historia detectada
            </span>
            <small>
              Basada en observaciones visuales locales; revisa antes de
              exportar.
            </small>
          </header>
          <div>
            {journey.map((chapter: JourneyChapter, index: number) => (
              <span
                className="journey-step"
                key={`${chapter.scene_type}-${index}`}
                title={chapter.description}
              >
                <strong>{chapter.label}</strong>
                <small>
                  {chapter.count} {chapter.count === 1 ? "momento" : "momentos"}
                </small>
                {index < journey.length - 1 && <ChevronRight />}
              </span>
            ))}
          </div>
        </section>
      )}
      {journey.length === 0 && (
        <div className="semantic-warning">
          <CircleAlert />
          <span>
            <strong>No se pudo construir una historia visual</strong>
            <small>
              Esta propuesta usa únicamente cronología y calidad técnica.
              Verifica que Ollama esté activo y que qwen3.5:9b esté instalado
              antes de regenerarla.
            </small>
          </span>
        </div>
      )}
      <div className="review-tabs">
        <button
          className={tab === "timeline" ? "active" : ""}
          onClick={() => setTab("timeline")}
        >
          Mi película <span>{items.length}</span>
        </button>
        <button
          className={tab === "selected" ? "active" : ""}
          onClick={() => setTab("selected")}
        >
          Elegidos <span>{selected.length}</span>
        </button>
        <button
          className={tab === "rejected" ? "active" : ""}
          onClick={() => setTab("rejected")}
        >
          Descartados <span>{rejected.length}</span>
        </button>
      </div>
      {tab === "timeline" ? (
        <div className="preview-layout">
          <div
            className="player"
            style={{ aspectRatio: aspectRatio.replace(":", " / ") }}
          >
            {previewPath ? (
              <video
                className="preview-video"
                src={convertFileSrc(previewPath)}
                controls
                playsInline
              />
            ) : (
              <div className="player-empty">
                <Film size={44} />
                <span>Genera una vista previa ligera antes de exportar</span>
                <button
                  className="preview-generate"
                  onClick={onGeneratePreview}
                >
                  <Play fill="currentColor" /> Vista previa
                </button>
              </div>
            )}
          </div>
          <aside className="review-guide">
            <span className="review-guide-icon">
              <Check />
            </span>
            <div>
              <strong>Ya está editada automáticamente</strong>
              <p>
                Reproduce la vista previa. Si te gusta, solo elige los formatos
                de exportación. No necesitas mover clips ni aprender una línea
                de tiempo.
              </p>
              <dl>
                <div>
                  <dt>Momentos</dt>
                  <dd>
                    {items.filter((i: TimelineItem) => !i.excluded).length}
                  </dd>
                </div>
                <div>
                  <dt>Duración</dt>
                  <dd>{formatDuration(duration)}</dd>
                </div>
              </dl>
              <button
                className="secondary compact"
                onClick={() => setShowAdvanced((value) => !value)}
              >
                {showAdvanced
                  ? "Ocultar ajustes"
                  : "Ajustar momentos y encuadre"}
              </button>
            </div>
          </aside>
          {showAdvanced && (
            <div className="timeline-list advanced-timeline">
              {items.map((item: TimelineItem, index: number) => (
                <div
                  key={item.id}
                  className={`timeline-row ${item.excluded ? "excluded" : ""}`}
                >
                  <span className="clip-number">{index + 1}</span>
                  <span className="clip-icon">
                    {item.kind === "video" ? <Film /> : <Image />}
                  </span>
                  <span className="clip-info">
                    <strong>{item.source_path.split("/").pop()}</strong>
                    <small>
                      {formatDuration(item.start_seconds)} -{" "}
                      {formatDuration(item.end_seconds)} ·{" "}
                      {formatDuration(item.duration_seconds)}
                    </small>
                  </span>
                  <button
                    className="icon-button"
                    onClick={() => toggle(item.id)}
                    aria-label={item.excluded ? "Restaurar" : "Excluir"}
                  >
                    {item.excluded ? <RotateCcw /> : <Trash2 />}
                  </button>
                  <div className="framing-control">
                    {item.kind === "video" && (
                      <button
                        onClick={() => {
                          const policies: TimelineItem["audio_policy"][] = [
                            "dialogue",
                            "ambient",
                            "mute",
                          ];
                          const current = policies.indexOf(item.audio_policy);
                          updateFraming(item.id, {
                            audio_policy:
                              policies[(current + 1) % policies.length],
                          });
                        }}
                      >
                        Audio:{" "}
                        {item.audio_policy === "dialogue"
                          ? "conversación"
                          : item.audio_policy === "ambient"
                            ? "ambiente suave"
                            : item.audio_policy === "mute"
                              ? "silenciado"
                              : "original"}
                      </button>
                    )}
                    <button
                      onClick={() =>
                        updateFraming(item.id, {
                          framing_mode:
                            item.framing_mode === "fit" ? "fill" : "fit",
                        })
                      }
                    >
                      {item.framing_mode === "fit"
                        ? "Llenar encuadre"
                        : "Conservar completa"}
                    </button>
                    {item.framing_mode === "fill" && (
                      <>
                        <label>
                          Horizontal
                          <input
                            type="range"
                            min="0"
                            max="1"
                            step="0.05"
                            value={item.focal_x}
                            onChange={(e) =>
                              updateFraming(item.id, {
                                focal_x: Number(e.target.value),
                              })
                            }
                          />
                        </label>
                        <label>
                          Vertical
                          <input
                            type="range"
                            min="0"
                            max="1"
                            step="0.05"
                            value={item.focal_y}
                            onChange={(e) =>
                              updateFraming(item.id, {
                                focal_y: Number(e.target.value),
                              })
                            }
                          />
                        </label>
                      </>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      ) : (
        <CullingGroups
          items={tab === "selected" ? selected : rejected}
          rejected={tab === "rejected"}
          onOverride={onOverride}
        />
      )}
      <section className="export-formats" aria-labelledby="export-title">
        <div>
          <span className="step">Versiones de entrega</span>
          <h3 id="export-title">¿En qué formatos quieres exportar?</h3>
          <p>
            Puedes crear varias versiones en una sola exportación. El análisis
            se reutiliza y cada formato recalcula selección y encuadre.
          </p>
        </div>
        <div className="export-format-options">
          {[
            ["16:9", "Horizontal", "1920 × 1080"],
            ["9:16", "Vertical", "1080 × 1920"],
            ["1:1", "Cuadrado", "1080 × 1080"],
          ].map(([format, label, resolution]) => {
            const active = exportFormats.includes(format);
            return (
              <button
                key={format}
                className={active ? "export-format active" : "export-format"}
                onClick={() => toggleExportFormat(format)}
                aria-pressed={active}
              >
                <span
                  className={`mini-ratio ratio-${format.replace(":", "")}`}
                />
                <span>
                  <strong>{format}</strong> {label}
                  <small>{resolution}</small>
                </span>
                <span className="export-check">{active && <Check />}</span>
              </button>
            );
          })}
        </div>
      </section>
      <div className="bottom-bar">
        <button className="secondary" onClick={onBack}>
          Ajustes
        </button>
        <button
          className="primary large"
          onClick={onRender}
          disabled={!exportFormats.length}
        >
          Exportar {exportFormats.length || ""}{" "}
          {exportFormats.length === 1 ? "versión" : "versiones"}
          <ChevronRight />
        </button>
      </div>
    </section>
  );
}

function CullingGroups({
  items,
  rejected,
  onOverride,
}: {
  items: CullingItem[];
  rejected: boolean;
  onOverride: (id: number, value: boolean) => void;
}) {
  const groups = Object.values(
    items.reduce((acc: Record<string, CullingItem[]>, item) => {
      (acc[item.group_id] ??= []).push(item);
      return acc;
    }, {}),
  );
  if (!items.length)
    return (
      <div className="review-empty">
        <Check />
        <strong>No hay elementos en esta sección</strong>
      </div>
    );
  return (
    <div className="culling-groups">
      {groups.map((group) => (
        <section className="culling-group" key={group[0].group_id}>
          <div className="group-heading">
            <div>
              <strong>{group[0].group_kind}</strong>
              <span>
                {group.length} {group.length === 1 ? "archivo" : "archivos"}
              </span>
            </div>
            <small>
              {group[0].captured_at
                ? new Date(group[0].captured_at).toLocaleString("es-MX")
                : "Fecha incierta"}
            </small>
          </div>
          {group.map((item) => (
            <div className="decision-row" key={item.media_id}>
              <span className="decision-icon">
                {item.kind === "video" ? <Film /> : <Image />}
              </span>
              <span className="decision-copy">
                <strong>{item.filename}</strong>
                <small>{item.reason}</small>
                <span className="signal-line">
                  <span>Calidad {Math.round(item.score)}</span>
                  {item.sharpness != null && (
                    <span>Nitidez {Math.round(item.sharpness)}</span>
                  )}
                  {item.has_audio && (
                    <span>
                      <Volume2 size={13} />
                      Audio original
                    </span>
                  )}
                </span>
              </span>
              <button
                className={rejected ? "secondary compact" : "icon-button"}
                onClick={() => onOverride(item.media_id, rejected)}
              >
                {rejected ? (
                  <>
                    <RotateCcw />
                    Restaurar
                  </>
                ) : (
                  <Trash2 />
                )}
              </button>
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

function ProgressView({
  progress,
  exported,
  purpose,
  media,
  suggestedTitle,
  onCancel,
  onReveal,
  onBack,
}: any) {
  const [showcaseIndex, setShowcaseIndex] = useState(0);
  const [etaSeconds, setEtaSeconds] = useState<number | null>(null);
  const etaSamples = useRef<{
    stage: string;
    values: { time: number; fraction: number }[];
  }>({ stage: "", values: [] });
  const pct = Math.round(progress.fraction * 100);
  const cancelled = progress.stage === "cancelled";
  const cancelling = progress.stage === "cancelling";
  const photos = (media as MediaItem[]).filter((item) => item.kind === "photo");
  useEffect(() => {
    if (exported || cancelled || !photos.length) return;
    const timer = window.setInterval(
      () => setShowcaseIndex((value) => (value + 1) % photos.length),
      3200,
    );
    return () => window.clearInterval(timer);
  }, [exported, cancelled, photos.length]);
  const thoughts =
    purpose === "import"
      ? ["Leyendo fechas", "Encontrando cámaras", "Ordenando recuerdos"]
      : [
          "Buscando esas miradas que valen oro",
          "Separando ruido de conversaciones bonitas",
          "Dándole aire a los paisajes",
          "Conectando casa, camino y destino",
          "Probando dónde debe entrar la música",
          "Guardando un buen final para la historia",
        ];
  const thought = thoughts[Math.floor(pct / 7) % thoughts.length];
  useEffect(() => {
    if (exported || cancelled || progress.fraction <= 0) {
      setEtaSeconds(null);
      return;
    }
    const now = Date.now();
    if (etaSamples.current.stage !== progress.stage) {
      etaSamples.current = {
        stage: progress.stage,
        values: [{ time: now, fraction: progress.fraction }],
      };
      setEtaSeconds(null);
      return;
    }
    const values = etaSamples.current.values;
    values.push({ time: now, fraction: progress.fraction });
    etaSamples.current.values = values.filter(
      (sample) => now - sample.time <= 90_000,
    );
    const first = etaSamples.current.values[0];
    const elapsed = (now - first.time) / 1000;
    const completed = progress.fraction - first.fraction;
    if (elapsed >= 30 && completed >= 0.002) {
      const estimate = ((1 - progress.fraction) * elapsed) / completed;
      setEtaSeconds(Math.round(Math.min(estimate, 8 * 60 * 60)));
    }
  }, [progress.fraction, progress.stage, exported, cancelled]);
  const etaCopy = etaSeconds
    ? `${etaSeconds >= 3600 ? `${Math.floor(etaSeconds / 3600)} h ` : ""}${Math.max(1, Math.round((etaSeconds % 3600) / 60))} min restantes · aprox. ${new Date(Date.now() + etaSeconds * 1000).toLocaleTimeString("es-MX", { hour: "numeric", minute: "2-digit" })}`
    : "Calculando el tiempo con el ritmo real de esta Mac…";
  return (
    <section className="progress-view page-enter">
      <div className="progress-showcase">
        {photos.length ? (
          <img
            key={photos[showcaseIndex % photos.length].path}
            src={convertFileSrc(photos[showcaseIndex % photos.length].path)}
            alt="Recuerdo del viaje que Casi Cine está organizando"
          />
        ) : (
          <Film size={52} />
        )}
        <span>{exported ? "Lista para verla" : thought}</span>
      </div>
      <div className="progress-copy">
        <div className="progress-orbit">
          {exported ? (
            <Check size={34} />
          ) : cancelled ? (
            <X size={34} />
          ) : (
            <Film size={30} />
          )}
        </div>
        <span className="step">
          {exported
            ? "Película terminada"
            : cancelled
              ? "Proceso cancelado"
              : purpose === "import"
                ? "Importando tus recuerdos"
                : "Creando tu película"}
        </span>
        <h2>
          {exported
            ? suggestedTitle || "Ya puedes volver a vivirla."
            : progress.message || "Preparando la historia"}
        </h2>
        {!cancelled && (
          <>
            <div className="progress-track">
              <span style={{ width: `${pct}%` }} />
            </div>
            <div className="progress-meta">
              <span>{pct}%</span>
              <span>{progress.stage}</span>
            </div>
            {!exported && purpose !== "import" && (
              <p className="progress-eta">
                <Clock3 /> {etaCopy}
              </p>
            )}
          </>
        )}
        <div className="actions">
          {exported ? (
            <>
              <button className="secondary" onClick={onBack}>
                Volver a editar
              </button>
              <button className="primary" onClick={onReveal}>
                Mostrar en Finder
              </button>
            </>
          ) : cancelled ? (
            <button className="primary" onClick={onBack}>
              Volver
            </button>
          ) : (
            <button
              className="secondary"
              onClick={onCancel}
              disabled={cancelling}
            >
              {cancelling ? <LoaderCircle className="spin" /> : <X />}
              {cancelling ? "Cancelando…" : "Cancelar"}
            </button>
          )}
        </div>
      </div>
    </section>
  );
}
