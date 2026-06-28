extern crate alloc;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;

use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_ecs::system::In;
use bevy_remote::{BrpError, BrpResult, error_codes};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use motiongfx::action::{ActionId, Ease, EaseFn};
use motiongfx::remote::RemoteEditError;
use motiongfx::timeline::Timeline;

use super::catalog::MotionGfxCatalog;
use super::edit::{
    self, EaseParam, ease_to_json, err, invalid, map_catalog_err,
    map_edit_err, parse, require_manager, resolve_ease,
};
use super::state::{ClipMeta, JournalEntry, MotionGfxEditState};
use crate::manager::{MotionGfxManager, TimelineId};
use crate::world::BevyWorld;

/// One timeline edit, shared by `timeline_batch` and the journal.
/// Same shape as the single-op params, minus the timeline `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    Insert {
        track: usize,
        entity: u64,
        component: String,
        field: String,
        /// Asset subject: the handle-bearing component on `entity`.
        /// `component` is then the asset's type path.
        /// `None` = component subject.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_of: Option<String>,
        /// Absolute target. Omit when `to_relative` is given.
        /// The journal always stores the resolved absolute
        /// (a replayed relative would double-apply).
        #[serde(default)]
        to: Value,
        /// Relative target: animate to `base + to_relative`, where
        /// `base` is the last same-key clip's baked end, or the live
        /// value. Numeric component fields only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_relative: Option<f64>,
        duration: f32,
        #[serde(default)]
        start_at: Option<f32>,
        #[serde(default)]
        ease: Option<Value>,
        /// Optional display label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Optional display colour `[r, g, b]` (0..=255).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        /// `false` inserts muted. Absent/`true` = enabled.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        /// Journal-only: schedule at exactly `start_at` instead of
        /// appending, to restore a clip mid-sequence.
        #[serde(skip)]
        exact: bool,
        /// Journal-only: the action id this restores. Remapped to the
        /// new id after applying.
        #[serde(skip)]
        restore_id: Option<u64>,
    },
    /// Multi-keyframe sibling of `Insert`: one clip whose value
    /// follows `keyframes` (each `t` normalized within the clip).
    InsertKeyframes {
        track: usize,
        entity: u64,
        component: String,
        field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        asset_of: Option<String>,
        keyframes: Vec<KeyframeDoc>,
        duration: f32,
        #[serde(default)]
        start_at: Option<f32>,
        /// Clip-level ease = whole-clip time warp.
        #[serde(default)]
        ease: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        color: Option<[u8; 3]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(skip)]
        exact: bool,
        #[serde(skip)]
        restore_id: Option<u64>,
    },
    Remove {
        track: usize,
        action_id: u64,
    },
    Move {
        track: usize,
        action_id: u64,
        start_at: f32,
        #[serde(default)]
        duration: Option<f32>,
    },
    Update {
        action_id: u64,
        #[serde(default)]
        to: Option<Value>,
        /// Relative retarget: current baked end plus this delta.
        /// Numeric only, exclusive with `to`/`keyframes`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to_relative: Option<f64>,
        #[serde(default)]
        ease: Option<Value>,
        /// Replace a keyframed action's points. Keyframed actions
        /// only, exclusive with `to`.
        #[serde(default)]
        keyframes: Option<Vec<KeyframeDoc>>,
        /// Set or clear (`null`) the label. Absent = unchanged.
        #[serde(
            default,
            deserialize_with = "double_option",
            skip_serializing_if = "Option::is_none"
        )]
        label: Option<Option<String>>,
        /// Set (`[r,g,b]`) or clear (`null`) the display colour.
        /// Absent = unchanged.
        #[serde(
            default,
            deserialize_with = "double_option",
            skip_serializing_if = "Option::is_none"
        )]
        color: Option<Option<[u8; 3]>>,
        /// Mute (`false`) / unmute (`true`). Absent = unchanged.
        /// A muted clip holds its start value and is skipped by
        /// followers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
    },
    Clear {},
    /// Journal-only placeholder for an inverse that can't be applied
    /// (e.g. field not in the catalog). Fails with `reason`.
    Unrestorable {
        reason: String,
    },
}

/// One keyframe: a normalized time, a JSON value, and an optional
/// segment ease.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyframeDoc {
    pub t: f32,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ease: Option<Value>,
    /// Step: hold the previous value, snap at `t`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<bool>,
}

/// `smooth: true`: give each non-hold segment a cubic-bezier ease
/// whose tangents follow a Catmull-Rom spline, so the value glides
/// through every keyframe. Pure authoring transform. The clip stays
/// per-point editable.
pub(crate) fn smooth_keyframes(
    docs: &mut [KeyframeDoc],
) -> Result<(), BrpError> {
    if docs.len() < 2 {
        return Ok(());
    }
    docs.sort_by(|a, b| a.t.total_cmp(&b.t));
    let vals: Vec<f64> = docs
        .iter()
        .map(|d| {
            d.value.as_f64().ok_or_else(|| {
                invalid("`smooth` needs numeric keyframe values")
            })
        })
        .collect::<Result<_, _>>()?;
    let ts: Vec<f64> = docs.iter().map(|d| f64::from(d.t)).collect();
    let n = docs.len();
    let eps = 1e-6;

    // Finite-difference tangents (non-uniform spacing, one-sided at
    // the ends).
    let mut tangents = alloc::vec![0.0f64; n];
    for (i, tangent) in tangents.iter_mut().enumerate() {
        *tangent = if i == 0 {
            (vals[1] - vals[0]) / (ts[1] - ts[0]).max(eps)
        } else if i == n - 1 {
            (vals[n - 1] - vals[n - 2])
                / (ts[n - 1] - ts[n - 2]).max(eps)
        } else {
            (vals[i + 1] - vals[i - 1])
                / (ts[i + 1] - ts[i - 1]).max(eps)
        };
    }

    for i in 0..n - 1 {
        // Step segments stay steps. Flat segments stay flat.
        if docs[i + 1].hold == Some(true) {
            continue;
        }
        let dv = vals[i + 1] - vals[i];
        let dt = (ts[i + 1] - ts[i]).max(eps);
        if dv.abs() < 1e-9 {
            docs[i + 1].ease = None;
            continue;
        }
        let y1 = (tangents[i] * dt / dv) / 3.0;
        let y2 = 1.0 - (tangents[i + 1] * dt / dv) / 3.0;
        docs[i + 1].ease = Some(serde_json::json!({
            "cubic_bezier": [
                1.0_f32 / 3.0,
                y1 as f32,
                2.0_f32 / 3.0,
                y2 as f32,
            ],
        }));
    }
    Ok(())
}

