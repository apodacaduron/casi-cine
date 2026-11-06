import { invoke } from "@tauri-apps/api/core";
import type {
  CullingReport,
  CleanupResult,
  Dependency,
  MediaItem,
  MusicEngineStatus,
  MusicRequest,
  MusicTrack,
  Project,
  TimelineItem,
  JourneyChapter,
} from "./types";

export const api = {
  dependencies: () => invoke<Dependency[]>("check_dependencies"),
  createProject: (name: string, root: string) =>
    invoke<Project>("create_project", { name, root }),
  createDefaultProject: () => invoke<Project>("create_default_project"),
  openProject: (root: string) => invoke<Project>("open_project", { root }),
  openProjectDocument: (documentPath: string) =>
    invoke<Project>("open_project_document", { documentPath }),
  recentProjects: () => invoke<Project[]>("recent_projects"),
  setProjectFormat: (projectRoot: string, aspectRatio: string) =>
    invoke<Project>("set_project_format", { projectRoot, aspectRatio }),
  importMedia: (projectRoot: string, paths: string[]) =>
    invoke<MediaItem[]>("import_media", { projectRoot, paths }),
  listMedia: (projectRoot: string) =>
    invoke<MediaItem[]>("list_media", { projectRoot }),
  clearProjectMedia: (projectRoot: string) =>
    invoke<void>("clear_project_media", { projectRoot }),
  cleanupProject: (projectRoot: string) =>
    invoke<CleanupResult>("cleanup_project", { projectRoot }),
  setTimestamp: (
    projectRoot: string,
    mediaId: number,
    timestamp: string | null,
  ) =>
    invoke<void>("set_manual_timestamp", { projectRoot, mediaId, timestamp }),
  buildTimeline: (projectRoot: string, targetSeconds: number) =>
    invoke<TimelineItem[]>("build_timeline", { projectRoot, targetSeconds }),
  analyzeCulling: (projectRoot: string) =>
    invoke<CullingReport>("analyze_culling", { projectRoot }),
  loadCulling: (projectRoot: string) =>
    invoke<CullingReport>("load_culling", { projectRoot }),
  semanticJourney: (projectRoot: string) =>
    invoke<JourneyChapter[]>("semantic_journey", { projectRoot }),
  suggestProjectTitle: (projectRoot: string) =>
    invoke<string>("suggest_project_title", { projectRoot }),
  overrideCulling: (projectRoot: string, mediaId: number, selected: boolean) =>
    invoke<CullingReport>("override_culling", {
      projectRoot,
      mediaId,
      selected,
    }),
  musicStatus: () => invoke<MusicEngineStatus>("music_engine_status"),
  installMusic: () => invoke<MusicEngineStatus>("install_music_engine"),
  generateMusic: (projectRoot: string, request: MusicRequest) =>
    invoke<MusicTrack[]>("generate_music", { projectRoot, request }),
  exportMovie: (
    projectRoot: string,
    outputPath: string,
    items: TimelineItem[],
    width: number,
    height: number,
    musicPaths: string[],
    locationTitles: boolean,
  ) =>
    invoke<string>("export_movie", {
      projectRoot,
      outputPath,
      items,
      width,
      height,
      musicPaths,
      locationTitles,
    }),
  exportPreview: (
    projectRoot: string,
    items: TimelineItem[],
    width: number,
    height: number,
    musicPaths: string[],
    locationTitles: boolean,
  ) =>
    invoke<string>("export_preview", {
      projectRoot,
      items,
      width,
      height,
      musicPaths,
      locationTitles,
    }),
  cancel: () => invoke<void>("cancel_job"),
};

export const formatBytes = (value: number) => {
  if (!value) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(
    Math.floor(Math.log(value) / Math.log(1024)),
    units.length - 1,
  );
  return `${(value / 1024 ** i).toFixed(i ? 1 : 0)} ${units[i]}`;
};

export const formatDuration = (seconds: number) => {
  const s = Math.max(0, Math.round(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};
