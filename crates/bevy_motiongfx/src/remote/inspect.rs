extern crate alloc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::TypeId;

use bevy_ecs::name::Name;
use bevy_ecs::prelude::*;
use bevy_ecs::reflect::AppTypeRegistry;
use bevy_ecs::system::In;
use bevy_remote::{BrpResult, error_codes};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use motiongfx::timeline::Timeline;

use super::catalog::MotionGfxCatalog;
use super::edit::{ease_to_json, err, parse, require_manager};
use super::state::MotionGfxEditState;
use crate::manager::{MotionGfxManager, TimelineId};
use crate::world::BevyWorld;

#[derive(Deserialize)]
pub(crate) struct MotionGfxInspectParams {
    /// Raw [`TimelineId`].
    id: u64,
    /// Include baked start/end values per clip (default `true`).
    /// Setting this to `false` skips the reflection serialization,
    /// which is cheaper for purely structural queries.
    #[serde(default = "default_true")]
    values: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize)]
struct MotionGfxInspectResult {
    id: u64,
    /// Edit version (see [`MotionGfxEditState`]).
    version: u64,
    curr_track: usize,
    curr_time: f32,
    tracks: Vec<TrackInfo>,
}

#[derive(Serialize)]
pub(crate) struct TrackInfo {
    pub(crate) index: usize,
    pub(crate) duration: f32,
    pub(crate) sequences: Vec<SequenceInfo>,
}

/// One subject+field lane: the clips animating a single field of a
/// single subject, in start order.
#[derive(Serialize)]
pub(crate) struct SequenceInfo {
    /// Raw entity bits. `null` for non-entity subjects.
    pub(crate) entity: Option<u64>,
    /// The entity's [`Name`], when it has one.
    pub(crate) entity_name: Option<String>,
    /// Component type path. `null` if the field is not in the catalog.
    pub(crate) component: Option<String>,
    /// Reflection field path. `null` if the field is not in the catalog.
    pub(crate) field: Option<String>,
    /// `true` when the subject entity no longer exists -
    /// these clips sample into nothing. `motiongfx.timeline_gc`
    /// removes them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) dangling: Option<bool>,
    pub(crate) clips: Vec<ClipInfo>,
}

#[derive(Serialize)]
pub(crate) struct ClipInfo {
    pub(crate) action_id: u64,
    pub(crate) start: f32,
    pub(crate) duration: f32,
    /// Baked start value. `null` when unavailable (no catalog entry,
    /// not yet baked) or when `values: false` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) start_value: Option<Value>,
    /// Baked end value. Same availability as `start_value`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) end_value: Option<Value>,
    /// `"linear"`-equivalent omissions are `null`. Otherwise an ease
    /// name, `{"cubic_bezier": [...]}`, or `"custom"` for an unknown fn.
    pub(crate) ease: Option<Value>,
    /// Present on keyframed clips (phase 6): the point list
    /// `[{t, value, ease?}]` with `t` normalized within the clip.
    /// For these, `ease` above is the clip-level time warp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) keyframes: Option<Vec<super::batch::KeyframeDoc>>,
    /// Display label, when one was attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) label: Option<String>,
    /// Display colour `[r, g, b]`, when one was attached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<[u8; 3]>,
    /// `false` when the clip is muted. Omitted when enabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled: Option<bool>,
}

/// Build the [`TrackInfo`]s for a timeline. Shared with
/// `timeline_export`, which walks the same structure. `tid` keys the
/// clip-metadata lookup (labels/colours).
pub(crate) fn collect_tracks(
    timeline: &Timeline<BevyWorld>,
    catalog: &MotionGfxCatalog,
    world: &World,
    registry: &bevy_reflect::TypeRegistry,
    values: bool,
    tid: TimelineId,
) -> Vec<TrackInfo> {
    let aw = timeline.action_world();
    let meta_map = world
        .get_resource::<MotionGfxEditState>()
        .and_then(|s| s.clip_meta_map(&tid));

    timeline
        .tracks()
        .iter()
        .enumerate()
        .map(|(index, track)| TrackInfo {
            index,
            duration: track.duration(),
            sequences: track
                .sequences_spans()
                .iter()
                .map(|(key, span)| {
                    // Recover the entity from the type-erased subject.
                    let entity = (key.subject_id().type_id()
                        == TypeId::of::<Entity>())
                    .then(|| {
                        aw.get_id::<Entity>(&key.subject_id().uid())
                            .copied()
                    })
                    .flatten();
                    let entity_name = entity
                        .and_then(|e| world.get::<Name>(e))
                        .map(|n| n.as_str().to_string());

                    let entry = catalog.get_by_field(key.field());
                    let (component, field) = match entry {
                        Some(((c, f), _)) => {
                            (Some(c.clone()), Some(f.clone()))
                        }
                        None => (None, None),
                    };

                    let clips = track
                        .clips(*span)
                        .iter()
                        .map(|clip| {
                            let (start_value, end_value) = match entry
                            {
                                Some((_, e)) if values => (e
                                    .serialize)(
                                    aw, clip.id, registry,
                                )
                                .map_or((None, None), |(s, e)| {
                                    (Some(s), Some(e))
                                }),
                                _ => (None, None),
                            };
                            let keyframes = match entry {
                                Some((_, e)) if values => {
                                    super::batch::keyframe_docs(
                                        aw, clip.id, e, registry,
                                    )
                                }
                                _ => None,
                            };

                            let meta = meta_map
                                .and_then(|m| {
                                    m.get(&clip.id.to_bits())
                                })
                                .cloned()
                                .unwrap_or_default();

                            ClipInfo {
                                action_id: clip.id.to_bits(),
                                start: clip.start,
                                duration: clip.duration,
                                start_value,
                                end_value,
                                ease: aw
                                    .get_ease(clip.id)
                                    .map(ease_to_json),
                                keyframes,
                                label: meta.label,
                                color: meta.color,
                                enabled: aw
                                    .is_disabled(clip.id)
                                    .then_some(false),
                            }
                        })
                        .collect();

                    SequenceInfo {
                        entity: entity.map(Entity::to_bits),
                        entity_name,
                        component,
                        field,
                        dangling: entity
                            .is_some_and(|e| {
                                world.get_entity(e).is_err()
                            })
                            .then_some(true),
                        clips,
                    }
                })
                .collect(),
        })
        .collect()
}


