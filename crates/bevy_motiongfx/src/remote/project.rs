extern crate alloc;
// The crate is `#![no_std]`. The `remote` feature implies `std`, but
// file IO still needs the explicit crate.
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use bevy_ecs::prelude::*;
use bevy_ecs::reflect::{AppTypeRegistry, ReflectComponent};
use bevy_ecs::system::In;
use bevy_reflect::TypePath;
use bevy_reflect::serde::TypedReflectSerializer;
use bevy_remote::{BrpResult, error_codes};
use serde::Deserialize;
use serde_json::{Value, json};

use super::edit::{err, invalid, parse};
use super::{dynamic, persist};

/// The current project document format/version.
///
/// 2: multi-timeline - `timelines: [{id, name?, document}]`
/// replaces the single `timeline` field (version-1 documents still
/// load).
pub const FORMAT: &str = "motiongfx-project";
pub const FORMAT_VERSION: u64 = 2;

/// Which component types make an entity part of the project. Filled
/// by [`MotionGfxProjectApp::register_project_subject`].
#[derive(Resource, Clone)]
pub struct MotionGfxProjectConfig {
    /// An entity belongs to the project if it carries any of these
    /// (fully-qualified type paths).
    pub subjects: Vec<String>,
    /// Also saved from each project entity when present.
    pub extras: Vec<String>,
}

impl Default for MotionGfxProjectConfig {
    fn default() -> Self {
        Self {
            subjects: Vec::new(),
            extras: alloc::vec![
                "bevy_transform::components::transform::Transform"
                    .to_string(),
                "bevy_ecs::name::Name".to_string(),
            ],
        }
    }
}

/// App extension: declare a component type a project subject.
pub trait MotionGfxProjectApp {
    /// Entities carrying `T` are saved by `motiongfx.project_save`
    /// (with their `Transform`/`Name`) and respawned by
    /// `project_load`. `T` must be reflect-registered and spawnable
    /// over reflection (the `motiongfx.spawn` requirements).
    fn register_project_subject<T: TypePath>(&mut self) -> &mut Self;
}

impl MotionGfxProjectApp for bevy_app::App {
    fn register_project_subject<T: TypePath>(&mut self) -> &mut Self {
        self.init_resource::<MotionGfxProjectConfig>();
        let mut config =
            self.world_mut().resource_mut::<MotionGfxProjectConfig>();
        let path = T::type_path().to_string();
        if !config.subjects.contains(&path) {
            config.subjects.push(path);
        }
        self
    }
}


#[derive(Deserialize)]
struct MotionGfxSaveParams {
    /// Timeline to bundle. Omitted = **all** of the manager's
    /// timelines.
    #[serde(default)]
    id: Option<u64>,
    /// Where to write the document, server-side. Omitted = the
    /// document is returned instead of written.
    #[serde(default)]
    path: Option<String>,
}