/// JSON-level `smooth` for batch ops: strip `"smooth": true` and
/// rewrite `keyframes` in place before the op deserializes (the
/// journal stores the computed result).
pub(crate) fn apply_smooth_in_op(
    raw: &mut Value,
) -> Result<(), BrpError> {
    let Some(obj) = raw.as_object_mut() else {
        return Ok(());
    };
    let Some(smooth) = obj.remove("smooth") else {
        return Ok(());
    };
    match smooth {
        Value::Bool(false) => return Ok(()),
        Value::Bool(true) => {}
        _ => return Err(invalid("`smooth` must be a boolean")),
    }
    let Some(kfs) = obj.get_mut("keyframes") else {
        return Err(invalid("`smooth` requires `keyframes`"));
    };
    let mut docs: Vec<KeyframeDoc> =
        serde_json::from_value(kfs.clone())
            .map_err(|e| invalid(e.to_string()))?;
    smooth_keyframes(&mut docs)?;
    *kfs = serde_json::to_value(docs).map_err(|e| {
        err(error_codes::INTERNAL_ERROR, e.to_string())
    })?;
    Ok(())
}

impl EditOp {
    /// A one-word label for this op, for an undo-history panel.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Insert { .. } => "insert",
            Self::InsertKeyframes { .. } => "insert keyframes",
            Self::Remove { .. } => "remove",
            Self::Move { .. } => "move",
            Self::Update { .. } => "update",
            Self::Clear {} => "clear",
            Self::Unrestorable { .. } => "-",
        }
    }

    /// Rewrite references to action id `old` to `new` (see
    /// [`super::state::EditJournal::remap_action_id`]).
    pub(crate) fn remap_action_id(&mut self, old: u64, new: u64) {
        match self {
            Self::Remove { action_id, .. }
            | Self::Move { action_id, .. }
            | Self::Update { action_id, .. } => {
                if *action_id == old {
                    *action_id = new;
                }
            }
            Self::Insert { restore_id, .. }
            | Self::InsertKeyframes { restore_id, .. } => {
                if *restore_id == Some(old) {
                    *restore_id = Some(new);
                }
            }
            Self::Clear {} | Self::Unrestorable { .. } => {}
        }
    }
}


fn not_found(id: u64) -> BrpError {
    err(
        error_codes::RESOURCE_ERROR,
        format!("No timeline with id {id}"),
    )
}

/// Locate a clip's `(start, duration)` on a track.
fn find_clip(
    timeline: &Timeline<BevyWorld>,
    track: usize,
    id: ActionId,
) -> Option<(f32, f32)> {
    let track = timeline.tracks().get(track)?;
    track
        .sequences_spans()
        .iter()
        .flat_map(|(_, span)| track.clips(*span))
        .find(|clip| clip.id == id)
        .map(|clip| (clip.start, clip.duration))
}

/// Re-bake `tid`'s segments. Safe to repeat, needed before reading
/// values from a freshly edited timeline.
fn bake(
    world: &World,
    manager: &mut MotionGfxManager,
    tid: &TimelineId,
) {
    if let Some((registry, timeline)) =
        manager.registry_and_timeline_mut(tid)
    {
        timeline.bake_actions(registry, BevyWorld::from_ref(world));
    }
}

/// Snapshot a clip as the [`EditOp::Insert`] that restores it: timing
/// from the track, value from the baked segment, ease from the action
/// world. [`EditOp::Unrestorable`] when the catalog can't describe it.
fn snapshot_clip(
    world: &World,
    manager: &mut MotionGfxManager,
    tid: TimelineId,
    track: usize,
    id: ActionId,
) -> Result<EditOp, BrpError> {
    // The segment must exist to read the value. Bake covers clips
    // inserted earlier in the same request.
    bake(world, manager, &tid);

    let timeline = manager
        .get_timeline(&tid)
        .ok_or_else(|| not_found(tid.raw()))?;
    let (start, duration) = find_clip(timeline, track, id)
        .ok_or_else(|| map_edit_err(RemoteEditError::NotFound))?;

    let aw = timeline.action_world();
    let key = aw
        .action_key(id)
        .ok_or_else(|| map_edit_err(RemoteEditError::NotFound))?;

    let entity = (key.subject_id().type_id()
        == core::any::TypeId::of::<Entity>())
    .then(|| aw.get_id::<Entity>(&key.subject_id().uid()).copied())
    .flatten();

    let catalog = world.resource::<MotionGfxCatalog>();
    let snapshot = catalog.get_by_field(key.field()).and_then(
        |((component, field), entry)| {
            let type_registry =
                world.resource::<AppTypeRegistry>().read();
            // Keyframed clips snapshot as their point list. Constant
            // ones as their baked end value.
            let keyframes =
                keyframe_docs(aw, id, entry, &type_registry);
            let to = match &keyframes {
                Some(_) => Value::Null,
                None => (entry.serialize)(aw, id, &type_registry)
                    .map(|(_, end)| end)?,
            };
            Some((component.clone(), field.clone(), to, keyframes))
        },
    );

    // Asset-subject clips resolve no `Entity` from the key, but their
    // insert-time addressing (handle entity + `asset_of`) restores them.
    let asset_ref = world
        .get_resource::<MotionGfxEditState>()
        .and_then(|s| s.asset_ref(&tid, id.to_bits()).cloned());
    let is_resource = key.subject_id().type_id()
        == core::any::TypeId::of::<crate::world::ResourceSubject>();
    let (entity_bits, asset_of) = match (entity, asset_ref) {
        _ if is_resource => (0, None),
        (Some(entity), _) => (entity.to_bits(), None),
        (None, Some((bits, path))) => (bits, Some(path)),
        (None, None) => {
            return Ok(EditOp::Unrestorable {
                reason: format!(
                    "clip {} cannot be restored: its subject is \
                     neither an entity nor a recorded asset \
                     reference",
                    id.to_bits()
                ),
            });
        }
    };
    let Some((component, field, to, keyframes)) = snapshot else {
        return Ok(EditOp::Unrestorable {
            reason: format!(
                "clip {} cannot be restored: its field is not \
                 registered as animatable",
                id.to_bits()
            ),
        });
    };

    // Restore the clip's label/colour and mute state along with it.
    let meta = world
        .get_resource::<MotionGfxEditState>()
        .and_then(|s| s.clip_meta(&tid, id.to_bits()).cloned())
        .unwrap_or_default();
    // `Some(false)` only when muted. Enabled is the wire default.
    let enabled = aw.is_disabled(id).then_some(false);

    Ok(match keyframes {
        Some(keyframes) => EditOp::InsertKeyframes {
            track,
            entity: entity_bits,
            component,
            field,
            asset_of,
            keyframes,
            duration,
            start_at: Some(start),
            ease: aw.get_ease(id).map(ease_to_json),
            label: meta.label,
            color: meta.color,
            enabled,
            exact: true,
            restore_id: Some(id.to_bits()),
        },
        None => EditOp::Insert {
            track,
            entity: entity_bits,
            component,
            field,
            asset_of,
            to,
            to_relative: None,
            duration,
            start_at: Some(start),
            ease: aw.get_ease(id).map(ease_to_json),
            label: meta.label,
            color: meta.color,
            enabled,
            exact: true,
            restore_id: Some(id.to_bits()),
        },
    })
}

