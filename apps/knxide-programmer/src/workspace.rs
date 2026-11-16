//! The projects the server knows, and the one that is open.
//!
//! A project is an ETS export with the file of edits beside it. The library is a directory
//! with an index (`projects.json`) and one directory per project that was brought in through
//! the page. A project named on the command line stays where it is and is only listed: it is
//! never copied, moved or deleted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path as UrlPath, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use knxide_core::IndividualAddress;
use knxide_program::{Project, ProjectArchive};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    Bus, Programmer, Refusal, Settings, body_json, export, guard, json_response, now_ms, ok,
    sidecar,
};
use crate::link::BusAccess;
use crate::sim::SimBus;
use crate::source::load_project_in;

/// The largest project file the page may bring in.
pub const MAX_PROJECT: usize = 512 * 1024 * 1024;

/// The name of the index in the library directory.
const INDEX: &str = "projects.json";
/// The name a project file gets inside the library.
const PROJECT_FILE: &str = "project.knxproj";

/// How the projects that are opened reach a bus.
#[derive(Debug, Clone, Default)]
pub enum BusConfig {
    /// Not at all.
    #[default]
    None,
    /// Over KNXnet/IP.
    Access(BusAccess),
    /// Over KNXnet/IP routing, on the interface chosen each time the bus is used.
    AutoRouting {
        /// The individual address of the tool.
        tool: IndividualAddress,
    },
    /// Simulated devices made from the project that is opened.
    Simulated {
        /// The individual address of the tool.
        tool: IndividualAddress,
        /// The pause per frame, to go at about the pace of a real bus.
        frame_delay: Duration,
    },
}

impl BusConfig {
    fn bus(&self, project: &Project) -> Bus {
        match self {
            Self::None => Bus::None,
            Self::Access(access) => Bus::Access(access.clone()),
            Self::AutoRouting { tool } => Bus::AutoRouting(*tool),
            Self::Simulated { tool, frame_delay } => {
                Bus::simulate_on(SimBus::new().with_frame_delay(*frame_delay), project, *tool)
            }
        }
    }
}

/// One project of the list.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    name: String,
    /// The project file, or the directory with its unpacked content.
    path: PathBuf,
    /// Whether the file lives in the library, and goes when the project is deleted.
    owned: bool,
    /// Where its edits are kept, when not beside the project file.
    #[serde(default)]
    state: Option<PathBuf>,
    created_ms: u64,
    #[serde(default)]
    opened_ms: Option<u64>,
    /// What the page last worked out about the project, for the list.
    #[serde(default)]
    summary: Value,
}

impl Entry {
    fn state_path(&self) -> PathBuf {
        self.state
            .clone()
            .unwrap_or_else(|| sidecar::default_path(&self.path))
    }

    fn view(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "file": self.path.file_name().map(|name| name.to_string_lossy().into_owned()),
            "owned": self.owned,
            "created_ms": self.created_ms,
            "opened_ms": self.opened_ms,
            "summary": self.summary,
        })
    }
}

#[derive(Default)]
struct Shared {
    entries: Vec<Entry>,
    /// The open project: its identifier in the list and its server state.
    open: Option<(String, Programmer)>,
    next: u64,
}

struct Inner {
    /// Where projects brought in through the page are kept; `None` for a server that serves
    /// one project and no list.
    library: Option<PathBuf>,
    bus: BusConfig,
    settings: Settings,
    language: String,
    shared: Mutex<Shared>,
}

/// The server's state: the list of projects and the open one.
#[derive(Clone)]
pub struct Workspace {
    inner: Arc<Inner>,
}

impl Workspace {
    /// A server for this one project, with no list of others.
    pub fn fixed(programmer: Programmer) -> Self {
        Self {
            inner: Arc::new(Inner {
                library: None,
                bus: BusConfig::None,
                settings: Settings::default(),
                language: "en-US".into(),
                shared: Mutex::new(Shared {
                    open: Some((String::new(), programmer)),
                    ..Shared::default()
                }),
            }),
        }
    }

