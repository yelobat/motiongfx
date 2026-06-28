extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use bevy_ecs::name::Name;
use bevy_ecs::prelude::*;
use bevy_ecs::system::In;
use bevy_platform::collections::HashMap;
use bevy_remote::{BrpResult, error_codes};
use serde::Deserialize;
use serde_json::{Value, json};

use super::batch::{self, EditOp};
use super::edit::{err, invalid, ok, parse, require_manager};
use super::state::{Marker, MotionGfxEditState};
use crate::manager::{MotionGfxManager, TimelineId};

pub(crate) const FORMAT: &str = "motiongfx-timeline";
/// 2 (phase 6): actions may carry `keyframes: [{t, value, ease?}]`
/// instead of `to`. Version-1 documents still import.
///
/// 3: actions may carry `label`, `color: [r,g,b]` and
/// `enabled: false` (mute). Versions 1-2 still import.
pub(crate) const FORMAT_VERSION: u64 = 3;


#[derive(Deserialize)]
struct MotionGfxExportParams {
    id: u64,
}

/// `motiongfx.timeline_export` - snapshot a timeline as a document.
pub fn timeline_export(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxExportParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, mut manager| {
            let ops =
                batch::snapshot_all_clips(world, &mut manager, tid)?;
            let track_count = manager
                .get_timeline(&tid)
                .ok_or_else(|| {
                    err(
                        error_codes::RESOURCE_ERROR,
                        format!("No timeline with id {}", p.id),
                    )
                })?
                .tracks()
                .len();

            // Entities whose Name is shared cannot be told apart at
            // import time. Export those by raw bits instead.
            let mut name_owner: HashMap<String, u64> = HashMap::new();
            let mut ambiguous: HashMap<String, ()> = HashMap::new();
            for op in &ops {
                if let EditOp::Insert { entity, .. } = op
                    && let Some(name) = Entity::try_from_bits(*entity)
                        .and_then(|e| world.get::<Name>(e))
                {
                    let name = name.as_str().to_string();
                    match name_owner.get(&name) {
                        Some(owner) if *owner != *entity => {
                            ambiguous.insert(name, ());
                        }
                        _ => {
                            name_owner.insert(name, *entity);
                        }
                    }
                }
            }

            let mut tracks: Vec<Vec<Value>> =
                (0..track_count).map(|_| Vec::new()).collect();
            let mut unserializable: Vec<String> = Vec::new();
            let mut warnings: Vec<String> = Vec::new();

            // One subject resolution for both insert flavours.
            let catalog =
                world.resource::<super::catalog::MotionGfxCatalog>();
            let subject_json = |component: &str,
                                field: &str,
                                entity: u64,
                                warnings: &mut Vec<String>|
             -> Value {
                if catalog.get(component, field).is_some_and(|e| {
                    matches!(
                        e.subject,
                        super::catalog::SubjectKind::Resource
                    )
                }) {
                    return json!({ "resource": true });
                }
                let name = Entity::try_from_bits(entity)
                    .and_then(|e| world.get::<Name>(e))
                    .map(|n| n.as_str().to_string())
                    .filter(|n| !ambiguous.contains_key(n));
                match name {
                    Some(name) => json!({ "name": name }),
                    None => {
                        warnings.push(format!(
                            "entity {entity} has no unique Name; \
                             exported as raw bits (re-bind on \
                             import via `bindings`)"
                        ));
                        json!({ "entity": entity })
                    }
                }
            };

            for op in ops {
                match op {
                    EditOp::Insert {
                        track,
                        entity,
                        component,
                        field,
                        asset_of,
                        to,
                        duration,
                        start_at,
                        ease,
                        label,
                        color,
                        enabled,
                        ..
                    } => {
                        let mut action = json!({
                            "subject": subject_json(
                                &component,
                                &field,
                                entity,
                                &mut warnings,
                            ),
                            "component": component,
                            "field": field,
                            "to": to,
                            "start": start_at.unwrap_or(0.0),
                            "duration": duration,
                        });
                        if let Some(ease) = ease {
                            action["ease"] = ease;
                        }
                        if let Some(label) = label {
                            action["label"] = json!(label);
                        }
                        if let Some(color) = color {
                            action["color"] = json!(color);
                        }
                        if enabled == Some(false) {
                            action["enabled"] = json!(false);
                        }
                        if let Some(asset_of) = asset_of {
                            action["asset_of"] = json!(asset_of);
                        }
                        tracks[track].push(action);
                    }
                    EditOp::InsertKeyframes {
                        track,
                        entity,
                        component,
                        field,
                        asset_of,
                        keyframes,
                        duration,
                        start_at,
                        ease,
                        label,
                        color,
                        enabled,
                        ..
                    } => {
                        let mut action = json!({
                            "subject": subject_json(
                                &component,
                                &field,
                                entity,
                                &mut warnings,
                            ),
                            "component": component,
                            "field": field,
                            "keyframes": keyframes,
                            "start": start_at.unwrap_or(0.0),
                            "duration": duration,
                        });
                        if let Some(ease) = ease {
                            action["ease"] = ease;
                        }
                        if let Some(label) = label {
                            action["label"] = json!(label);
                        }
                        if let Some(color) = color {
                            action["color"] = json!(color);
                        }
                        if enabled == Some(false) {
                            action["enabled"] = json!(false);
                        }
                        if let Some(asset_of) = asset_of {
                            action["asset_of"] = json!(asset_of);
                        }
                        tracks[track].push(action);
                    }
                    EditOp::Unrestorable { reason } => {
                        unserializable.push(reason);
                    }
                    _ => unreachable!(
                        "snapshot_all_clips only yields inserts"
                    ),
                }
            }

            if !unserializable.is_empty() {
                return Err(bevy_remote::BrpError {
                    code: error_codes::COMPONENT_ERROR,
                    message: format!(
                        "{} clip(s) cannot be exported; register \
                         their fields as animatable",
                        unserializable.len()
                    ),
                    data: Some(json!({
                        "unserializable": unserializable,
                    })),
                });
            }

            let markers: BTreeMap<String, Marker> = world
                .get_resource::<MotionGfxEditState>()
                .and_then(|s| s.markers(&tid).cloned())
                .unwrap_or_default();

            let mut doc = json!({
                "format": FORMAT,
                "format_version": FORMAT_VERSION,
                "tracks": tracks
                    .into_iter()
                    .map(|actions| json!({ "actions": actions }))
                    .collect::<Vec<_>>(),
                "markers": serde_json::to_value(markers)
                    .unwrap_or_else(|_| json!({})),
            });
            if !warnings.is_empty() {
                doc["warnings"] = json!(warnings);
            }
            Ok(doc)
        },
    )
}