/// Resolve an optional JSON ease (name or `{"cubic_bezier": ...}`).
fn parse_op_ease(
    ease: &Option<Value>,
) -> Result<Option<Ease>, BrpError> {
    ease.as_ref()
        .map(|v| {
            let p: EaseParam = serde_json::from_value(v.clone())
                .map_err(|e| invalid(e.to_string()))?;
            resolve_ease(&p)
        })
        .transpose()
}

/// Decode [`KeyframeDoc`]s into [`RemoteKeyframe`]s via the catalog's
/// per-field `deserialize` (each value crosses as a
/// [`RemoteTarget`](motiongfx::remote::RemoteTarget)).
pub(crate) fn decode_keyframes(
    keyframes: &[KeyframeDoc],
    entry: &super::catalog::CatalogEntry,
    registry: &bevy_reflect::TypeRegistry,
) -> Result<Vec<motiongfx::remote::RemoteKeyframe>, BrpError> {
    if keyframes.is_empty() {
        return Err(invalid("`keyframes` must not be empty"));
    }
    let mut out = Vec::with_capacity(keyframes.len());
    for (i, kf) in keyframes.iter().enumerate() {
        if !kf.t.is_finite() || !(0.0..=1.0).contains(&kf.t) {
            return Err(invalid(format!(
                "keyframe {i}: `t` must lie in [0, 1] (got {})",
                kf.t
            )));
        }
        let value = (entry.deserialize)(&kf.value, registry)
            .map_err(map_catalog_err)?;
        out.push(motiongfx::remote::RemoteKeyframe {
            t: kf.t,
            value,
            ease: parse_op_ease(&kf.ease)?,
            hold: kf.hold == Some(true),
        });
    }
    Ok(out)
}

/// Snapshot a keyframed action's points as [`KeyframeDoc`]s, `None`
/// for constant actions.
pub(crate) fn keyframe_docs(
    aw: &motiongfx::action::ActionWorld,
    id: ActionId,
    entry: &super::catalog::CatalogEntry,
    registry: &bevy_reflect::TypeRegistry,
) -> Option<Vec<KeyframeDoc>> {
    let points = (entry.serialize_keyframes)(aw, id, registry)?;
    Some(
        points
            .into_iter()
            .map(|(t, value, ease, hold)| KeyframeDoc {
                t,
                value,
                ease: ease.map(ease_to_json),
                hold: hold.then_some(true),
            })
            .collect(),
    )
}