#[derive(Deserialize)]
struct MotionGfxValueAtParams {
    /// Raw [`TimelineId`].
    id: u64,
    /// Track-local time (seconds) to evaluate at.
    time: f32,
    /// Track index (default 0).
    #[serde(default)]
    track: usize,
    #[serde(default)]
    entity: Option<u64>,
    #[serde(default)]
    entity_name: Option<String>,
    component: String,
    field: String,
}

/// Numeric mirror of `pipeline::sample_keyframes`, over the JSON
/// values the catalog serializes: implicit `(0, start)` anchor, the
/// destination point's ease shapes each segment, `hold` points step,
/// holds past the last point. `points` is sorted (the remote
/// constructor guarantees it).
fn sample_keyframes_json(
    points: &[(f32, Value, Option<motiongfx::action::Ease>, bool)],
    start: f64,
    t: f32,
) -> Option<f64> {
    let idx = points.partition_point(|(pt, ..)| *pt <= t);
    let Some((t1, v1, ease, hold)) = points.get(idx) else {
        // Past the last point: hold its value.
        return points.last().and_then(|(_, v, ..)| v.as_f64());
    };
    let (t0, v0) = match idx.checked_sub(1).map(|i| &points[i]) {
        Some((pt, pv, ..)) => (*pt, pv.as_f64()?),
        None => (0.0, start),
    };
    if *hold {
        // Step segment: previous value until exactly `t1`.
        return Some(v0);
    }
    let span = t1 - t0;
    let local = if span <= f32::EPSILON {
        1.0
    } else {
        ((t - t0) / span).clamp(0.0, 1.0)
    };
    let local = match ease {
        Some(e) => e.eval(local),
        None => local,
    };
    Some(v0 + (v1.as_f64()? - v0) * f64::from(local))
}