    /// A server with the projects listed in `library`, a directory that is made when it is
    /// not there. Projects that are opened reach the bus as `bus` says and show the products'
    /// texts in `language`.
    pub fn library(
        library: PathBuf,
        bus: BusConfig,
        settings: Settings,
        language: String,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(&library)
            .map_err(|error| format!("{}: {error}", library.display()))?;
        let index = library.join(INDEX);
        let entries: Vec<Entry> = match std::fs::read(&index) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("{}: {error}", index.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("{}: {error}", index.display())),
        };
        Ok(Self {
            inner: Arc::new(Inner {
                library: Some(library),
                bus,
                settings,
                language,
                shared: Mutex::new(Shared {
                    entries,
                    ..Shared::default()
                }),
            }),
        })
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.inner
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The open project.
    pub fn current(&self) -> Option<Programmer> {
        self.shared()
            .open
            .as_ref()
            .map(|(_, programmer)| programmer.clone())
    }

    fn save(&self, shared: &Shared) -> Result<(), Refusal> {
        let Some(library) = &self.inner.library else {
            return Ok(());
        };
        let failed = |error: String| Refusal::new(StatusCode::INTERNAL_SERVER_ERROR, error);
        let body = serde_json::to_vec_pretty(&shared.entries)
            .map_err(|error| failed(error.to_string()))?;
        let path = library.join(INDEX);
        let temporary = library.join(format!("{INDEX}.tmp"));
        std::fs::write(&temporary, body)
            .and_then(|()| std::fs::rename(&temporary, &path))
            .map_err(|error| failed(format!("{}: {error}", path.display())))
    }

    fn new_id(shared: &mut Shared) -> String {
        shared.next += 1;
        format!("p{:x}{:x}", now_ms(), shared.next)
    }

