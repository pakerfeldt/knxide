//! Floor plans: images the user supplies, with the devices the user placed on them.
//!
//! A plan is kept in the state file (name, image file, places) and its image as a file in the
//! directory beside it; see [`crate::sidecar`]. Nothing of it belongs to the project. Places
//! are percentages of the image's width and height, so they hold at any size.
//!
//! Only PNG, JPEG and WebP images are taken, told by their first bytes and not by what the
//! request calls them, and only up to [`MAX_IMAGE`] bytes. SVG is refused: it can carry
//! script, and the image is served back to the page.

use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::body::Bytes;
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Programmer, Refusal, body_json, guard, json_response, ok};
use crate::sidecar::{self, FloorPlan, Position};
use crate::workspace::Open;

/// The largest image taken, in bytes.
pub(crate) const MAX_IMAGE: usize = 8 * 1024 * 1024;
/// How many plans a project may have.
const MAX_PLANS: usize = 32;

/// The kind of image `bytes` is, as file extension and media type, if it is one that is taken.
fn kind(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(("png", "image/png"))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("webp", "image/webp"))
    } else {
        None
    }
}

fn check(bytes: &[u8]) -> Result<(&'static str, &'static str), Refusal> {
    if bytes.len() > MAX_IMAGE {
        return Err(Refusal::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("the image is larger than {} MB", MAX_IMAGE / (1024 * 1024)),
        ));
    }
    kind(bytes).ok_or_else(|| {
        Refusal::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "the image has to be a PNG, JPEG or WebP file",
        )
    })
}

fn view(plan: &FloorPlan) -> Value {
    let positions: BTreeMap<&String, Value> = plan
        .positions
        .iter()
        .map(|(device, at)| {
            (
                device,
                json!({ "x": f64::from(at.x) / 100.0, "y": f64::from(at.y) / 100.0 }),
            )
        })
        .collect();
    json!({
        "id": plan.id,
        "name": plan.name,
        // The file name changes with the image, so a replaced image is fetched anew.
        "image": format!("/api/floor-plans/{}/image?v={}", plan.id, plan.image),
        "positions": positions,
    })
}

fn name_of(text: &str, fallback: usize) -> Result<String, Refusal> {
    let name = text.trim();
    if name.chars().count() > 60 {
        return Err(Refusal::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the name is longer than 60 characters",
        ));
    }
    Ok(if name.is_empty() {
        format!("Plan {fallback}")
    } else {
        name.to_string()
    })
}

impl Programmer {
    /// The directory of the images, when there is a state file to keep them beside.
    fn plans_directory(&self) -> Option<PathBuf> {
        self.inner
            .settings
            .state
            .as_deref()
            .map(sidecar::plans_directory)
    }

