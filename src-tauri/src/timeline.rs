use crate::{
    db,
    error::Result,
    models::{CullingItem, TimelineItem},
};
use chrono::DateTime;
use std::{collections::HashMap, path::Path};
use uuid::Uuid;

fn timestamp(item: &CullingItem) -> Option<i64> {
    item.captured_at
        .as_deref()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.naive_local().and_utc().timestamp())
}
fn visual_hash(item: &CullingItem) -> Option<u64> {
    item.visual_hash
        .as_deref()
        .and_then(|v| u64::from_str_radix(v, 16).ok())
}
fn chapters(items: &[CullingItem], target: f64) -> Vec<Vec<usize>> {
    if items.is_empty() {
        return vec![];
    }
    let desired = ((target / 50.0).round() as usize)
        .clamp(4, 18)
        .min(items.len());
    let times = items.iter().filter_map(timestamp).collect::<Vec<_>>();
    if times.len() >= items.len() * 2 / 3 {
        let min = *times.iter().min().unwrap();
        let max = *times.iter().max().unwrap();
        if max - min >= 3600 {
            let mut bins = vec![Vec::new(); desired];
            for (index, item) in items.iter().enumerate() {
                let bin = timestamp(item)
                    .map(|value| {
                        (((value - min) as f64 / (max - min + 1) as f64) * desired as f64).floor()
                            as usize
                    })
                    .unwrap_or(index * desired / items.len())
                    .min(desired - 1);
                bins[bin].push(index);
            }
            return bins.into_iter().filter(|bin| !bin.is_empty()).collect();
        }
    }
    (0..desired)
        .filter_map(|chapter| {
            let start = chapter * items.len() / desired;
            let end = (chapter + 1) * items.len() / desired;
            (start < end).then(|| (start..end).collect())
        })
        .collect()
}

fn semantic_chapters(
    items: &[CullingItem],
    semantic: &HashMap<i64, crate::semantic::Observation>,
    target: f64,
) -> Vec<Vec<usize>> {
    let important_transition =
        |scene: &str| matches!(scene, "home" | "airport" | "flight" | "road" | "return");
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current_scene = "unknown";
    for (index, item) in items.iter().enumerate() {
        let scene = semantic
            .get(&item.media_id)
            .map(|value| value.scene_type.as_str())
            .unwrap_or("unknown");
        let previous_scene = items.get(index.saturating_sub(1)).and_then(|previous| {
            semantic
                .get(&previous.media_id)
                .map(|value| value.scene_type.as_str())
        });
        let next_scene = items.get(index + 1).and_then(|next| {
            semantic
                .get(&next.media_id)
                .map(|value| value.scene_type.as_str())
        });
        // One isolated classification between two matching scenes is usually
        // model noise, not an actual new chapter. Travel transitions are kept
        // even when represented by a single clip.
        let stable_scene = if previous_scene == next_scene && !important_transition(scene) {
            previous_scene.unwrap_or(scene)
        } else {
            scene
        };
        if groups.is_empty() || stable_scene != current_scene {
            groups.push(vec![index]);
            current_scene = stable_scene;
        } else if let Some(group) = groups.last_mut() {
            group.push(index);
        }
    }
    // Every act needs enough screen time to develop. A short requested movie
    // therefore uses fewer, broader chapters instead of dozens of one-second
    // fragments. Merging keeps chronology and all source candidates intact.
    let max_groups = ((target / 10.0).floor() as usize).clamp(4, 18);
    while groups.len() > max_groups {
        let merge_at = groups
            .iter()
            .enumerate()
            .min_by_key(|(_, group)| group.len())
            .map(|(index, _)| index)
            .unwrap_or(0);
        if merge_at == 0 {
            let next = groups.remove(1);
            groups[0].extend(next);
        } else {
            let current = groups.remove(merge_at);
            groups[merge_at - 1].extend(current);
        }
    }
    groups
}

