use crate::{db, error::Result};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tauri::{AppHandle, Manager};

#[derive(Clone)]
struct Place {
    name: String,
    country: String,
    lat: f64,
    lon: f64,
    population: i64,
}

fn ensure_schema(root: &Path) -> Result<()> {
    db::connect(root)?.execute_batch(
        "CREATE TABLE IF NOT EXISTS media_location(
      media_id INTEGER PRIMARY KEY,latitude REAL,longitude REAL,label TEXT,source TEXT NOT NULL,
      FOREIGN KEY(media_id) REFERENCES media(id) ON DELETE CASCADE);",
    )?;
    Ok(())
}

fn gazetteer_path(app: &AppHandle) -> Option<PathBuf> {
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/geonames/cities500.txt");
    if dev.exists() {
        return Some(dev);
    }
    app.path()
        .resource_dir()
        .ok()
        .map(|dir| dir.join("resources/geonames/cities500.txt"))
        .filter(|path| path.exists())
}

fn load_grid(path: &Path) -> HashMap<(i16, i16), Vec<Place>> {
    let mut grid: HashMap<(i16, i16), Vec<Place>> = HashMap::new();
    let Ok(content) = fs::read_to_string(path) else {
        return grid;
    };
    for line in content.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() < 15 {
            continue;
        }
        let (Ok(lat), Ok(lon)) = (fields[4].parse::<f64>(), fields[5].parse::<f64>()) else {
            continue;
        };
        let population = fields[14].parse::<i64>().unwrap_or(0);
        grid.entry((lat.floor() as i16, lon.floor() as i16))
            .or_default()
            .push(Place {
                name: fields[1].to_string(),
                country: fields[8].to_string(),
                lat,
                lon,
                population,
            });
    }
    grid
}

fn distance_km(a_lat: f64, a_lon: f64, b_lat: f64, b_lon: f64) -> f64 {
    let (a_lat, b_lat) = (a_lat.to_radians(), b_lat.to_radians());
    let dlat = b_lat - a_lat;
    let dlon = (b_lon - a_lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + a_lat.cos() * b_lat.cos() * (dlon / 2.0).sin().powi(2);
    6371.0 * 2.0 * h.sqrt().asin()
}

fn nearest(grid: &HashMap<(i16, i16), Vec<Place>>, lat: f64, lon: f64) -> Option<String> {
    let key = (lat.floor() as i16, lon.floor() as i16);
    let mut best: Option<(&Place, f64)> = None;
    for dy in -2..=2 {
        for dx in -2..=2 {
            if let Some(places) = grid.get(&(key.0 + dy, key.1 + dx)) {
                for place in places {
                    let distance = distance_km(lat, lon, place.lat, place.lon);
                    // Prefer the recognizable center of a metropolitan area
                    // over a tiny adjacent suburb, while an actually nearby
                    // destination (Chemuyil, Bacalar, etc.) still wins over a
                    // larger city many kilometres away.
                    let score = distance - (place.population.max(1) as f64).ln() * 2.5;
                    if distance <= 100.0
                        && best.as_ref().is_none_or(|(current, current_score)| {
                            score < *current_score
                                || ((score - *current_score).abs() < 0.25
                                    && place.population > current.population)
                        })
                    {
                        best = Some((place, score));
                    }
                }
            }
        }
    }
    best.map(|(place, _)| {
        if place.country.is_empty() {
            place.name.clone()
        } else {
            format!("{}, {}", place.name, place.country)
        }
    })
}

pub fn analyze(app: &AppHandle, root: &Path) -> Result<()> {
    ensure_schema(root)?;
    let conn = db::connect(root)?;
    let existing: i64 = conn
        .query_row("SELECT COUNT(*) FROM media_location", [], |row| row.get(0))
        .unwrap_or(0);
    let media = db::list_media(root)?;
    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key='location_analyzer_version'",
            [],
            |row| row.get(0),
        )
        .ok();
    if existing >= media.len() as i64 && version.as_deref() == Some("2") {
        return Ok(());
    }
    let mut rows = Vec::new();
    for chunk in media.chunks(150) {
        let mut command = Command::new("exiftool");
        command.args([
            "-j",
            "-n",
            "-GPSLatitude",
            "-GPSLongitude",
            "-City",
            "-State",
            "-Country",
            "-Location",
            "-GPSAreaInformation",
        ]);
        command.args(chunk.iter().map(|item| item.path.as_str()));
        let Ok(output) = command.output() else {
            continue;
        };
        if let Ok(values) = serde_json::from_slice::<Vec<Value>>(&output.stdout) {
            rows.extend(values);
        }
    }
    let needs_grid = rows
        .iter()
        .any(|row| row.get("GPSLatitude").and_then(Value::as_f64).is_some());
    let grid = if needs_grid {
        gazetteer_path(app)
            .map(|path| load_grid(&path))
            .unwrap_or_default()
    } else {
        HashMap::new()
    };
    let ids = media
        .into_iter()
        .map(|item| (item.path, item.id))
        .collect::<HashMap<_, _>>();
    for row in rows {
        let Some(path) = row.get("SourceFile").and_then(Value::as_str) else {
            continue;
        };
        let Some(id) = ids.get(path) else { continue };
        let lat = row.get("GPSLatitude").and_then(Value::as_f64);
        let lon = row.get("GPSLongitude").and_then(Value::as_f64);
        let embedded = ["City", "Location", "GPSAreaInformation", "State", "Country"]
            .iter()
            .find_map(|key| {
                row.get(*key)
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
            })
            .map(str::to_string);
        let label = embedded
            .clone()
            .or_else(|| lat.zip(lon).and_then(|(a, b)| nearest(&grid, a, b)));
        conn.execute("INSERT OR REPLACE INTO media_location(media_id,latitude,longitude,label,source)VALUES(?1,?2,?3,?4,?5)",rusqlite::params![id,lat,lon,label,if embedded.is_some(){"embedded label"}else if lat.is_some(){"offline GeoNames"}else{"none"}])?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO settings(key,value) VALUES('location_analyzer_version','2')",
        [],
    )?;
    Ok(())
}

pub fn suggest_title(root: &Path) -> String {
    let Ok(conn) = db::connect(root) else {
        return "Nuestro viaje".into();
    };
    let Ok(mut stmt)=conn.prepare("SELECT ml.label FROM media_location ml JOIN media m ON m.id=ml.media_id WHERE ml.label IS NOT NULL ORDER BY COALESCE(m.manual_timestamp,m.captured_at),m.id") else{return "Nuestro viaje".into()};
    let labels = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map(|rows| {
            rows.filter_map(|row| row.ok())
                .fold(Vec::new(), |mut out, value| {
                    if out.last() != Some(&value) {
                        out.push(value);
                    }
                    out
                })
        })
        .unwrap_or_default();
    let clean = |value: &str| value.split(',').next().unwrap_or(value).to_string();
    let mut places = Vec::new();
    for label in labels {
        let place = clean(&label);
        if !places.contains(&place) {
            places.push(place);
        }
    }
    match places.as_slice() {
        [] => "Nuestro viaje".into(),
        [one] => format!("Recuerdos de {one}"),
        [first, second] => format!("{first} y {second}"),
        many => format!("{} y {}", many[many.len() - 2], many[many.len() - 1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distance_is_symmetric() {
        let a = distance_km(21.16, -86.85, 18.68, -88.39);
        let b = distance_km(18.68, -88.39, 21.16, -86.85);
        assert!((a - b).abs() < 0.001);
        assert!(a > 100.0);
    }
}