#[derive(Deserialize)]
struct MotionGfxImportParams {
    id: u64,
    document: DocumentParams,
    /// `"replace"` (default) clears the timeline first. `"append"`
    /// adds on top of the existing content.
    #[serde(default)]
    mode: Option<String>,
    /// Explicit `name -> entity bits` overrides, taking precedence
    /// over `Name`-component resolution.
    #[serde(default)]
    bindings: Option<HashMap<String, u64>>,
}

#[derive(Deserialize)]
struct DocumentParams {
    format: String,
    format_version: u64,
    tracks: Vec<DocTrack>,
    #[serde(default)]
    markers: BTreeMap<String, Marker>,
}

#[derive(Deserialize)]
struct DocTrack {
    actions: Vec<DocAction>,
}

#[derive(Deserialize)]
struct DocAction {
    subject: DocSubject,
    component: String,
    field: String,
    /// Constant tween target. Exactly one of `to` / `keyframes`.
    #[serde(default)]
    to: Option<Value>,
    /// Keyframed clip points (format_version 2).
    #[serde(default)]
    keyframes: Option<Vec<super::batch::KeyframeDoc>>,
    start: f32,
    duration: f32,
    #[serde(default)]
    ease: Option<Value>,
    /// Optional clip metadata.
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    color: Option<[u8; 3]>,
    /// `false` = the clip is muted (non-destructive disable).
    #[serde(default)]
    enabled: Option<bool>,
    /// Asset addressing: the handle-bearing component on
    /// the subject entity. `component` is then the asset's type path.
    #[serde(default)]
    asset_of: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DocSubject {
    Name {
        name: String,
    },
    Entity {
        entity: u64,
    },
    /// A resource subject: `{ "resource": true }`.
    Resource {
        /// Marker key. Only its presence matters.
        #[allow(dead_code)]
        resource: bool,
    },
}

/// `motiongfx.timeline_import` - load a document into a timeline.
/// Validation happens before any mutation, and the apply phase reuses
/// the batch machinery, so a mid-apply failure unwinds cleanly. The
/// whole import journals as one entry (one undo step).
pub fn timeline_import(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxImportParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    if p.document.format != FORMAT {
        return Err(invalid(format!(
            "unknown format `{}` (expected `{FORMAT}`)",
            p.document.format
        )));
    }
    if !(1..=FORMAT_VERSION).contains(&p.document.format_version) {
        return Err(invalid(format!(
            "unsupported format_version {} (expected \
             1..={FORMAT_VERSION})",
            p.document.format_version
        )));
    }
    let replace = match p.mode.as_deref() {
        None | Some("replace") => true,
        Some("append") => false,
        Some(other) => {
            return Err(invalid(format!(
                "unknown mode `{other}` (expected `replace` or \
                 `append`)"
            )));
        }
    };

    let bindings = p.bindings.unwrap_or_default();
    let mut by_name: HashMap<String, u64> = HashMap::new();
    {
        let mut q = world.query::<(Entity, &Name)>();
        for (entity, name) in q.iter(world) {
            by_name
                .entry(name.as_str().to_string())
                .or_insert(entity.to_bits());
        }
    }

    let track_count = p.document.tracks.len();
    let mut ops: Vec<EditOp> = Vec::new();
    for (track, doc_track) in p.document.tracks.iter().enumerate() {
        for (i, action) in doc_track.actions.iter().enumerate() {
            let entity = match &action.subject {
                DocSubject::Resource { .. } => 0,
                DocSubject::Entity { entity } => *entity,
                DocSubject::Name { name } => bindings
                    .get(name)
                    .or_else(|| by_name.get(name))
                    .copied()
                    .ok_or_else(|| {
                        err(
                            error_codes::ENTITY_NOT_FOUND,
                            format!(
                                "track {track} action {i}: no entity \
                                 named `{name}` (pass it via \
                                 `bindings`)"
                            ),
                        )
                    })?,
            };

            ops.push(match (&action.to, &action.keyframes) {
                (Some(to), None) => EditOp::Insert {
                    track,
                    entity,
                    component: action.component.clone(),
                    field: action.field.clone(),
                    asset_of: action.asset_of.clone(),
                    to: to.clone(),
                    to_relative: None,
                    duration: action.duration,
                    start_at: Some(action.start),
                    // Documents carry exact starts. Appending after
                    // same-key clips would distort the layout.
                    exact: true,
                    restore_id: None,
                    ease: action.ease.clone(),
                    label: action.label.clone(),
                    color: action.color,
                    enabled: action.enabled,
                },
                (None, Some(keyframes)) => EditOp::InsertKeyframes {
                    track,
                    entity,
                    component: action.component.clone(),
                    field: action.field.clone(),
                    asset_of: action.asset_of.clone(),
                    keyframes: keyframes.clone(),
                    duration: action.duration,
                    start_at: Some(action.start),
                    exact: true,
                    restore_id: None,
                    ease: action.ease.clone(),
                    label: action.label.clone(),
                    color: action.color,
                    enabled: action.enabled,
                },
                _ => {
                    return Err(invalid(format!(
                        "track {track} action {i}: exactly one of \
                         `to` / `keyframes` is required"
                    )));
                }
            });
        }
    }

    {
        let catalog = world
            .get_resource::<super::catalog::MotionGfxCatalog>()
            .ok_or_else(|| {
                err(
                    error_codes::RESOURCE_ERROR,
                    "MotionGfxCatalog resource missing",
                )
            })?;
        let type_registry = world
            .resource::<bevy_ecs::reflect::AppTypeRegistry>()
            .clone();
        let registry = type_registry.read();
        for op in &ops {
            let (component, field, to, keyframes) = match op {
                EditOp::Insert {
                    component,
                    field,
                    to,
                    ..
                } => (component, field, Some(to), None),
                EditOp::InsertKeyframes {
                    component,
                    field,
                    keyframes,
                    ..
                } => (component, field, None, Some(keyframes)),
                _ => continue,
            };
            let entry =
                catalog.get(component, field).ok_or_else(|| {
                    err(
                        error_codes::COMPONENT_ERROR,
                        format!(
                            "`{component}`.`{field}` is not \
                             registered as animatable on this app"
                        ),
                    )
                })?;
            if let Some(to) = to {
                (entry.deserialize)(to, &registry)
                    .map_err(super::edit::map_catalog_err)?;
            }
            if let Some(keyframes) = keyframes {
                super::batch::decode_keyframes(
                    keyframes, entry, &registry,
                )?;
            }
        }
    }

    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, mut manager| {
            let mut forward: Vec<EditOp> = Vec::new();
            // Groups, because Clear's inverse is a whole re-insert
            // list. Rollback applies groups newest-first, ops within a
            // group in order.
            let mut inverse_groups: Vec<Vec<EditOp>> = Vec::new();

            if replace {
                let snapshot = batch::snapshot_all_clips(
                    world,
                    &mut manager,
                    tid,
                )?;
                let (_, _) = batch::apply_op(
                    world,
                    &mut manager,
                    tid,
                    &EditOp::Clear {},
                )?;
                forward.push(EditOp::Clear {});
                inverse_groups.push(snapshot);
            }

            // Imports may address tracks the (possibly just-cleared)
            // timeline doesn't have yet.
            if let Some((_, timeline)) =
                manager.registry_and_timeline_mut(&tid)
            {
                timeline.ensure_track_count(track_count);
            }

            let mut inserted = 0usize;
            for (index, op) in ops.iter().enumerate() {
                match batch::apply_op(world, &mut manager, tid, op) {
                    Ok((result, inverse)) => {
                        inserted += 1;
                        let mut f = op.clone();
                        batch::stamp_insert_id(&mut f, &result);
                        forward.push(f);
                        inverse_groups.push(alloc::vec![inverse]);
                    }
                    Err(mut e) => {
                        let mut rollback_failed = false;
                        for group in inverse_groups.iter().rev() {
                            for inv in group {
                                if batch::apply_op(
                                    world,
                                    &mut manager,
                                    tid,
                                    inv,
                                )
                                .is_err()
                                {
                                    rollback_failed = true;
                                }
                            }
                        }
                        let mut data =
                            json!({ "failed_action": index });
                        if let Some(prev) = e.data.take() {
                            data["error_data"] = prev;
                        }
                        if rollback_failed {
                            data["rollback_incomplete"] = true.into();
                        }
                        e.data = Some(data);
                        e.message = format!(
                            "import failed at action {index}: {}",
                            e.message
                        );
                        return Err(e);
                    }
                }
            }

            // Markers: replace swaps them wholesale, append merges.
            {
                let mut state = world.get_resource_or_insert_with(
                    MotionGfxEditState::default,
                );
                let markers = state.markers_mut(tid);
                if replace {
                    markers.clear();
                }
                markers.extend(p.document.markers.clone());
            }

            // Newest-first: undoing the import removes the imported
            // clips, then (for replace) the snapshot group restores
            // the original content.
            let inverse: Vec<EditOp> =
                inverse_groups.into_iter().rev().flatten().collect();

            let version = batch::finish_edit(
                world,
                &mut manager,
                tid,
                forward,
                inverse,
            );

            ok(json!({ "version": version, "inserted": inserted }))
        },
    )
}