fn chapter_weight(
    items: &[CullingItem],
    indices: &[usize],
    semantic: &HashMap<i64, crate::semantic::Observation>,
) -> f64 {
    let mut interaction: f64 = 0.0;
    let mut memorable: f64 = 0.0;
    let mut nature: f64 = 0.0;
    let mut journey: f64 = 0.0;
    let mut dialogue = false;
    for index in indices {
        if let Some(value) = semantic.get(&items[*index].media_id) {
            interaction = interaction.max(value.interaction_score);
            memorable = memorable.max(value.memorable_score);
            nature = nature.max(value.nature_score);
            journey = journey.max(value.journey_score);
            dialogue |= !value.transcript.trim().is_empty();
        }
    }
    1.0 + interaction / 100.0 * 1.25
        + memorable / 100.0 * 0.8
        + nature / 100.0 * 0.45
        + journey / 100.0 * 0.55
        + if dialogue { 0.65 } else { 0.0 }
}
fn near_duplicate(item: &CullingItem, hashes: &[u64]) -> bool {
    item.kind == "photo"
        && visual_hash(item).is_some_and(|hash| {
            hashes
                .iter()
                .any(|selected| (hash ^ selected).count_ones() <= 5)
        })
}
fn choose_item(
    items: &[CullingItem],
    index: usize,
    limit: f64,
    remaining: &mut f64,
    selected: &mut HashMap<i64, f64>,
    hashes: &mut Vec<u64>,
) -> f64 {
    if *remaining <= 0.05 || selected.contains_key(&items[index].media_id) {
        return 0.0;
    }
    let item = &items[index];
    if item.user_override != Some(true) && near_duplicate(item, hashes) {
        return 0.0;
    }
    let duration = item.duration_seconds.min(*remaining).min(limit).max(0.0);
    let minimum = if item.kind == "photo" { 2.0 } else { 3.0 };
    if duration < minimum {
        return 0.0;
    }
    selected.insert(item.media_id, duration);
    *remaining -= duration;
    if let Some(hash) = visual_hash(item) {
        hashes.push(hash);
    }
    duration
}

fn extend_selected_item(
    items: &[CullingItem],
    index: usize,
    desired: f64,
    remaining: &mut f64,
    selected: &mut HashMap<i64, f64>,
) -> f64 {
    let item = &items[index];
    let Some(current) = selected.get_mut(&item.media_id) else {
        return 0.0;
    };
    let target = item
        .duration_seconds
        .min(desired)
        .min(*current + *remaining);
    let added = (target - *current).max(0.0);
    *current += added;
    *remaining -= added;
    added
}
#[cfg(test)]
fn select_coverage(items: &[CullingItem], target: f64) -> HashMap<i64, f64> {
    select_coverage_with_semantics(items, target, &HashMap::new())
}