    /// List the project at `path`, where it is, unless it is listed already. Its edits are
    /// kept in `state`, or beside it. Returns its identifier.
    pub fn register(&self, path: &Path, state: Option<PathBuf>) -> Result<String, String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let mut shared = self.shared();
        if let Some(entry) = shared.entries.iter_mut().find(|entry| entry.path == path) {
            if state.is_some() {
                entry.state = state;
            }
            return Ok(entry.id.clone());
        }
        let id = Self::new_id(&mut shared);
        let name = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        shared.entries.push(Entry {
            id: id.clone(),
            name,
            path,
            owned: false,
            state,
            created_ms: now_ms(),
            opened_ms: None,
            summary: Value::Null,
        });
        self.save(&shared).map_err(|refusal| refusal.1)?;
        Ok(id)
    }

    /// Open the project `id` of the list, as the page does. Fails with the reason in words.
    pub async fn open_listed(&self, id: &str) -> Result<(), String> {
        self.open(id).await.map_err(|refusal| refusal.1)
    }

    /// The project `id` of the list as a project file with its edits written in, as the
    /// page's export gives it: the name to save it under and its content. Fails with the
    /// reason in words.
    pub async fn export_listed(&self, id: &str) -> Result<(String, Vec<u8>), String> {
        self.export(id).await.map_err(|refusal| refusal.1)
    }

    /// Open the project `id` of the list: read it, with its edits, and make it the one the
    /// page works on. Refused while the open project has an operation running.
    pub(crate) async fn open(&self, id: &str) -> Result<(), Refusal> {
        let entry = {
            let shared = self.shared();
            if let Some((open, programmer)) = &shared.open {
                if open == id {
                    return Ok(());
                }
                if programmer.running() {
                    return Err(Refusal::new(
                        StatusCode::CONFLICT,
                        "an operation is running in the open project",
                    ));
                }
            }
            shared
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .cloned()
                .ok_or_else(Refusal::missing)?
        };
        let unusable = |error: String| Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, error);
        let path = entry.path.clone();
        let language = self.inner.language.clone();
        let project = tokio::task::spawn_blocking(move || load_project_in(&path, Some(&language)))
            .await
            .map_err(|error| unusable(error.to_string()))?
            .map_err(unusable)?;
        let bus = self.inner.bus.bus(&project);
        let settings = Settings {
            state: Some(entry.state_path()),
            ..self.inner.settings.clone()
        };
        let source = entry
            .path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let named = project.name.clone();
        let programmer = Programmer::open(project, source, bus, settings).map_err(unusable)?;
        let mut shared = self.shared();
        if let Some(entry) = shared.entries.iter_mut().find(|entry| entry.id == id) {
            entry.opened_ms = Some(now_ms());
            // A project listed from the command line is called by its file until it has
            // been read.
            if !entry.owned && entry.summary.is_null() && !named.is_empty() {
                entry.name = named;
            }
        }
        shared.open = Some((id.to_string(), programmer));
        self.save(&shared)
    }

    fn list(&self) -> Value {
        let shared = self.shared();
        json!({
            "projects": shared.entries.iter().map(Entry::view).collect::<Vec<_>>(),
            "open": shared.open.as_ref().map(|(id, _)| id.clone()),
            "library": self.inner.library.is_some(),
        })
    }

    /// Bring a project file in: check that it can be read, keep it in the library, list it.
    async fn create(&self, name: Option<String>, bytes: Vec<u8>) -> Result<Value, Refusal> {
        let library = self.inner.library.clone().ok_or_else(|| {
            Refusal::new(
                StatusCode::CONFLICT,
                "this server keeps no list of projects",
            )
        })?;
        let unusable = |error: String| Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, error);
        let language = self.inner.language.clone();
        // Read it once before keeping it: a file that cannot be opened is not listed.
        let (bytes, project_name) = tokio::task::spawn_blocking(move || {
            let name = {
                let archive = ProjectArchive::open(&bytes).map_err(|error| error.to_string())?;
                Project::load_in(&archive, Some(&language))
                    .map_err(|error| error.to_string())?
                    .name
            };
            Ok::<_, String>((bytes, name))
        })
        .await
        .map_err(|error| unusable(error.to_string()))?
        .map_err(unusable)?;

        let mut shared = self.shared();
        let id = Self::new_id(&mut shared);
        let directory = library.join(&id);
        let path = directory.join(PROJECT_FILE);
        std::fs::create_dir_all(&directory)
            .and_then(|()| std::fs::write(&path, &bytes))
            .map_err(|error| {
                Refusal::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("{}: {error}", path.display()),
                )
            })?;
        let name = name
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or(project_name);
        let entry = Entry {
            id,
            name: Self::unique_name(&shared, &name, None),
            path,
            owned: true,
            state: None,
            created_ms: now_ms(),
            opened_ms: None,
            summary: Value::Null,
        };
        let view = entry.view();
        shared.entries.push(entry);
        self.save(&shared)?;
        Ok(view)
    }

    /// `name`, or `name 2`, `name 3` and so on when another project has it.
    fn unique_name(shared: &Shared, name: &str, except: Option<&str>) -> String {
        let name = if name.is_empty() { "Project" } else { name };
        let taken = |candidate: &str| {
            shared.entries.iter().any(|entry| {
                Some(entry.id.as_str()) != except
                    && entry.name.to_lowercase() == candidate.to_lowercase()
            })
        };
        let mut candidate = name.to_string();
        let mut number = 2;
        while taken(&candidate) {
            candidate = format!("{name} {number}");
            number += 1;
        }
        candidate
    }

    /// A copy of the project `id` in the library, with its edits and floor plans.
    fn copy(&self, id: &str, name: &str) -> Result<Value, Refusal> {
        let library = self.inner.library.clone().ok_or_else(|| {
            Refusal::new(
                StatusCode::CONFLICT,
                "this server keeps no list of projects",
            )
        })?;
        let mut shared = self.shared();
        let source = shared
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(Refusal::missing)?;
        if source.path.is_dir() {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "an unpacked project cannot be copied; export it from ETS as a file",
            ));
        }
        let new_id = Self::new_id(&mut shared);
        let directory = library.join(&new_id);
        let path = directory.join(PROJECT_FILE);
        let state = sidecar::default_path(&path);
        let copied = || -> std::io::Result<()> {
            std::fs::create_dir_all(&directory)?;
            std::fs::copy(&source.path, &path)?;
            let from = source.state_path();
            if from.is_file() {
                std::fs::copy(&from, &state)?;
            }
            let plans = sidecar::plans_directory(&from);
            if plans.is_dir() {
                let target = sidecar::plans_directory(&state);
                std::fs::create_dir_all(&target)?;
                for file in std::fs::read_dir(&plans)? {
                    let file = file?;
                    if file.path().is_file() {
                        std::fs::copy(file.path(), target.join(file.file_name()))?;
                    }
                }
            }
            Ok(())
        };
        copied().map_err(|error| {
            let _ = std::fs::remove_dir_all(&directory);
            Refusal::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
        })?;
        let wanted = if name.trim().is_empty() {
            format!("{} copy", source.name)
        } else {
            name.trim().to_string()
        };
        let entry = Entry {
            id: new_id,
            name: Self::unique_name(&shared, &wanted, None),
            path,
            owned: true,
            state: None,
            created_ms: now_ms(),
            opened_ms: None,
            summary: source.summary.clone(),
        };
        let view = entry.view();
        shared.entries.push(entry);
        self.save(&shared)?;
        Ok(view)
    }

    fn change(&self, id: &str, body: &Value) -> Result<Value, Refusal> {
        let mut shared = self.shared();
        let name = body["name"].as_str().map(|name| name.trim().to_string());
        if let Some(name) = &name
            && name.is_empty()
        {
            return Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "a project needs a name",
            ));
        }
        let unique = name.map(|name| Self::unique_name(&shared, &name, Some(id)));
        let entry = shared
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(Refusal::missing)?;
        if let Some(name) = unique {
            entry.name = name;
        }
        if let Some(summary) = body.get("summary") {
            entry.summary = summary.clone();
        }
        let view = entry.view();
        self.save(&shared)?;
        Ok(view)
    }

    /// Take the project `id` off the list. A project kept in the library goes with its edits;
    /// one that was only listed stays where it is.
    fn remove(&self, id: &str) -> Result<(), Refusal> {
        let mut shared = self.shared();
        let position = shared
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or_else(Refusal::missing)?;
        if let Some((open, programmer)) = &shared.open
            && open == id
        {
            if programmer.running() {
                return Err(Refusal::new(
                    StatusCode::CONFLICT,
                    "an operation is running in this project",
                ));
            }
            shared.open = None;
        }
        let entry = shared.entries.remove(position);
        if entry.owned
            && let (Some(library), Some(directory)) = (&self.inner.library, entry.path.parent())
            && directory.parent() == Some(library.as_path())
        {
            // Only ever a directory of the library, made by this server.
            let _ = std::fs::remove_dir_all(directory);
        }
        self.save(&shared)
    }

    fn close(&self) -> Result<(), Refusal> {
        let mut shared = self.shared();
        if let Some((_, programmer)) = &shared.open
            && programmer.running()
        {
            return Err(Refusal::new(
                StatusCode::CONFLICT,
                "an operation is running in the open project",
            ));
        }
        if self.inner.library.is_some() {
            shared.open = None;
        }
        Ok(())
    }

    /// The project `id` as a project file with the edits written into it, and a name for it.
    async fn export(&self, id: &str) -> Result<(String, Vec<u8>), Refusal> {
        let (entry, open) = {
            let shared = self.shared();
            let entry = shared
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .cloned()
                .ok_or_else(Refusal::missing)?;
            let open = shared
                .open
                .as_ref()
                .filter(|(open, _)| open == id)
                .map(|(_, programmer)| programmer.clone());
            (entry, open)
        };
        let unusable = |error: String| Refusal::new(StatusCode::UNPROCESSABLE_ENTITY, error);
        if entry.path.is_dir() {
            return Err(unusable(
                "an unpacked project cannot be exported; open the project file instead".into(),
            ));
        }
        let language = self.inner.language.clone();
        let state_path = entry.state_path();
        let path = entry.path.clone();
        let bytes = tokio::task::spawn_blocking(move || {
            let original =
                std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            // The edits as they are on disk; the open project writes them at every change.
            let (project, state) = match open {
                Some(programmer) => {
                    let state = programmer.state_copy();
                    (programmer.project().clone(), state)
                }
                None => {
                    let project = load_project_in(&path, Some(&language))?;
                    let state = sidecar::State::load(&state_path, &project.id)?;
                    (project, state)
                }
            };
            export::project_file(&original, &project, &state)
        })
        .await
        .map_err(|error| unusable(error.to_string()))?
        .map_err(unusable)?;
        Ok((format!("{}.knxproj", entry.name), bytes))
    }
}

