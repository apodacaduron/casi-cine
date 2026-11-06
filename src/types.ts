export type Project = {
  id: string;
  name: string;
  root: string;
  created_at: string;
  updated_at: string;
  aspect_ratio: string;
  format_confirmed: boolean;
};
export type MediaKind = "video" | "photo";
export type MediaItem = {
  id: number;
  path: string;
  filename: string;
  kind: MediaKind;
  size_bytes: number;
  duration_seconds: number | null;
  width: number | null;
  height: number | null;
  captured_at: string | null;
  timestamp_source: string;
  timestamp_uncertain: boolean;
  duplicate_of: number | null;
  manual_timestamp: string | null;
};
export type TimelineItem = {
  id: string;
  media_id: number;
  source_path: string;
  kind: MediaKind;
  start_seconds: number;
  end_seconds: number;
  duration_seconds: number;
  captured_at: string | null;
  locked: boolean;
  excluded: boolean;
  output_aspect: string;
  audio_policy: "dialogue" | "ambient" | "mute" | "original";
  framing_mode: "fit" | "fill";
  focal_x: number;
  focal_y: number;
};
export type Dependency = {
  name: string;
  available: boolean;
  path: string | null;
  detail: string;
};
export type CleanupResult = {
  bytes_freed: number;
  files_removed: number;
  repaired_references: number;
};
export type ExportProgress = {
  stage: string;
  fraction: number;
  message: string;
};
export type CullingItem = {
  media_id: number;
  source_path: string;
  filename: string;
  kind: MediaKind;
  selected: boolean;
  user_override: boolean | null;
  group_id: string;
  group_kind: string;
  score: number;
  reason: string;
  start_seconds: number;
  end_seconds: number;
  duration_seconds: number;
  captured_at: string | null;
  sharpness: number | null;
  exposure: number | null;
  visual_hash: string | null;
  has_audio: boolean;
};
export type CullingReport = {
  items: CullingItem[];
  selected_count: number;
  rejected_count: number;
  groups: number;
};
export type MusicEngineStatus = {
  installed: boolean;
  root: string | null;
  model_bytes: number;
  engine: string;
  license: string;
  memory_note: string;
  download_note: string;
};
export type MusicRequest = {
  mood: string;
  duration_seconds: number;
  candidates: number;
  seed: number | null;
};
export type JourneyChapter = {
  scene_type: string;
  label: string;
  description: string;
  count: number;
};
export type MusicTrack = {
  path: string;
  prompt: string;
  mood: string;
  duration_seconds: number;
  seed: number;
  cached: boolean;
  engine: string;
  disclaimer: string;
};