fn select_coverage_with_semantics(
    items: &[CullingItem],
    target: f64,
    semantic: &HashMap<i64, crate::semantic::Observation>,
) -> HashMap<i64, f64> {
    let mut groups = chapters(items, target);
    // Preserve visible journey transitions as chapter boundaries. Consecutive
    // files with the same semantic scene remain together; a change such as
    // home -> airport -> flight -> pool creates distinct coverage obligations.
    if !semantic.is_empty() {
        let semantic_groups = semantic_chapters(items, semantic, target);
        if semantic_groups.len() >= 3 {
            groups = semantic_groups;
        }
    }
    if groups.is_empty() {
        return HashMap::new();
    }
    let weights = groups
        .iter()
        .map(|group| chapter_weight(items, group, semantic))
        .collect::<Vec<_>>();
    let weight_total = weights.iter().sum::<f64>().max(0.01);
    let minimum = (target / groups.len() as f64).min(5.0);
    let flexible = (target - minimum * groups.len() as f64).max(0.0);
    let mut selected = HashMap::new();
    let mut hashes = Vec::new();
    for (chapter_index, indices) in groups.iter().enumerate() {
        let mut remaining = minimum + flexible * weights[chapter_index] / weight_total;
        if chapter_index == 0 {
            choose_item(
                items,
                indices[0],
                if items[indices[0]].kind == "photo" {
                    2.5
                } else {
                    4.0
                },
                &mut remaining,
                &mut selected,
                &mut hashes,
            );
        }
        if chapter_index + 1 == groups.len() {
            // A closing beat needs a setup and a final image. Reserve a moment
            // from the latter part of the last chapter before the literal last
            // clip, then let chronological output create a small closing arc.
            let setup = indices[indices.len().saturating_sub(1) * 2 / 3];
            choose_item(
                items,
                setup,
                if items[setup].kind == "photo" {
                    2.5
                } else {
                    4.0
                },
                &mut remaining,
                &mut selected,
                &mut hashes,
            );
            let index = *indices.last().unwrap();
            choose_item(
                items,
                index,
                if items[index].kind == "photo" {
                    2.5
                } else {
                    4.0
                },
                &mut remaining,
                &mut selected,
                &mut hashes,
            );
        }
        for index in indices
            .iter()
            .copied()
            .filter(|i| items[*i].user_override == Some(true))
        {
            choose_item(
                items,
                index,
                items[index].duration_seconds,
                &mut remaining,
                &mut selected,
                &mut hashes,
            );
        }
        if !semantic.is_empty() {
            // Open each act with visual context: movement/arrival when present,
            // otherwise the strongest piece of scenery. Chronological output
            // order is preserved later; this only reserves its place.
            if let Some(establishing) = indices.iter().copied().max_by(|a, b| {
                let value = |index: usize| {
                    semantic
                        .get(&items[index].media_id)
                        .map(|observation| {
                            observation.journey_score * 0.6 + observation.nature_score * 0.4
                        })
                        .unwrap_or(0.0)
                };
                value(*a).total_cmp(&value(*b))
            }) {
                choose_item(
                    items,
                    establishing,
                    if items[establishing].kind == "photo" {
                        2.5
                    } else {
                        4.0
                    },
                    &mut remaining,
                    &mut selected,
                    &mut hashes,
                );
            }

            // Every chapter should contain its strongest human memory when one
            // exists—not just its prettiest establishing shot. Conversations,
            // play, affection and reactions get enough time to develop even if
            // that means using fewer generic scenic clips.
            if let Some(family_anchor) = indices
                .iter()
                .copied()
                .filter(|index| {
                    semantic.get(&items[*index].media_id).is_some_and(|value| {
                        value.interaction_score >= 30.0
                            || (value.people_count >= 2 && value.memorable_score >= 45.0)
                            || (!value.transcript.trim().is_empty()
                                && value.interaction_score >= 20.0)
                            || value
                                .tags
                                .iter()
                                .any(|tag| tag == "action_payoff" || tag == "meaningful_reaction")
                    })
                })
                .max_by(|a, b| {
                    let value = |index: usize| {
                        semantic
                            .get(&items[index].media_id)
                            .map(|observation| {
                                observation.interaction_score * 0.55
                                    + observation.memorable_score * 0.30
                                    + observation.people_count.min(4) as f64 * 4.0
                                    + if observation.transcript.trim().is_empty() {
                                        0.0
                                    } else {
                                        30.0
                                    }
                            })
                            .unwrap_or(0.0)
                    };
                    value(*a).total_cmp(&value(*b))
                })
            {
                let observation = semantic.get(&items[family_anchor].media_id).unwrap();
                let desired = if items[family_anchor].kind == "photo" {
                    3.0
                } else if !observation.transcript.trim().is_empty() {
                    8.0
                } else {
                    6.0
                };
                choose_item(
                    items,
                    family_anchor,
                    desired,
                    &mut remaining,
                    &mut selected,
                    &mut hashes,
                );
                extend_selected_item(items, family_anchor, desired, &mut remaining, &mut selected);
            }
        }
        let mut videos = indices
            .iter()
            .copied()
            .filter(|i| items[*i].kind == "video")
            .collect::<Vec<_>>();
        let mut photos = indices
            .iter()
            .copied()
            .filter(|i| items[*i].kind == "photo")
            .collect::<Vec<_>>();
        let story_score = |index: usize| {
            let item = &items[index];
            let semantic_bonus = semantic
                .get(&item.media_id)
                .map(|value| {
                    value.interaction_score * 0.35
                        + value.memorable_score * 0.25
                        + value.nature_score * 0.16
                        + value.journey_score * 0.24
                        + if value.transcript.trim().is_empty() {
                            0.0
                        } else {
                            30.0
                        }
                })
                .unwrap_or(0.0);
            item.score + semantic_bonus
                - semantic
                    .get(&item.media_id)
                    .map(crate::semantic::obstruction_penalty)
                    .unwrap_or(0.0)
                - semantic
                    .get(&item.media_id)
                    .map(crate::semantic::irrelevance_penalty)
                    .unwrap_or(0.0)
        };
        videos.sort_by(|a, b| story_score(*b).total_cmp(&story_score(*a)));
        photos.sort_by(|a, b| story_score(*b).total_cmp(&story_score(*a)));
        let video_target = remaining * if videos.is_empty() { 0.0 } else { 0.78 };
        let mut video_used = 0.0;
        for index in videos.iter().copied() {
            if video_used >= video_target {
                break;
            }
            video_used += choose_item(
                items,
                index,
                (video_target - video_used).max(0.0),
                &mut remaining,
                &mut selected,
                &mut hashes,
            );
        }
        for index in photos.iter().copied() {
            if remaining <= 0.05 {
                break;
            }
            choose_item(
                items,
                index,
                3.0,
                &mut remaining,
                &mut selected,
                &mut hashes,
            );
        }
        for index in videos.into_iter().chain(photos.into_iter()) {
            if remaining <= 0.05 {
                break;
            }
            choose_item(
                items,
                index,
                items[index].duration_seconds,
                &mut remaining,
                &mut selected,
                &mut hashes,
            );
        }
    }
    // Avoid showing the same ordinary scene repeatedly across neighboring
    // groups. Repeated family interaction and real conversation are protected:
    // emotional continuity matters more than visual novelty in those cases.
    if !semantic.is_empty() {
        let meaningful = |index: usize| {
            semantic.get(&items[index].media_id).is_some_and(|value| {
                value.interaction_score >= 55.0
                    || (value.people_count >= 2 && value.interaction_score >= 30.0)
                    || (!value.transcript.trim().is_empty() && value.interaction_score >= 20.0)
                    || value.memorable_score >= 78.0
            })
        };
        let mut kept: Vec<usize> = Vec::new();
        for index in 0..items.len() {
            if items[index].kind != "video" || !selected.contains_key(&items[index].media_id) {
                continue;
            }
            let repeated = kept.iter().position(|previous| {
                let close = match (timestamp(&items[index]), timestamp(&items[*previous])) {
                    (Some(a), Some(b)) => (a - b).abs() <= 600,
                    _ => index.abs_diff(*previous) <= 5,
                };
                close
                    && semantic
                        .get(&items[index].media_id)
                        .zip(semantic.get(&items[*previous].media_id))
                        .is_some_and(|(a, b)| crate::semantic::semantically_similar(a, b))
            });
            if let Some(position) = repeated {
                let previous = kept[position];
                if meaningful(index) || meaningful(previous) {
                    kept.push(index);
                } else if items[index].score > items[previous].score {
                    selected.remove(&items[previous].media_id);
                    kept[position] = index;
                } else {
                    selected.remove(&items[index].media_id);
                }
            } else {
                kept.push(index);
            }
        }
    }
    selected
}