/// Snapshot every clip as the [`EditOp::Insert`] list that rebuilds
/// the timeline: the inverse of `Clear` and the basis of
/// `timeline_export`. In order, so clips restore front to back.
pub(crate) fn snapshot_all_clips(
    world: &World,
    manager: &mut MotionGfxManager,
    tid: TimelineId,
) -> Result<Vec<EditOp>, BrpError> {
    let clip_ids: Vec<(usize, ActionId)> = {
        let timeline = manager
            .get_timeline(&tid)
            .ok_or_else(|| not_found(tid.raw()))?;
        timeline
            .tracks()
            .iter()
            .enumerate()
            .flat_map(|(t, track)| {
                track
                    .sequences_spans()
                    .iter()
                    .flat_map(|(_, span)| track.clips(*span))
                    .map(move |clip| (t, clip.id))
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    let mut ops = Vec::with_capacity(clip_ids.len());
    for (track, id) in clip_ids {
        ops.push(snapshot_clip(world, manager, tid, track, id)?);
    }
    Ok(ops)
}

use alloc::string::ToString;

/// Maintain insert-time side tables: clip metadata and, for asset
/// clips, the addressing that makes them restorable.
fn record_insert_tables(
    world: &mut World,
    tid: TimelineId,
    action_id: u64,
    label: &Option<String>,
    color: &Option<[u8; 3]>,
    asset_of: &Option<String>,
    entity_bits: u64,
) {
    if label.is_none() && color.is_none() && asset_of.is_none() {
        return;
    }
    let mut state = world
        .get_resource_or_insert_with(MotionGfxEditState::default);
    if label.is_some() || color.is_some() {
        state.set_clip_meta(
            tid,
            action_id,
            ClipMeta {
                label: label.clone(),
                color: *color,
            },
        );
    }
    if let Some(path) = asset_of {
        state.set_asset_ref(
            tid,
            action_id,
            entity_bits,
            path.clone(),
        );
    }
}

/// Read a live component field as a number by reflection: the
/// `to_relative` fallback when no same-key clip exists.
fn remote_numeric_field(
    world: &World,
    entity: Entity,
    component: &str,
    field: &str,
) -> Result<f64, BrpError> {
    use bevy_reflect::ReflectRef;

    let not_numeric = || {
        invalid(format!(
            "`to_relative` needs a numeric field with a readable \
             live value (`{component}`.`{field}`)"
        ))
    };
    let registry_arc = world
        .get_resource::<AppTypeRegistry>()
        .ok_or_else(not_numeric)?
        .clone();
    let registry = registry_arc.read();
    let reflect_component = registry
        .get_with_type_path(component)
        .and_then(|r| r.data::<bevy_ecs::reflect::ReflectComponent>())
        .ok_or_else(not_numeric)?;
    let entity_ref = world.get_entity(entity).map_err(|_| {
        err(
            error_codes::ENTITY_NOT_FOUND,
            format!("entity {} does not exist", entity.to_bits()),
        )
    })?;
    let reflected = reflect_component
        .reflect(entity_ref)
        .ok_or_else(not_numeric)?;
    let value = bevy_reflect::GetPath::reflect_path(reflected, field)
        .map_err(|_| not_numeric())?;
    match value.reflect_ref() {
        ReflectRef::Opaque(v) => v
            .try_downcast_ref::<f32>()
            .map(|v| f64::from(*v))
            .or_else(|| v.try_downcast_ref::<f64>().copied())
            .ok_or_else(not_numeric),
        _ => Err(not_numeric()),
    }
}

/// Base value a `to_relative` insert resolves against: the last
/// same-key clip's baked end on `track`, or the live value if none.
#[allow(clippy::too_many_arguments)]
fn relative_base(
    world: &World,
    manager: &mut MotionGfxManager,
    tid: TimelineId,
    track: usize,
    entity_bits: u64,
    component: &str,
    field: &str,
) -> Result<f64, BrpError> {
    use core::any::TypeId;

    // Chained values live in baked segments.
    bake(world, manager, &tid);

    let timeline = manager
        .get_timeline(&tid)
        .ok_or_else(|| not_found(tid.raw()))?;
    let catalog = world.resource::<MotionGfxCatalog>();
    let registry_arc = world.resource::<AppTypeRegistry>().clone();
    let registry = registry_arc.read();
    let aw = timeline.action_world();

    if let Some(track_ref) = timeline.tracks().get(track) {
        for (key, span) in track_ref.sequences_spans() {
            let entity = (key.subject_id().type_id()
                == TypeId::of::<Entity>())
            .then(|| {
                aw.get_id::<Entity>(&key.subject_id().uid()).copied()
            })
            .flatten();
            if entity.map(Entity::to_bits) != Some(entity_bits) {
                continue;
            }
            let Some(((c, f), entry)) =
                catalog.get_by_field(key.field())
            else {
                continue;
            };
            if c != component || f != field {
                continue;
            }
            if let Some(clip) = track_ref.clips(*span).last()
                && let Some((_, end)) =
                    (entry.serialize)(aw, clip.id, &registry)
            {
                return end.as_f64().ok_or_else(|| {
                    invalid("`to_relative` needs a numeric field")
                });
            }
        }
    }

    remote_numeric_field(
        world,
        Entity::from_bits(entity_bits),
        component,
        field,
    )
}

/// Deserialize helper distinguishing absent (`None`) from explicit
/// `null` (`Some(None)`), for update patches. Plain `Option<T>` folds
/// both into `None`.
pub(crate) fn double_option<'de, T, D>(
    de: D,
) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}


/// Apply one [`EditOp`] to `tid`, returning its JSON result and
/// inverse. Does not re-bake, journal, or bump the version.
/// [`finish_edit`] does that once the request succeeds.
pub(crate) fn apply_op(
    world: &mut World,
    manager: &mut MotionGfxManager,
    tid: TimelineId,
    op: &EditOp,
) -> Result<(Value, EditOp), BrpError> {
    match op {
        EditOp::Insert {
            track,
            entity,
            component,
            field,
            asset_of,
            to,
            to_relative,
            duration,
            start_at,
            ease,
            label,
            color,
            enabled,
            exact,
            restore_id: _,
        } => {
            let ease = parse_op_ease(ease)?;

            // Relative targets resolve to an absolute first, so the
            // journal/export never replay (and double-apply) a delta.
            let resolved_to: Option<Value> = match to_relative {
                Some(delta) => {
                    if !to.is_null() {
                        return Err(invalid(
                            "`to` and `to_relative` are mutually \
                             exclusive",
                        ));
                    }
                    let is_component = world
                        .resource::<MotionGfxCatalog>()
                        .get(component, field)
                        .is_some_and(|e| {
                            matches!(
                                e.subject,
                                super::catalog::SubjectKind::Component
                            )
                        });
                    if asset_of.is_some() || !is_component {
                        return Err(invalid(
                            "`to_relative` supports component \
                             subjects only",
                        ));
                    }
                    let base = relative_base(
                        world, manager, tid, *track, *entity,
                        component, field,
                    )?;
                    Some(Value::from(base + delta))
                }
                None => None,
            };
            let to = resolved_to.as_ref().unwrap_or(to);

            // Decode the JSON target through the catalog (the only
            // place that knows the concrete `T`).
            let type_registry =
                world.resource::<AppTypeRegistry>().clone();
            let (key, target, subject_kind) = {
                let registry = type_registry.read();
                let catalog = world.resource::<MotionGfxCatalog>();
                let entry =
                    catalog.get(component, field).ok_or_else(|| {
                        err(
                            error_codes::COMPONENT_ERROR,
                            format!(
                                "`{component}`.`{field}` is not \
                                 registered as animatable (animate it \
                                 via act() or call \
                                 App::register_animatable)"
                            ),
                        )
                    })?;
                let target = (entry.deserialize)(to, &registry)
                    .map_err(map_catalog_err)?;
                (entry.key(), target, entry.subject)
            };
            let is_resource = matches!(
                subject_kind,
                super::catalog::SubjectKind::Resource
            );
            if is_resource && asset_of.is_some() {
                return Err(invalid(
                    "resource fields take no `asset_of`",
                ));
            }

            // Resource clips carry no meaningful entity (the wire
            // default 0 is not even valid `Entity` bits).
            let entity = if is_resource {
                None
            } else {
                Some(Entity::try_from_bits(*entity).ok_or_else(
                    || {
                        err(
                            error_codes::ENTITY_NOT_FOUND,
                            "missing or invalid `entity` bits",
                        )
                    },
                )?)
            };

            // Asset addressing: the real subject is the asset behind
            // the entity's handle component, resolved while `world`
            // is still readable.
            #[cfg(feature = "asset")]
            let asset_subject: Option<
                bevy_asset::UntypedAssetId,
            > = match (asset_of.as_deref(), entity) {
                (Some(component_path), Some(entity)) => {
                    Some(edit::resolve_asset_of(
                        world,
                        entity,
                        component_path,
                    )?)
                }
                _ => None,
            };
            #[cfg(not(feature = "asset"))]
            if asset_of.is_some() {
                return Err(err(
                    error_codes::COMPONENT_ERROR,
                    "asset subjects need the `asset` feature of \
                     bevy_motiongfx",
                ));
            }

            let (registry, timeline) = manager
                .registry_and_timeline_mut(&tid)
                .ok_or_else(|| not_found(tid.raw()))?;

            let resource_subject = crate::world::ResourceSubject;
            #[cfg(feature = "asset")]
            let subject: &dyn Any = if is_resource {
                &resource_subject
            } else {
                match (&asset_subject, &entity) {
                    (Some(id), _) => id,
                    (None, Some(entity)) => entity,
                    (None, None) => unreachable!(
                        "non-resource subjects resolved an entity"
                    ),
                }
            };
            #[cfg(not(feature = "asset"))]
            let subject: &dyn Any = if is_resource {
                &resource_subject
            } else {
                entity.as_ref().expect("resolved above")
            };

            let start_at = start_at.unwrap_or(0.0);
            let action_id = timeline
                .insert_constant_action(
                    *track,
                    &registry.remote,
                    key,
                    subject,
                    target,
                    *duration,
                    start_at,
                    ease,
                )
                .map_err(map_edit_err)?;

            // Journal restores need the clip back at its *exact* spot,
            // not appended after same-key siblings.
            if *exact
                && let Err(e) = timeline.reschedule_action(
                    *track, action_id, start_at, None,
                )
            {
                // Don't leave the misplaced clip behind.
                let _ = timeline.remove_action(*track, action_id);
                return Err(map_edit_err(e));
            }

            if *enabled == Some(false) {
                let _ = timeline.set_action_enabled(action_id, false);
            }

            record_insert_tables(
                world,
                tid,
                action_id.to_bits(),
                label,
                color,
                asset_of,
                entity.map(Entity::to_bits).unwrap_or_default(),
            );

            let mut result = serde_json::json!({
                "action_id": action_id.to_bits(),
            });
            if let Some(resolved) = resolved_to {
                // Echoed to the client AND folded into the journaled
                // forward op by `stamp_insert_id`.
                result["resolved_to"] = resolved;
            }

            Ok((
                result,
                EditOp::Remove {
                    track: *track,
                    action_id: action_id.to_bits(),
                },
            ))
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
            exact,
            restore_id: _,
        } => {
            let ease = parse_op_ease(ease)?;

            // Decode every point through the catalog (the only place
            // that knows the concrete `T`). The clip-level params
            // resolve exactly like a constant insert.
            let type_registry =
                world.resource::<AppTypeRegistry>().clone();
            let (key, remote_keyframes, subject_kind) = {
                let registry = type_registry.read();
                let catalog = world.resource::<MotionGfxCatalog>();
                let entry =
                    catalog.get(component, field).ok_or_else(|| {
                        err(
                            error_codes::COMPONENT_ERROR,
                            format!(
                                "`{component}`.`{field}` is not \
                                 registered as animatable (animate it \
                                 via act() or call \
                                 App::register_animatable)"
                            ),
                        )
                    })?;
                let remote_keyframes =
                    decode_keyframes(keyframes, entry, &registry)?;
                (entry.key(), remote_keyframes, entry.subject)
            };
            let is_resource = matches!(
                subject_kind,
                super::catalog::SubjectKind::Resource
            );
            if is_resource && asset_of.is_some() {
                return Err(invalid(
                    "resource fields take no `asset_of`",
                ));
            }

            let entity = if is_resource {
                None
            } else {
                Some(Entity::try_from_bits(*entity).ok_or_else(
                    || {
                        err(
                            error_codes::ENTITY_NOT_FOUND,
                            "missing or invalid `entity` bits",
                        )
                    },
                )?)
            };

            #[cfg(feature = "asset")]
            let asset_subject: Option<
                bevy_asset::UntypedAssetId,
            > = match (asset_of.as_deref(), entity) {
                (Some(component_path), Some(entity)) => {
                    Some(edit::resolve_asset_of(
                        world,
                        entity,
                        component_path,
                    )?)
                }
                _ => None,
            };
            #[cfg(not(feature = "asset"))]
            if asset_of.is_some() {
                return Err(err(
                    error_codes::COMPONENT_ERROR,
                    "asset subjects need the `asset` feature of \
                     bevy_motiongfx",
                ));
            }

            let (registry, timeline) = manager
                .registry_and_timeline_mut(&tid)
                .ok_or_else(|| not_found(tid.raw()))?;

            let resource_subject = crate::world::ResourceSubject;
            #[cfg(feature = "asset")]
            let subject: &dyn Any = if is_resource {
                &resource_subject
            } else {
                match (&asset_subject, &entity) {
                    (Some(id), _) => id,
                    (None, Some(entity)) => entity,
                    (None, None) => unreachable!(
                        "non-resource subjects resolved an entity"
                    ),
                }
            };
            #[cfg(not(feature = "asset"))]
            let subject: &dyn Any = if is_resource {
                &resource_subject
            } else {
                entity.as_ref().expect("resolved above")
            };

            let start_at = start_at.unwrap_or(0.0);
            let action_id = timeline
                .insert_keyframes_action(
                    *track,
                    &registry.remote,
                    key,
                    subject,
                    remote_keyframes,
                    *duration,
                    start_at,
                    ease,
                )
                .map_err(map_edit_err)?;

            if *exact
                && let Err(e) = timeline.reschedule_action(
                    *track, action_id, start_at, None,
                )
            {
                let _ = timeline.remove_action(*track, action_id);
                return Err(map_edit_err(e));
            }

            if *enabled == Some(false) {
                let _ = timeline.set_action_enabled(action_id, false);
            }

            record_insert_tables(
                world,
                tid,
                action_id.to_bits(),
                label,
                color,
                asset_of,
                entity.map(Entity::to_bits).unwrap_or_default(),
            );

            Ok((
                serde_json::json!({
                    "action_id": action_id.to_bits(),
                }),
                EditOp::Remove {
                    track: *track,
                    action_id: action_id.to_bits(),
                },
            ))
        }

        EditOp::Remove { track, action_id } => {
            let aid = ActionId::from_bits(*action_id);
            let inverse =
                snapshot_clip(world, manager, tid, *track, aid)?;

            let (_, timeline) = manager
                .registry_and_timeline_mut(&tid)
                .ok_or_else(|| not_found(tid.raw()))?;
            timeline
                .remove_action(*track, aid)
                .map_err(map_edit_err)?;

            // The metadata + asset addressing die with the clip (the
            // inverse insert snapshot above carries them for undo).
            if let Some(mut state) =
                world.get_resource_mut::<MotionGfxEditState>()
            {
                state.remove_clip_meta(tid, *action_id);
                state.remove_asset_ref(tid, *action_id);
            }

            Ok((serde_json::json!({ "removed": true }), inverse))
        }

        EditOp::Move {
            track,
            action_id,
            start_at,
            duration,
        } => {
            let aid = ActionId::from_bits(*action_id);
            let (_, timeline) = manager
                .registry_and_timeline_mut(&tid)
                .ok_or_else(|| not_found(tid.raw()))?;

            let (old_start, old_duration) = find_clip(
                timeline, *track, aid,
            )
            .ok_or_else(|| map_edit_err(RemoteEditError::NotFound))?;

            timeline
                .reschedule_action(*track, aid, *start_at, *duration)
                .map_err(map_edit_err)?;

            Ok((
                serde_json::json!({ "action_id": *action_id }),
                EditOp::Move {
                    track: *track,
                    action_id: *action_id,
                    start_at: old_start,
                    duration: Some(old_duration),
                },
            ))
        }

        EditOp::Update {
            action_id,
            to,
            to_relative,
            ease,
            keyframes,
            label,
            color,
            enabled,
        } => {
            if to.is_none()
                && to_relative.is_none()
                && ease.is_none()
                && keyframes.is_none()
                && label.is_none()
                && color.is_none()
                && enabled.is_none()
            {
                return Err(invalid(
                    "provide at least one of `to` / `to_relative` / \
                     `ease` / `keyframes` / `label` / `color` / \
                     `enabled`",
                ));
            }
            if to.is_some() && to_relative.is_some() {
                return Err(invalid(
                    "`to` and `to_relative` are mutually exclusive",
                ));
            }
            if (to.is_some() || to_relative.is_some())
                && keyframes.is_some()
            {
                return Err(invalid(
                    "`to` and `keyframes` are mutually exclusive",
                ));
            }
            let aid = ActionId::from_bits(*action_id);
            let new_ease = parse_op_ease(ease)?;

            let old_keyframes = if let Some(new_points) = keyframes {
                let type_registry =
                    world.resource::<AppTypeRegistry>().clone();
                let registry = type_registry.read();
                let catalog = world.resource::<MotionGfxCatalog>();

                let timeline = manager
                    .get_timeline(&tid)
                    .ok_or_else(|| not_found(tid.raw()))?;
                let aw = timeline.action_world();
                let key = aw.action_key(aid).ok_or_else(|| {
                    map_edit_err(RemoteEditError::NotFound)
                })?;
                let (_, entry) = catalog
                    .get_by_field(key.field())
                    .ok_or_else(|| {
                        err(
                            error_codes::COMPONENT_ERROR,
                            "the action's field is not registered \
                             as animatable",
                        )
                    })?;
                // The inverse - and the "is this keyframed?" check.
                let old = keyframe_docs(aw, aid, entry, &registry)
                    .ok_or_else(|| {
                        invalid(
                            "`keyframes` can only update an action \
                             that already has keyframes (use \
                             remove + insert_keyframes to convert \
                             a constant clip)",
                        )
                    })?;
                let remote_keyframes =
                    decode_keyframes(new_points, entry, &registry)?;

                let (registry, timeline) = manager
                    .registry_and_timeline_mut(&tid)
                    .ok_or_else(|| not_found(tid.raw()))?;
                timeline
                    .update_keyframes_action(
                        aid,
                        &registry.remote,
                        remote_keyframes,
                    )
                    .map_err(map_edit_err)?;
                Some(old)
            } else {
                None
            };

            // Snapshot the old value/ease for the inverse first.
            // (`to_relative` also needs it as the delta's base.)
            let old_to = if to.is_some() || to_relative.is_some() {
                bake(world, manager, &tid);
                let timeline = manager
                    .get_timeline(&tid)
                    .ok_or_else(|| not_found(tid.raw()))?;
                let aw = timeline.action_world();
                let key = aw.action_key(aid).ok_or_else(|| {
                    map_edit_err(RemoteEditError::NotFound)
                })?;
                let catalog = world.resource::<MotionGfxCatalog>();
                let (_, entry) = catalog
                    .get_by_field(key.field())
                    .ok_or_else(|| {
                        err(
                            error_codes::COMPONENT_ERROR,
                            "the action's field is not registered as \
                             animatable, so its value type is unknown \
                             (ease-only updates still work)",
                        )
                    })?;
                let type_registry =
                    world.resource::<AppTypeRegistry>().read();
                // A keyframed action's value is its point list, not a
                // single `to`. Flattening it would lose data.
                if keyframe_docs(aw, aid, entry, &type_registry)
                    .is_some()
                {
                    return Err(invalid(
                        "this action has keyframes; update them via \
                         `keyframes`, not `to`",
                    ));
                }
                Some(
                    (entry.serialize)(aw, aid, &type_registry)
                        .map(|(_, end)| end)
                        .ok_or_else(|| {
                            err(
                                error_codes::INTERNAL_ERROR,
                                "could not snapshot the current value",
                            )
                        })?,
                )
            } else {
                None
            };
            let old_ease = ease.as_ref().map(|_| {
                manager
                    .get_timeline(&tid)
                    .and_then(|t| t.action_world().get_ease(aid))
                    .map(ease_to_json)
                    // Inverse of "had no ease" is clearing it.
                    .unwrap_or_else(|| Value::String("linear".into()))
            });

            // Resolve a relative retarget against the snapshotted
            // current end value (journal/result carry the absolute).
            let mut applied_resolved: Option<Value> = None;
            let new_to: Option<Value> = match (to, to_relative) {
                (Some(v), None) => Some(v.clone()),
                (None, Some(delta)) => {
                    let base = old_to
                        .as_ref()
                        .and_then(Value::as_f64)
                        .ok_or_else(|| {
                            invalid(
                                "`to_relative` needs a numeric field",
                            )
                        })?;
                    let resolved = Value::from(base + delta);
                    applied_resolved = Some(resolved.clone());
                    Some(resolved)
                }
                _ => None,
            };

            if let Some(to) = &new_to {
                let type_registry =
                    world.resource::<AppTypeRegistry>().clone();
                let target = {
                    let registry = type_registry.read();
                    let catalog =
                        world.resource::<MotionGfxCatalog>();
                    let timeline = manager
                        .get_timeline(&tid)
                        .ok_or_else(|| not_found(tid.raw()))?;
                    let key = timeline
                        .action_world()
                        .action_key(aid)
                        .ok_or_else(|| {
                            map_edit_err(RemoteEditError::NotFound)
                        })?;
                    let (_, entry) = catalog
                        .get_by_field(key.field())
                        .expect("checked above");
                    (entry.deserialize)(to, &registry)
                        .map_err(map_catalog_err)?
                };

                let (registry, timeline) = manager
                    .registry_and_timeline_mut(&tid)
                    .ok_or_else(|| not_found(tid.raw()))?;
                timeline
                    .update_action(aid, &registry.remote, target)
                    .map_err(map_edit_err)?;
            }

            if let Some(new_ease) = new_ease {
                // Explicit "linear" clears the ease.
                let linear =
                    Ease::Fn(motiongfx::ease::linear as EaseFn);
                let new_ease =
                    (new_ease != linear).then_some(new_ease);
                let (_, timeline) = manager
                    .registry_and_timeline_mut(&tid)
                    .ok_or_else(|| not_found(tid.raw()))?;
                timeline
                    .set_action_ease(aid, new_ease)
                    .map_err(map_edit_err)?;
            }

            let (old_label, old_color) = if label.is_some()
                || color.is_some()
            {
                // A meta-only update must still address a real clip.
                let timeline = manager
                    .get_timeline(&tid)
                    .ok_or_else(|| not_found(tid.raw()))?;
                if timeline.action_world().action_key(aid).is_none() {
                    return Err(map_edit_err(
                        RemoteEditError::NotFound,
                    ));
                }

                let mut state = world.get_resource_or_insert_with(
                    MotionGfxEditState::default,
                );
                let mut meta = state
                    .clip_meta(&tid, *action_id)
                    .cloned()
                    .unwrap_or_default();
                // Inverses mirror the request: only touched fields
                // appear, as set-or-null.
                let old_label =
                    label.as_ref().map(|_| meta.label.clone());
                let old_color = color.as_ref().map(|_| meta.color);
                if let Some(patch) = label {
                    meta.label = patch.clone();
                }
                if let Some(patch) = color {
                    meta.color = *patch;
                }
                state.set_clip_meta(tid, *action_id, meta);
                (old_label, old_color)
            } else {
                (None, None)
            };

            let old_enabled = if let Some(enabled) = enabled {
                let (_, timeline) = manager
                    .registry_and_timeline_mut(&tid)
                    .ok_or_else(|| not_found(tid.raw()))?;
                let old =
                    timeline.is_action_enabled(aid).ok_or_else(
                        || map_edit_err(RemoteEditError::NotFound),
                    )?;
                timeline
                    .set_action_enabled(aid, *enabled)
                    .map_err(map_edit_err)?;
                Some(old)
            } else {
                None
            };

            let mut result =
                serde_json::json!({ "action_id": *action_id });
            if let Some(resolved) = applied_resolved {
                result["resolved_to"] = resolved;
            }

            Ok((
                result,
                EditOp::Update {
                    action_id: *action_id,
                    to: old_to,
                    to_relative: None,
                    ease: old_ease,
                    keyframes: old_keyframes,
                    label: old_label,
                    color: old_color,
                    enabled: old_enabled,
                },
            ))
        }

        EditOp::Clear {} => {
            // An op-level inverse cannot express "re-insert N clips".
            // `timeline_clear` snapshots the whole timeline and
            // journals the re-insert list itself (see
            // [`snapshot_all_clips`]). The inverse here is only a
            // placeholder: redo discards it, undo uses the journal.
            let (_, timeline) = manager
                .registry_and_timeline_mut(&tid)
                .ok_or_else(|| not_found(tid.raw()))?;
            timeline.clear_actions();

            // Clip metadata dies with the clips (the journaled
            // snapshot's insert list carries it for undo).
            if let Some(mut state) =
                world.get_resource_mut::<MotionGfxEditState>()
            {
                state.clear_clip_meta(&tid);
                state.clear_asset_refs(&tid);
            }

            Ok((
                serde_json::json!({ "cleared": tid.raw() }),
                EditOp::Unrestorable {
                    reason: "clear is restored from its journaled \
                             snapshot, not an op inverse"
                        .to_string(),
                },
            ))
        }

        EditOp::Unrestorable { reason } => Err(err(
            error_codes::INTERNAL_ERROR,
            format!("cannot apply this edit: {reason}"),
        )),
    }
}

/// Stamp a journaled [`EditOp::Insert`] with the action id it created
/// so undo/redo can remap references. Also folds a resolved relative
/// target in, so replays apply the absolute, not the delta.
pub(crate) fn stamp_insert_id(op: &mut EditOp, result: &Value) {
    if let EditOp::Insert { restore_id, .. }
    | EditOp::InsertKeyframes { restore_id, .. } = op
    {
        *restore_id = result["action_id"].as_u64();
    }
    if let Some(resolved) = result.get("resolved_to") {
        match op {
            EditOp::Insert {
                to, to_relative, ..
            } => {
                *to = resolved.clone();
                *to_relative = None;
            }
            EditOp::Update {
                to, to_relative, ..
            } => {
                *to = Some(resolved.clone());
                *to_relative = None;
            }
            _ => {}
        }
    }
}

/// Bake once, journal the request as one entry, bump the version,
/// emit one `journal+watch` stream event describing the applied ops.
/// `inverse` must already be in reverse application order.
pub(crate) fn finish_edit(
    world: &mut World,
    manager: &mut MotionGfxManager,
    tid: TimelineId,
    forward: Vec<EditOp>,
    inverse: Vec<EditOp>,
) -> u64 {
    bake(world, manager, &tid);
    let ops_json =
        serde_json::to_value(&forward).unwrap_or(Value::Null);
    let mut state = world
        .get_resource_or_insert_with(MotionGfxEditState::default);
    state.record(tid, JournalEntry { forward, inverse });
    let version = state.bump(tid);
    state.push_event(
        tid,
        serde_json::json!({
            "kind": "edit",
            "version": version,
            "ops": ops_json,
        }),
    );
    version
}

/// Apply a single op as one journaled request: shared implementation
/// of the single-op BRP methods.
pub(crate) fn single_edit(
    world: &mut World,
    tid: TimelineId,
    mut op: EditOp,
) -> BrpResult {
    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, mut manager| {
            let (mut result, inverse) =
                apply_op(world, &mut manager, tid, &op)?;
            stamp_insert_id(&mut op, &result);
            let version = finish_edit(
                world,
                &mut manager,
                tid,
                alloc::vec![op],
                alloc::vec![inverse],
            );
            result["version"] = version.into();
            Ok(result)
        },
    )
}