    /// Keep `bytes` as the image file `file`.
    fn store_image(&self, file: &str, bytes: &[u8]) -> Result<(), Refusal> {
        let failed = |error: std::io::Error| {
            Refusal::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the image could not be kept: {error}"),
            )
        };
        match self.plans_directory() {
            Some(directory) => {
                std::fs::create_dir_all(&directory).map_err(failed)?;
                // Whole or not at all, like the state file.
                let temporary = directory.join(format!("{file}.tmp"));
                std::fs::write(&temporary, bytes).map_err(failed)?;
                std::fs::rename(&temporary, directory.join(file)).map_err(failed)
            }
            None => {
                self.shared()
                    .plan_images
                    .insert(file.to_string(), bytes.to_vec());
                Ok(())
            }
        }
    }

    fn drop_image(&self, file: &str) {
        match self.plans_directory() {
            Some(directory) => {
                let _ = std::fs::remove_file(directory.join(file));
            }
            None => {
                self.shared().plan_images.remove(file);
            }
        }
    }

    fn load_image(&self, file: &str) -> Option<Vec<u8>> {
        match self.plans_directory() {
            Some(directory) => std::fs::read(directory.join(file)).ok(),
            None => self.shared().plan_images.get(file).cloned(),
        }
    }

    /// Change the plans and write the state file.
    fn with_plans<T>(
        &self,
        change: impl FnOnce(&mut Vec<FloorPlan>) -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        let mut shared = self.shared();
        let result = change(&mut shared.state.plans)?;
        match self.save(&shared) {
            Some(error) => Err(Refusal::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the plan could not be saved: {error}"),
            )),
            None => Ok(result),
        }
    }

    fn create_plan(&self, name: &str, bytes: &[u8]) -> Result<Value, Refusal> {
        let (extension, _) = check(bytes)?;
        let (id, count) = {
            let shared = self.shared();
            let plans = &shared.state.plans;
            if plans.len() >= MAX_PLANS {
                return Err(Refusal::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("there are {MAX_PLANS} plans already"),
                ));
            }
            let free = (1..)
                .map(|number| format!("plan-{number}"))
                .find(|id| plans.iter().all(|plan| plan.id != *id))
                .unwrap_or_default();
            (free, plans.len())
        };
        let plan = FloorPlan {
            name: name_of(name, count + 1)?,
            image: format!("{id}-1.{extension}"),
            id,
            positions: BTreeMap::new(),
        };
        self.store_image(&plan.image, bytes)?;
        let made = plan.clone();
        self.with_plans(move |plans| {
            plans.push(plan);
            Ok(())
        })?;
        Ok(view(&made))
    }

    fn change_plan(&self, id: &str, body: &Value) -> Result<Value, Refusal> {
        let bad = |message: &str| Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, message);
        let name = match &body["name"] {
            Value::Null => None,
            Value::String(name) => Some(name_of(name, 1)?),
            _ => return Err(bad("the name has to be text")),
        };
        let positions = match &body["positions"] {
            Value::Null => None,
            Value::Object(places) => {
                let mut positions = BTreeMap::new();
                for (device, place) in places {
                    if self.inner.project.device(device).is_none() {
                        return Err(bad("a placed device is not in the project"));
                    }
                    let part = |key: &str| {
                        place[key]
                            .as_f64()
                            .filter(|value| (0.0..=100.0).contains(value))
                            .map(|value| (value * 100.0).round() as u16)
                            .ok_or_else(|| bad("a place is two numbers from 0 to 100, x and y"))
                    };
                    positions.insert(
                        device.clone(),
                        Position {
                            x: part("x")?,
                            y: part("y")?,
                        },
                    );
                }
                Some(positions)
            }
            _ => return Err(bad("the places have to be given by device")),
        };
        self.with_plans(|plans| {
            let plan = plans
                .iter_mut()
                .find(|plan| plan.id == id)
                .ok_or_else(Refusal::missing)?;
            if let Some(name) = name {
                plan.name = name;
            }
            if let Some(positions) = positions {
                plan.positions = positions;
            }
            Ok(view(plan))
        })
    }

    fn replace_plan_image(&self, id: &str, bytes: &[u8]) -> Result<Value, Refusal> {
        let (extension, _) = check(bytes)?;
        let old = self
            .shared()
            .state
            .plans
            .iter()
            .find(|plan| plan.id == id)
            .map(|plan| plan.image.clone())
            .ok_or_else(Refusal::missing)?;
        // A new file name each time: `plan-1-2.png` after `plan-1-1.png`.
        let turn = old
            .rsplit_once('.')
            .and_then(|(stem, _)| stem.rsplit_once('-'))
            .and_then(|(_, turn)| turn.parse::<u32>().ok())
            .unwrap_or(0)
            + 1;
        let file = format!("{id}-{turn}.{extension}");
        self.store_image(&file, bytes)?;
        let changed = self.with_plans(|plans| {
            let plan = plans
                .iter_mut()
                .find(|plan| plan.id == id)
                .ok_or_else(Refusal::missing)?;
            plan.image.clone_from(&file);
            Ok(view(plan))
        })?;
        self.drop_image(&old);
        Ok(changed)
    }

    fn remove_plan(&self, id: &str) -> Result<(), Refusal> {
        let image = self.with_plans(|plans| {
            let at = plans
                .iter()
                .position(|plan| plan.id == id)
                .ok_or_else(Refusal::missing)?;
            Ok(plans.remove(at).image)
        })?;
        self.drop_image(&image);
        Ok(())
    }
}

#[derive(Deserialize)]
pub(crate) struct Named {
    #[serde(default)]
    name: String,
}

pub(crate) async fn list(Open(programmer): Open) -> Response {
    let plans: Vec<Value> = programmer.shared().state.plans.iter().map(view).collect();
    ok(&json!({ "plans": plans }))
}

pub(crate) async fn create(
    Open(programmer): Open,
    Query(named): Query<Named>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let plan = programmer.create_plan(&named.name, &body)?;
    Ok(json_response(StatusCode::CREATED, &plan))
}

pub(crate) async fn change(
    Open(programmer): Open,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    Ok(ok(&programmer.change_plan(&id, &body_json(&body)?)?))
}

pub(crate) async fn remove(
    Open(programmer): Open,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    programmer.remove_plan(&id)?;
    Ok(ok(&json!({ "removed": id })))
}

pub(crate) async fn image(
    Open(programmer): Open,
    Path(id): Path<String>,
) -> Result<Response, Refusal> {
    let file = programmer
        .shared()
        .state
        .plans
        .iter()
        .find(|plan| plan.id == id)
        .map(|plan| plan.image.clone())
        .ok_or_else(Refusal::missing)?;
    let bytes = programmer.load_image(&file).ok_or_else(Refusal::missing)?;
    // Told by its bytes again: what is served is what was checked.
    let (_, media) = kind(&bytes).ok_or_else(Refusal::missing)?;
    Ok((
        [
            (header::CONTENT_TYPE, media),
            (
                header::CACHE_CONTROL,
                "private, max-age=31536000, immutable",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

pub(crate) async fn replace_image(
    Open(programmer): Open,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    Ok(ok(&programmer.replace_plan_image(&id, &body)?))
}