/// `motiongfx.value_at` - evaluate one field's baked curve at a time,
/// server-side. Numeric fields only. Returns the value plus where
/// `time` fell: `before` the first clip, `inside` one (with its
/// `action_id`), in a `gap`, or `after` the last.
pub fn value_at(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    use super::edit::{invalid, resolve_subject_entity};

    let p: MotionGfxValueAtParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);
    let entity_bits = resolve_subject_entity(
        world,
        p.entity,
        p.entity_name.as_deref(),
    )?;

    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, mut manager| {
            // Segments must be fresh to read values out.
            if let Some((registry, timeline)) =
                manager.registry_and_timeline_mut(&tid)
            {
                timeline.bake_actions(
                    registry,
                    BevyWorld::from_ref(world),
                );
            }
            let timeline =
                manager.get_timeline(&tid).ok_or_else(|| {
                    err(
                        error_codes::RESOURCE_ERROR,
                        alloc::format!(
                            "No timeline with id {}",
                            p.id
                        ),
                    )
                })?;
            let track =
                timeline.tracks().get(p.track).ok_or_else(|| {
                    err(
                        error_codes::RESOURCE_ERROR,
                        "track index out of range",
                    )
                })?;
            let catalog = world.resource::<MotionGfxCatalog>();
            let type_registry =
                world.resource::<AppTypeRegistry>().clone();
            let registry = type_registry.read();
            let aw = timeline.action_world();

            // Find the (subject, field) sequence on this track.
            let mut found = None;
            for (key, span) in track.sequences_spans() {
                let entity = (key.subject_id().type_id()
                    == TypeId::of::<Entity>())
                .then(|| {
                    aw.get_id::<Entity>(&key.subject_id().uid())
                        .copied()
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
                if *c != p.component || *f != p.field {
                    continue;
                }
                found = Some((entry, track.clips(*span)));
                break;
            }
            let Some((entry, clips)) = found else {
                return Err(err(
                    error_codes::RESOURCE_ERROR,
                    alloc::format!(
                        "no clips animate `{}`.`{}` of that entity \
                         on track {}",
                        p.component,
                        p.field,
                        p.track
                    ),
                ));
            };

            let numeric = |v: &Value| {
                v.as_f64().ok_or_else(|| {
                    invalid("value_at supports numeric fields only")
                })
            };
            let respond =
                |value: f64,
                 position: &str,
                 action_id: Option<u64>| {
                    super::edit::ok(serde_json::json!({
                        "value": value,
                        "position": position,
                        "action_id": action_id,
                        "time": p.time,
                    }))
                };

            // Before the first clip: its (chained) start value.
            let first = clips.first().expect("spans are non-empty");
            if p.time < first.start {
                let (start, _) =
                    (entry.serialize)(aw, first.id, &registry)
                        .ok_or_else(|| {
                            err(
                                error_codes::INTERNAL_ERROR,
                                "could not serialize the segment",
                            )
                        })?;
                return respond(numeric(&start)?, "before", None);
            }

            // Inside a clip, or in the gap after the previous one.
            for (i, clip) in clips.iter().enumerate() {
                let end = clip.start + clip.duration;
                if p.time <= end {
                    if p.time < clip.start {
                        // Gap: the previous clip's end holds.
                        let prev = &clips[i - 1];
                        let (_, value) = (entry.serialize)(
                            aw, prev.id, &registry,
                        )
                        .ok_or_else(|| {
                            err(
                                error_codes::INTERNAL_ERROR,
                                "could not serialize the segment",
                            )
                        })?;
                        return respond(
                            numeric(&value)?,
                            "gap",
                            None,
                        );
                    }

                    // Disabled clips hold their chained start.
                    let (start, end_value) =
                        (entry.serialize)(aw, clip.id, &registry)
                            .ok_or_else(|| {
                                err(
                                    error_codes::INTERNAL_ERROR,
                                    "could not serialize the segment",
                                )
                            })?;
                    let start = numeric(&start)?;
                    if aw.is_disabled(clip.id) {
                        return respond(
                            start,
                            "inside",
                            Some(clip.id.to_bits()),
                        );
                    }

                    let mut t = if clip.duration <= f32::EPSILON {
                        1.0
                    } else {
                        ((p.time - clip.start) / clip.duration)
                            .clamp(0.0, 1.0)
                    };
                    if let Some(ease) = aw.get_ease(clip.id) {
                        t = ease.eval(t);
                    }
                    let value = match (entry.serialize_keyframes)(
                        aw, clip.id, &registry,
                    ) {
                        Some(points) => sample_keyframes_json(
                            &points, start, t,
                        )
                        .ok_or_else(|| {
                            invalid(
                                "value_at supports numeric fields \
                                 only",
                            )
                        })?,
                        None => {
                            let end = numeric(&end_value)?;
                            start + (end - start) * f64::from(t)
                        }
                    };
                    return respond(
                        value,
                        "inside",
                        Some(clip.id.to_bits()),
                    );
                }
            }

            // Past the last clip: its end value holds.
            let last = clips.last().expect("non-empty");
            let (_, value) =
                (entry.serialize)(aw, last.id, &registry)
                    .ok_or_else(|| {
                        err(
                            error_codes::INTERNAL_ERROR,
                            "could not serialize the segment",
                        )
                    })?;
            respond(numeric(&value)?, "after", None)
        },
    )
}

/// `motiongfx.timeline_inspect` - the full clip graph of one timeline.
pub fn timeline_inspect(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxInspectParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, manager| {
            let timeline =
                manager.get_timeline(&tid).ok_or_else(|| {
                    err(
                        error_codes::RESOURCE_ERROR,
                        alloc::format!(
                            "No timeline with id {}",
                            p.id
                        ),
                    )
                })?;
            let catalog = world
                .get_resource::<MotionGfxCatalog>()
                .ok_or_else(|| {
                    err(
                        error_codes::RESOURCE_ERROR,
                        "MotionGfxCatalog resource missing",
                    )
                })?;
            let type_registry =
                world.resource::<AppTypeRegistry>().clone();
            let registry = type_registry.read();

            let tracks = collect_tracks(
                timeline, catalog, world, &registry, p.values, tid,
            );

            super::edit::ok(MotionGfxInspectResult {
                id: p.id,
                version: world
                    .get_resource::<MotionGfxEditState>()
                    .map(|s| s.version(&tid))
                    .unwrap_or(0),
                curr_track: timeline.curr_index(),
                curr_time: timeline.curr_time(),
                tracks,
            })
        },
    )
}