#[derive(Deserialize)]
struct MotionGfxMarkerSetParams {
    id: u64,
    name: String,
    time: f32,
    #[serde(default)]
    track: Option<usize>,
}

/// `motiongfx.marker_set` - create or move a named time anchor.
pub fn marker_set(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxMarkerSetParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    // Default the track to wherever the playhead currently is.
    let track = match p.track {
        Some(track) => track,
        None => world
            .resource_scope::<MotionGfxManager, Option<usize>>(
                |_, manager| {
                    manager.get_timeline(&tid).map(|t| t.curr_index())
                },
            )
            .ok_or_else(|| {
                err(
                    error_codes::RESOURCE_ERROR,
                    format!("No timeline with id {}", p.id),
                )
            })?,
    };

    let mut state = world
        .get_resource_or_insert_with(MotionGfxEditState::default);
    state.markers_mut(tid).insert(
        p.name.clone(),
        Marker {
            track,
            time: p.time,
        },
    );
    let version = state.bump(tid);
    state.push_event(
        tid,
        json!({
            "kind": "marker_set",
            "name": p.name,
            "track": track,
            "time": p.time,
            "version": version,
        }),
    );
    ok(json!({ "version": version }))
}

#[derive(Deserialize)]
struct MotionGfxMarkerRemoveParams {
    id: u64,
    name: String,
}

/// `motiongfx.marker_remove` - drop a marker.
pub fn marker_remove(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxMarkerRemoveParams = parse(params)?;
    let tid = TimelineId::from_raw(p.id);

    let mut state = world
        .get_resource_or_insert_with(MotionGfxEditState::default);
    if state.markers_mut(tid).remove(&p.name).is_none() {
        return Err(err(
            error_codes::RESOURCE_ERROR,
            format!("no marker `{}` on timeline {}", p.name, p.id),
        ));
    }
    let version = state.bump(tid);
    state.push_event(
        tid,
        json!({
            "kind": "marker_removed",
            "name": p.name,
            "version": version,
        }),
    );
    ok(json!({ "removed": true, "version": version }))
}

#[derive(Deserialize)]
struct MotionGfxMarkerListParams {
    id: u64,
}

/// `motiongfx.marker_list` - all markers of a timeline.
pub fn marker_list(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxMarkerListParams = parse(params)?;
    let tid = TimelineId::from_raw(p.id);

    let markers = world
        .get_resource::<MotionGfxEditState>()
        .and_then(|s| s.markers(&tid).cloned())
        .unwrap_or_default();
    ok(markers)
}