/// `motiongfx.project_save {id?, path?}` - bundle the scene's project
/// entities and the timeline document. Write to `path` (server-side)
/// or return the document.
pub fn project_save(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxSaveParams = parse(params)?;

    // The timeline half (clips + markers), via the existing exporter
    // - one document per timeline (all of them unless `id` filters).
    let ids: Vec<u64> = match p.id {
        Some(id) => alloc::vec![id],
        None => {
            let mut ids: Vec<u64> = world
                .get_resource::<crate::manager::MotionGfxManager>()
                .map(|m| {
                    m.iter_ids()
                        .map(crate::manager::TimelineId::raw)
                        .collect()
                })
                .unwrap_or_default();
            ids.sort_unstable();
            ids
        }
    };
    let mut timelines = Vec::with_capacity(ids.len());
    for id in ids {
        let document = persist::timeline_export(
            In(Some(json!({ "id": id }))),
            world,
        )?;
        let name = world
            .get_resource::<super::state::MotionGfxEditState>()
            .and_then(|s| {
                s.name(&crate::manager::TimelineId::from_raw(id))
                    .map(ToString::to_string)
            });
        timelines.push(json!({
            "id": id,
            "name": name,
            "document": document,
        }));
    }

    let config = world
        .get_resource::<MotionGfxProjectConfig>()
        .cloned()
        .unwrap_or_default();
    if config.subjects.is_empty() {
        return Err(err(
            error_codes::RESOURCE_ERROR,
            "no project subjects registered; call \
             App::register_project_subject::<T>() for every \
             spawnable type",
        ));
    }

    let app_registry = world.resource::<AppTypeRegistry>().clone();
    let registry = app_registry.read();
    let resolve = |path: &String| {
        registry
            .get_with_type_path(path)
            .and_then(|r| r.data::<ReflectComponent>())
            .map(|rc| (path.clone(), rc))
    };
    let subject_comps: Vec<_> =
        config.subjects.iter().filter_map(resolve).collect();
    let extra_comps: Vec<_> =
        config.extras.iter().filter_map(resolve).collect();

    let mut entities = Vec::new();
    let mut all = world.query::<EntityRef>();
    for entity_ref in all.iter(world) {
        let qualifies = subject_comps
            .iter()
            .any(|(_, rc)| rc.reflect(entity_ref).is_some());
        if !qualifies {
            continue;
        }
        let mut components = serde_json::Map::new();
        for (path, rc) in
            subject_comps.iter().chain(extra_comps.iter())
        {
            let Some(reflected) = rc.reflect(entity_ref) else {
                continue;
            };
            let serialized =
                serde_json::to_value(TypedReflectSerializer::new(
                    reflected.as_partial_reflect(),
                    &registry,
                ))
                .map_err(|e| {
                    err(
                        error_codes::COMPONENT_ERROR,
                        format!("cannot serialize `{path}`: {e}"),
                    )
                })?;
            components.insert(path.clone(), serialized);
        }
        let name =
            entity_ref.get::<Name>().map(|n| n.as_str().to_string());
        entities.push(json!({
            "name": name,
            "components": Value::Object(components),
        }));
    }
    drop(registry);

    let count = entities.len();
    let doc = json!({
        "format": FORMAT,
        "format_version": FORMAT_VERSION,
        "entities": entities,
        "timelines": timelines,
    });

    match p.path {
        Some(path) => {
            let pretty =
                serde_json::to_string_pretty(&doc).map_err(|e| {
                    err(error_codes::INTERNAL_ERROR, e.to_string())
                })?;
            std::fs::write(&path, pretty).map_err(|e| {
                invalid(format!("cannot write `{path}`: {e}"))
            })?;
            Ok(json!({ "entities": count, "path": path }))
        }
        None => Ok(json!({
            "entities": count,
            "document": doc,
        })),
    }
}


#[derive(Deserialize)]
struct MotionGfxLoadParams {
    /// Target timeline for *single-timeline* documents (default `0`).
    /// Multi-timeline documents address timelines themselves: each
    /// entry loads into its own id when the manager knows it, and a
    /// fresh timeline is created otherwise.
    #[serde(default)]
    id: Option<u64>,
    /// Server-side file to read.
    #[serde(default)]
    path: Option<String>,
    /// Inline document (mutually exclusive with `path`).
    #[serde(default)]
    document: Option<Value>,
}

/// Despawn every entity carrying a registered project-subject component
/// (the set [`project_save`] collects), returning the count. The
/// stage-wipe half of [`project_load`], exposed so clients can reset the
/// stage on its own. Timelines and non-subject entities are untouched.
pub fn despawn_project_subjects(world: &mut World) -> usize {
    let config = world
        .get_resource::<MotionGfxProjectConfig>()
        .cloned()
        .unwrap_or_default();
    let app_registry = world.resource::<AppTypeRegistry>().clone();
    let doomed: Vec<Entity> = {
        let registry = app_registry.read();
        let subject_comps: Vec<_> = config
            .subjects
            .iter()
            .filter_map(|path| {
                registry
                    .get_with_type_path(path)
                    .and_then(|r| r.data::<ReflectComponent>())
            })
            .collect();
        let mut all = world.query::<EntityRef>();
        all.iter(world)
            .filter(|e| {
                subject_comps
                    .iter()
                    .any(|rc| rc.reflect(*e).is_some())
            })
            .map(|e| e.id())
            .collect()
    };
    let count = doomed.len();
    for entity in doomed {
        world.despawn(entity);
    }
    count
}