/// The open project, for a request that works on one. 409 when none is open.
pub(crate) struct Open(pub(crate) Programmer);

impl FromRequestParts<Workspace> for Open {
    type Rejection = Refusal;

    async fn from_request_parts(
        _parts: &mut Parts,
        workspace: &Workspace,
    ) -> Result<Self, Self::Rejection> {
        workspace
            .current()
            .map(Open)
            .ok_or_else(|| Refusal::new(StatusCode::CONFLICT, "no project is open"))
    }
}

/// The routes of the list of projects.
pub(crate) fn routes() -> Router<Workspace> {
    Router::new()
        .route(
            "/api/projects",
            get(list)
                .post(create)
                .layer(DefaultBodyLimit::max(MAX_PROJECT)),
        )
        .route("/api/projects/close", post(close))
        .route("/api/projects/{id}", put(change).delete(remove))
        .route("/api/projects/{id}/open", post(open))
        .route("/api/projects/{id}/copy", post(copy))
        .route("/api/projects/{id}/export", get(export_file))
}

async fn list(State(workspace): State<Workspace>) -> Response {
    ok(&workspace.list())
}

async fn create(
    State(workspace): State<Workspace>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let project = workspace
        .create(query.get("name").cloned(), body.to_vec())
        .await?;
    Ok(json_response(StatusCode::CREATED, &project))
}