#[derive(Deserialize)]
struct MotionGfxHistoryParams {
    id: u64,
}

/// Shared implementation of undo and redo: pop an entry off one stack,
/// re-apply one of its op lists, remap recreated action ids across the
/// whole journal, push the entry onto the other stack.
fn apply_history(
    world: &mut World,
    tid: TimelineId,
    redo: bool,
) -> BrpResult {
    // Pop the entry first (owned), so applying ops can borrow freely.
    let Some(mut entry) = world
        .get_resource_mut::<MotionGfxEditState>()
        .and_then(|mut s| {
            let journal = s.journal_mut(&tid)?;
            if redo {
                journal.redo.pop()
            } else {
                journal.undo.pop_back()
            }
        })
    else {
        return Err(err(
            error_codes::RESOURCE_ERROR,
            if redo {
                "nothing to redo"
            } else {
                "nothing to undo"
            },
        ));
    };

    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, mut manager| {
            let ops =
                if redo { &entry.forward } else { &entry.inverse };
            let mut remaps: Vec<(u64, u64)> = Vec::new();

            for (index, op) in ops.iter().enumerate() {
                let applied = apply_op(world, &mut manager, tid, op);
                match applied {
                    Ok((result, _)) => {
                        // An insert re-creates its action under a new
                        // id. Remember the mapping.
                        if let EditOp::Insert {
                            restore_id: Some(old),
                            ..
                        }
                        | EditOp::InsertKeyframes {
                            restore_id: Some(old),
                            ..
                        } = op
                            && let Some(new) =
                                result["action_id"].as_u64()
                            && *old != new
                        {
                            remaps.push((*old, new));
                        }
                    }
                    Err(mut e) => {
                        // The entry is consumed and state may be
                        // partially stepped. Clients should re-inspect.
                        bake(world, &mut manager, &tid);
                        let mut data = serde_json::json!({
                            "failed_op": index,
                            "history_entry_dropped": true,
                        });
                        if let Some(prev) = e.data.take() {
                            data["error_data"] = prev;
                        }
                        e.data = Some(data);
                        e.message = format!(
                            "{} failed at op {index}: {}",
                            if redo { "redo" } else { "undo" },
                            e.message
                        );
                        return Err(e);
                    }
                }
            }

            bake(world, &mut manager, &tid);

            let n_ops = ops.len();
            let mut state = world.get_resource_or_insert_with(
                MotionGfxEditState::default,
            );
            // Point every journal reference (including this entry's
            // own other-direction ops) at the recreated actions.
            for (old, new) in remaps {
                entry.remap_action_id(old, new);
                if let Some(journal) = state.journal_mut(&tid) {
                    journal.remap_action_id(old, new);
                }
            }
            if let Some(journal) = state.journal_mut(&tid) {
                if redo {
                    journal.undo.push_back(entry);
                } else {
                    journal.redo.push(entry);
                }
            }
            let version = state.bump(tid);
            state.push_event(
                tid,
                serde_json::json!({
                    "kind": if redo { "redo" } else { "undo" },
                    "version": version,
                }),
            );

            edit::ok(if redo {
                serde_json::json!({
                    "version": version, "redone": n_ops,
                })
            } else {
                serde_json::json!({
                    "version": version, "undone": n_ops,
                })
            })
        },
    )
}