fn audio_policy(item: &CullingItem, observation: Option<&crate::semantic::Observation>) -> String {
    if item.kind == "photo" || !item.has_audio {
        return "mute".into();
    }
    let Some(value) = observation else {
        return "original".into();
    };
    if crate::semantic::irrelevance_penalty(value) >= 80.0 {
        return "mute".into();
    }
    let meaningful_dialogue = !value.transcript.trim().is_empty()
        && (value.interaction_score >= 20.0
            || value.memorable_score >= 55.0
            || (value.people_count >= 2 && value.interaction_score >= 15.0));
    if meaningful_dialogue {
        return "dialogue".into();
    }
    if value.transcript.trim().is_empty()
        && matches!(
            value.scene_type.as_str(),
            "nature" | "beach" | "pool" | "flight" | "road"
        )
        && (value.nature_score >= 45.0 || value.journey_score >= 55.0)
    {
        return "ambient".into();
    }
    "mute".into()
}

fn focused_start(item: &CullingItem, duration: f64, selected: bool) -> f64 {
    if item.kind != "video" || !selected || duration + 0.05 >= item.duration_seconds {
        return item.start_seconds;
    }
    let focus = item.start_seconds + item.duration_seconds / 2.0;
    (focus - duration * 0.45).clamp(
        item.start_seconds,
        (item.end_seconds - duration).max(item.start_seconds),
    )
}