async fn open(
    State(workspace): State<Workspace>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    workspace.open(&id).await?;
    Ok(ok(&workspace.list()))
}

async fn close(
    State(workspace): State<Workspace>,
    headers: HeaderMap,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    workspace.close()?;
    Ok(ok(&workspace.list()))
}

async fn copy(
    State(workspace): State<Workspace>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    let body = body_json(&body)?;
    let project = workspace.copy(&id, body["name"].as_str().unwrap_or_default())?;
    Ok(json_response(StatusCode::CREATED, &project))
}

async fn change(
    State(workspace): State<Workspace>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: String,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    Ok(ok(&workspace.change(&id, &body_json(&body)?)?))
}

async fn remove(
    State(workspace): State<Workspace>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, Refusal> {
    guard(&headers)?;
    workspace.remove(&id)?;
    Ok(ok(&workspace.list()))
}

async fn export_file(
    State(workspace): State<Workspace>,
    UrlPath(id): UrlPath<String>,
) -> Result<Response, Refusal> {
    let (name, bytes) = workspace.export(&id).await?;
    // The name goes into a header: keep to what a header may carry and say the rest in
    // the encoded form browsers read first.
    let plain: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " .-_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b".-_".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect();
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{encoded}"),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        bytes,
    )
        .into_response())
}