/// `motiongfx.timeline_undo` - revert the most recent edit (a batch or
/// import is one step). Undoing a remove re-creates the action under a
/// new id, so re-`timeline_inspect` after undo.
pub fn timeline_undo(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxHistoryParams = parse(params)?;
    require_manager(world)?;
    apply_history(world, TimelineId::from_raw(p.id), false)
}

/// `motiongfx.timeline_redo` - re-apply the most recently undone edit.
pub fn timeline_redo(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxHistoryParams = parse(params)?;
    require_manager(world)?;
    apply_history(world, TimelineId::from_raw(p.id), true)
}


#[derive(Deserialize)]
struct MotionGfxBatchParams {
    id: u64,
    /// Raw ops, parsed one at a time so `entity_name` resolves to bits
    /// and an op can reference an earlier op's action as `{"$op": N}`.
    /// The journal carries only resolved bits.
    ops: Vec<Value>,
}

/// Resolve `"action_id": {"$op": N}` against the results of the ops
/// already applied in this batch. `N` must index an earlier op that
/// produced an `action_id` (an insert).
fn resolve_op_refs(
    raw: &mut Value,
    results: &[Value],
) -> Result<(), BrpError> {
    let Some(obj) = raw.as_object_mut() else {
        return Ok(());
    };
    let Some(reference) = obj.get("action_id") else {
        return Ok(());
    };
    let Some(ref_obj) = reference.as_object() else {
        return Ok(()); // plain id; nothing to resolve
    };
    let Some(n) = ref_obj.get("$op") else {
        return Err(invalid(
            "`action_id` must be a raw id or {\"$op\": <index>}",
        ));
    };
    let n = n
        .as_u64()
        .ok_or_else(|| invalid("`$op` must be an op index"))?
        as usize;
    let id = results
        .get(n)
        .and_then(|r| r["action_id"].as_u64())
        .ok_or_else(|| {
            invalid(format!(
                "{{\"$op\": {n}}} must reference an EARLIER op of \
                 this batch that created an action (an insert)"
            ))
        })?;
    obj.insert("action_id".to_string(), id.into());
    Ok(())
}