pub fn build(root: &Path, target: f64) -> Result<Vec<TimelineItem>> {
    let output_aspect = db::aspect_ratio(root);
    if let Ok(report) = crate::culling::load(root) {
        let mut candidates = report
            .items
            .into_iter()
            .filter(|i| i.user_override.unwrap_or(i.selected))
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            candidates.sort_by(|a, b| {
                timestamp(a)
                    .cmp(&timestamp(b))
                    .then(a.media_id.cmp(&b.media_id))
            });
            let semantic = crate::semantic::load(root);
            let selected = select_coverage_with_semantics(&candidates, target, &semantic);
            return Ok(candidates
                .into_iter()
                .map(|item| {
                    let chosen = selected.get(&item.media_id).copied();
                    let duration = chosen.unwrap_or(item.duration_seconds);
                    let policy = audio_policy(&item, semantic.get(&item.media_id));
                    // Preserve the moment that made a video worth selecting.
                    // When the movie budget shortens its analyzed window, keep
                    // the visual payoff near the middle with enough post-roll
                    // for actions such as jumps, splashes and reactions to end.
                    let start_seconds = focused_start(&item, duration, chosen.is_some());
                    TimelineItem {
                        id: Uuid::new_v4().to_string(),
                        media_id: item.media_id,
                        source_path: item.source_path,
                        kind: item.kind,
                        start_seconds,
                        end_seconds: start_seconds + duration,
                        duration_seconds: duration,
                        captured_at: item.captured_at,
                        locked: item.user_override == Some(true),
                        excluded: chosen.is_none(),
                        output_aspect: output_aspect.clone(),
                        audio_policy: policy,
                        framing_mode: "fit".into(),
                        focal_x: 0.5,
                        focal_y: 0.5,
                    }
                })
                .collect());
        }
    }
    let media = db::list_media(root)?
        .into_iter()
        .filter(|m| m.duplicate_of.is_none())
        .collect::<Vec<_>>();
    if media.is_empty() {
        return Ok(vec![]);
    }
    let per_item = target / media.len() as f64;
    Ok(media
        .into_iter()
        .map(|item| {
            let duration = if item.kind == "photo" {
                per_item.min(3.0)
            } else {
                item.duration_seconds.unwrap_or(0.0).min(per_item.max(3.0))
            };
            let policy = if item.kind == "photo" {
                "mute".into()
            } else {
                "original".into()
            };
            TimelineItem {
                id: Uuid::new_v4().to_string(),
                media_id: item.id,
                source_path: item.path,
                kind: item.kind,
                start_seconds: 0.0,
                end_seconds: duration,
                duration_seconds: duration,
                captured_at: item.manual_timestamp.or(item.captured_at),
                locked: false,
                excluded: duration <= 0.05,
                output_aspect: output_aspect.clone(),
                audio_policy: policy,
                framing_mode: "fit".into(),
                focal_x: 0.5,
                focal_y: 0.5,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use rusqlite::params;
    #[test]
    fn covers_beginning_middle_and_end_within_budget() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::connect(dir.path()).unwrap();
        for index in 0..20 {
            let ts = format!("2024-01-{:02}T10:00:00Z", index + 1);
            conn.execute("INSERT INTO media(path,filename,kind,size_bytes,fingerprint,duration_seconds,captured_at,timestamp_source,timestamp_uncertain)VALUES(?1,?2,'video',1,?3,20,?4,'test',0)", params![format!("/tmp/{index}.mp4"),format!("{index}.mp4"),format!("f{index}"),ts]).unwrap();
            let id = conn.last_insert_rowid();
            conn.execute("INSERT INTO culling(media_id,selected,group_id,group_kind,score,reason,start_seconds,end_seconds,duration_seconds,has_audio,analyzed_at)VALUES(?1,1,?2,'Video único',50,'test',0,8,8,0,'now')",params![id,format!("g{index}")]).unwrap();
        }
        drop(conn);
        let timeline = build(dir.path(), 40.0).unwrap();
        let included = timeline.iter().filter(|i| !i.excluded).collect::<Vec<_>>();
        assert!(included.iter().map(|i| i.duration_seconds).sum::<f64>() <= 40.001);
        assert!(included.iter().any(|i| i.media_id <= 3));
        assert!(included.iter().any(|i| i.media_id >= 18));
        assert!(included.iter().any(|i| (8..=13).contains(&i.media_id)));
    }
    #[test]
    fn repeated_visual_hashes_are_limited() {
        let items = (0..8)
            .map(|index| CullingItem {
                media_id: index,
                source_path: format!("/{index}.jpg"),
                filename: format!("{index}.jpg"),
                kind: "photo".into(),
                selected: true,
                user_override: None,
                group_id: format!("g{index}"),
                group_kind: "Foto".into(),
                score: 50.0,
                reason: "test".into(),
                start_seconds: 0.0,
                end_seconds: 3.0,
                duration_seconds: 3.0,
                captured_at: Some(format!("2024-01-{:02}T10:00:00Z", index + 1)),
                sharpness: None,
                exposure: None,
                visual_hash: Some(format!("{:016x}", 0xff00_u64 + index as u64 % 2)),
                has_audio: false,
            })
            .collect::<Vec<_>>();
        assert!(select_coverage(&items, 24.0).len() < items.len());
    }

    #[test]
    fn semantic_story_keeps_each_journey_act() {
        let scenes = ["home", "home", "airport", "airport", "nature", "nature"];
        let items = scenes
            .iter()
            .enumerate()
            .map(|(index, _)| CullingItem {
                media_id: index as i64 + 1,
                source_path: format!("/{index}.mp4"),
                filename: format!("{index}.mp4"),
                kind: "video".into(),
                selected: true,
                user_override: None,
                group_id: format!("g{index}"),
                group_kind: "Video único".into(),
                score: 50.0,
                reason: "test".into(),
                start_seconds: 0.0,
                end_seconds: 8.0,
                duration_seconds: 8.0,
                captured_at: Some(format!("2024-01-01T10:{index:02}:00Z")),
                sharpness: None,
                exposure: None,
                visual_hash: None,
                has_audio: true,
            })
            .collect::<Vec<_>>();
        let semantic = scenes
            .iter()
            .enumerate()
            .map(|(index, scene)| {
                (
                    index as i64 + 1,
                    crate::semantic::Observation {
                        scene_type: (*scene).into(),
                        interaction_score: if index == 1 { 95.0 } else { 20.0 },
                        nature_score: if *scene == "nature" { 90.0 } else { 10.0 },
                        journey_score: if *scene == "airport" { 90.0 } else { 10.0 },
                        memorable_score: 60.0,
                        transcript: if index == 1 {
                            "Mira, ven conmigo".into()
                        } else {
                            String::new()
                        },
                        ..Default::default()
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let selected = select_coverage_with_semantics(&items, 36.0, &semantic);
        for scene in ["home", "airport", "nature"] {
            assert!(items.iter().any(|item| {
                selected.contains_key(&item.media_id)
                    && semantic[&item.media_id].scene_type == scene
            }));
        }
        assert!(selected.get(&2).is_some_and(|duration| *duration >= 6.0));
    }

    #[test]
    fn audio_policy_keeps_meaning_and_mutes_chatter() {
        let item = CullingItem {
            media_id: 1,
            source_path: "/clip.mp4".into(),
            filename: "clip.mp4".into(),
            kind: "video".into(),
            selected: true,
            user_override: None,
            group_id: "g".into(),
            group_kind: "Video único".into(),
            score: 50.0,
            reason: "test".into(),
            start_seconds: 0.0,
            end_seconds: 8.0,
            duration_seconds: 8.0,
            captured_at: None,
            sharpness: None,
            exposure: None,
            visual_hash: None,
            has_audio: true,
        };
        let meaningful = crate::semantic::Observation {
            transcript: "Mira qué bonito, ven conmigo".into(),
            interaction_score: 85.0,
            ..Default::default()
        };
        let chatter = crate::semantic::Observation {
            transcript: "texto incidental".into(),
            interaction_score: 5.0,
            memorable_score: 10.0,
            ..Default::default()
        };
        let nature = crate::semantic::Observation {
            scene_type: "nature".into(),
            nature_score: 90.0,
            ..Default::default()
        };
        let incidental_service = crate::semantic::Observation {
            transcript: "Ay, disculpe".into(),
            interaction_score: 70.0,
            tags: vec!["incidental_service".into()],
            ..Default::default()
        };
        assert_eq!(audio_policy(&item, Some(&meaningful)), "dialogue");
        assert_eq!(audio_policy(&item, Some(&chatter)), "mute");
        assert_eq!(audio_policy(&item, Some(&nature)), "ambient");
        assert_eq!(audio_policy(&item, Some(&incidental_service)), "mute");
    }

    #[test]
    fn shortened_video_keeps_the_payoff_and_post_roll() {
        let item = CullingItem {
            media_id: 1,
            source_path: "/jump.mp4".into(),
            filename: "jump.mp4".into(),
            kind: "video".into(),
            selected: true,
            user_override: None,
            group_id: "jump".into(),
            group_kind: "Video único".into(),
            score: 80.0,
            reason: "acción".into(),
            start_seconds: 0.0,
            end_seconds: 15.0,
            duration_seconds: 15.0,
            captured_at: None,
            sharpness: None,
            exposure: None,
            visual_hash: None,
            has_audio: true,
        };
        let start = focused_start(&item, 5.0, true);
        assert!((start - 5.25).abs() < 0.001);
        assert!(start + 5.0 > item.duration_seconds / 2.0);
    }
}