/// `motiongfx.project_reset` - despawn every registered project subject
/// (the stage-wipe half of [`project_load`]), leaving timelines and
/// app-owned entities untouched. Safe against apps that register no
/// project subjects: it despawns nothing.
pub fn project_reset(
    In(_params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let despawned = despawn_project_subjects(world);
    Ok(json!({ "despawned": despawned }))
}

/// `motiongfx.project_load {id?, path? | document?}` - replace the
/// current project: despawn existing project subjects, respawn the
/// document's entities (reflection, like `motiongfx.spawn`), then
/// import the timeline with name bindings to the fresh entities.
pub fn project_load(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxLoadParams = parse(params)?;
    let id = p.id.unwrap_or(0);

    let doc = match (p.document, p.path) {
        (Some(doc), _) => doc,
        (None, Some(path)) => {
            let text =
                std::fs::read_to_string(&path).map_err(|e| {
                    invalid(format!("cannot read `{path}`: {e}"))
                })?;
            serde_json::from_str(&text).map_err(|e| {
                invalid(format!("`{path}` is not JSON: {e}"))
            })?
        }
        (None, None) => {
            return Err(invalid(
                "pass `path` or `document`".to_string(),
            ));
        }
    };
    if doc["format"] != json!(FORMAT) {
        return Err(invalid(format!(
            "not a {FORMAT} document (format = {})",
            doc["format"]
        )));
    }

    // Replace semantics: clear the current cast first.
    let despawned = despawn_project_subjects(world);

    // Respawn the cast through the same path as `motiongfx.spawn`.
    let empty = Vec::new();
    let doc_entities = doc["entities"].as_array().unwrap_or(&empty);
    let mut bindings = serde_json::Map::new();
    for ent in doc_entities {
        let result = dynamic::brp_spawn(
            In(Some(json!({ "components": ent["components"] }))),
            world,
        )?;
        if let (Some(name), Some(bits)) =
            (ent["name"].as_str(), result["entity"].as_u64())
        {
            bindings.insert(name.to_string(), json!(bits));
        }
    }

    // The timeline half, bound by name to the fresh entities. A v2
    // document carries several timelines. V1 carries one.
    let entries: Vec<(Option<u64>, Option<String>, Value)> = match doc
        ["timelines"]
        .as_array()
    {
        Some(list) => list
            .iter()
            .map(|t| {
                (
                    t["id"].as_u64(),
                    t["name"].as_str().map(ToString::to_string),
                    t["document"].clone(),
                )
            })
            .collect(),
        None => alloc::vec![(None, None, doc["timeline"].clone())],
    };

    let mut results = Vec::with_capacity(entries.len());
    for (doc_id, name, document) in entries {
        // Prefer the document's own id when the manager knows it.
        // Single-timeline documents honour the request's `id`, and a
        // missing timeline is created on the spot (the blank-app
        // story).
        let known = |world: &World, raw: u64| {
            world
                .get_resource::<crate::manager::MotionGfxManager>()
                .is_some_and(|m| {
                    m.get_timeline(
                        &crate::manager::TimelineId::from_raw(raw),
                    )
                    .is_some()
                })
        };
        let target = match doc_id {
            Some(raw) if known(world, raw) => raw,
            None => id,
            Some(_) => {
                let created = crate::remote::timeline_create(
                    In(Some(json!({}))),
                    world,
                )?;
                created["id"].as_u64().ok_or_else(|| {
                    err(
                        error_codes::INTERNAL_ERROR,
                        "timeline_create returned no id",
                    )
                })?
            }
        };
        let imported = persist::timeline_import(
            In(Some(json!({
                "id": target,
                "document": document,
                "mode": "replace",
                "bindings": Value::Object(bindings.clone()),
            }))),
            world,
        )?;
        if let Some(name) = name
            && let Some(mut state) = world
                .get_resource_mut::<super::state::MotionGfxEditState>(
            )
        {
            state.set_name(
                crate::manager::TimelineId::from_raw(target),
                Some(name),
            );
        }
        results.push(json!({
            "from": doc_id,
            "into": target,
            "result": imported,
        }));
    }

    Ok(json!({
        "entities": doc_entities.len(),
        "despawned": despawned,
        // Back-compat alias: the first timeline's import result.
        "timeline": results.first()
            .map(|r| r["result"].clone())
            .unwrap_or(Value::Null),
        "timelines": results,
    }))
}