#[derive(Serialize)]
struct MotionGfxBatchResult {
    version: u64,
    results: Vec<Value>,
}

/// `motiongfx.timeline_batch` - apply several edits as one request:
/// one re-bake, all-or-nothing (failures unwind the prefix), one
/// journal entry.
pub fn timeline_batch(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxBatchParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    if p.ops.is_empty() {
        return Err(invalid("`ops` must not be empty"));
    }

    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, mut manager| {
            let raw_ops = p.ops;
            let mut forward: Vec<EditOp> =
                Vec::with_capacity(raw_ops.len());
            let mut results = Vec::with_capacity(raw_ops.len());
            let mut inverses: Vec<EditOp> = Vec::new();

            for (index, mut raw) in raw_ops.into_iter().enumerate() {
                // Parse lazily: `{"$op": N}` refs need the results of
                // the ops already applied.
                let parsed: Result<EditOp, BrpError> = (|| {
                    resolve_op_refs(&mut raw, &results)?;
                    apply_smooth_in_op(&mut raw)?;
                    edit::resolve_entity_name_in_op(world, &mut raw)?;
                    let op: EditOp = serde_json::from_value(raw)
                        .map_err(|e| invalid(e.to_string()))?;
                    if matches!(
                        op,
                        EditOp::Clear {}
                            | EditOp::Unrestorable { .. }
                    ) {
                        return Err(invalid(
                            "`clear` is not a batch op; call \
                             motiongfx.timeline_clear",
                        ));
                    }
                    Ok(op)
                })(
                );
                let applied = parsed.and_then(|mut op| {
                    apply_op(world, &mut manager, tid, &op).map(
                        |(result, inverse)| {
                            stamp_insert_id(&mut op, &result);
                            (op, result, inverse)
                        },
                    )
                });

                match applied {
                    Ok((op, result, inverse)) => {
                        forward.push(op);
                        results.push(result);
                        inverses.push(inverse);
                    }
                    Err(mut e) => {
                        // Unwind the applied prefix, newest first.
                        let mut rollback_failed = false;
                        for inv in inverses.iter().rev() {
                            if apply_op(world, &mut manager, tid, inv)
                                .is_err()
                            {
                                rollback_failed = true;
                            }
                        }
                        bake(world, &mut manager, &tid);

                        let mut data = serde_json::json!({
                            "failed_op": index,
                        });
                        if let Some(prev) = e.data.take() {
                            data["error_data"] = prev;
                        }
                        if rollback_failed {
                            data["rollback_incomplete"] = true.into();
                        }
                        e.data = Some(data);
                        e.message = format!(
                            "batch op {index} failed: {}",
                            e.message
                        );
                        return Err(e);
                    }
                }
            }

            // Inverses must undo in reverse application order.
            inverses.reverse();
            let version = finish_edit(
                world,
                &mut manager,
                tid,
                forward,
                inverses,
            );

            edit::ok(MotionGfxBatchResult { version, results })
        },
    )
}
