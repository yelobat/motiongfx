use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use bevy_ecs::system::In;
use bevy_remote::{BrpError, BrpResult, RemotePlugin, error_codes};
use serde::{Deserialize, Serialize};
use serde_json::Value;

extern crate alloc;
use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;

pub mod batch;
pub mod catalog;
pub mod dynamic;
pub mod edit;
pub mod inspect;
pub mod persist;
pub mod project;
pub mod state;

use bevy_platform::collections::HashMap;

use crate::controller::{FixedRatePlayer, RealtimePlayer};
use crate::manager::{
    MotionGfxManager, TimelineComplete, TimelineId,
};

use catalog::MotionGfxCatalog;
use dynamic::DynAnimations;
use state::MotionGfxEditState;

pub use catalog::MotionGfxAnimatableApp;
pub use project::MotionGfxProjectApp;

pub const MOTIONGFX_LIST_METHOD: &str = "motiongfx.list";

/// The method path for a `motiongfx.get` request.
pub const MOTIONGFX_GET_METHOD: &str = "motiongfx.get";

/// The method path for a `motiongfx.get+watch` request.
pub const MOTIONGFX_GET_WATCH_METHOD: &str = "motiongfx.get+watch";

/// The method path for a `motiongfx.journal+watch` request.
pub const MOTIONGFX_JOURNAL_WATCH_METHOD: &str =
    "motiongfx.journal+watch";

/// The method path for a `motiongfx.seek` request.
pub const MOTIONGFX_SEEK_METHOD: &str = "motiongfx.seek";

/// The method path for a `motiongfx.play` request.
pub const MOTIONGFX_PLAY_METHOD: &str = "motiongfx.play";

/// The method path for a `motiongfx.pause` request.
pub const MOTIONGFX_PAUSE_METHOD: &str = "motiongfx.pause";

/// The method path for a `motiongfx.set_time_scale` request.
pub const MOTIONGFX_SET_TIME_SCALE_METHOD: &str =
    "motiongfx.set_time_scale";

/// The method path for a `motiongfx.remove` request.
pub const MOTIONGFX_REMOVE_METHOD: &str = "motiongfx.remove";

/// The method path for a `motiongfx.timeline_create` request.
pub const MOTIONGFX_TIMELINE_CREATE_METHOD: &str =
    "motiongfx.timeline_create";

/// The method path for a `motiongfx.timeline_track_add` request.
pub const MOTIONGFX_TIMELINE_TRACK_ADD_METHOD: &str =
    "motiongfx.timeline_track_add";

/// The method path for a `motiongfx.timeline_rename` request.
pub const MOTIONGFX_TIMELINE_RENAME_METHOD: &str =
    "motiongfx.timeline_rename";

/// The method path for a `motiongfx.spawn` request.
pub const MOTIONGFX_SPAWN_METHOD: &str = "motiongfx.spawn";

/// The method path for a `motiongfx.animate` request.
pub const MOTIONGFX_ANIMATE_METHOD: &str = "motiongfx.animate";

/// The method path for a `motiongfx.list_animations` request.
pub const MOTIONGFX_LIST_ANIMATIONS_METHOD: &str =
    "motiongfx.list_animations";

/// The method path for a `motiongfx.remove_animation` request.
pub const MOTIONGFX_REMOVE_ANIMATION_METHOD: &str =
    "motiongfx.remove_animation";

/// The method path for a `motiongfx.animatable_fields` request.
pub const MOTIONGFX_ANIMATABLE_FIELDS_METHOD: &str =
    "motiongfx.animatable_fields";

/// The method path for a `motiongfx.timeline_insert_action` request.
pub const MOTIONGFX_TIMELINE_INSERT_ACTION_METHOD: &str =
    "motiongfx.timeline_insert_action";

/// The method path for a `motiongfx.timeline_insert_keyframes` request.
pub const MOTIONGFX_TIMELINE_INSERT_KEYFRAMES_METHOD: &str =
    "motiongfx.timeline_insert_keyframes";

/// The method path for a `motiongfx.timeline_remove_action` request.
pub const MOTIONGFX_TIMELINE_REMOVE_ACTION_METHOD: &str =
    "motiongfx.timeline_remove_action";

/// The method path for a `motiongfx.timeline_move_action` request.
pub const MOTIONGFX_TIMELINE_MOVE_ACTION_METHOD: &str =
    "motiongfx.timeline_move_action";

/// The method path for a `motiongfx.timeline_clear` request.
pub const MOTIONGFX_TIMELINE_CLEAR_METHOD: &str =
    "motiongfx.timeline_clear";

/// The method path for a `motiongfx.timeline_gc` request.
pub const MOTIONGFX_TIMELINE_GC_METHOD: &str =
    "motiongfx.timeline_gc";

/// The method path for a `motiongfx.timeline_inspect` request.
pub const MOTIONGFX_TIMELINE_INSPECT_METHOD: &str =
    "motiongfx.timeline_inspect";

/// The method path for a `motiongfx.value_at` request.
pub const MOTIONGFX_VALUE_AT_METHOD: &str = "motiongfx.value_at";

/// The method path for a `motiongfx.timeline_update_action` request.
pub const MOTIONGFX_TIMELINE_UPDATE_ACTION_METHOD: &str =
    "motiongfx.timeline_update_action";

/// The method path for a `motiongfx.timeline_batch` request.
pub const MOTIONGFX_TIMELINE_BATCH_METHOD: &str =
    "motiongfx.timeline_batch";

/// The method path for a `motiongfx.timeline_export` request.
pub const MOTIONGFX_TIMELINE_EXPORT_METHOD: &str =
    "motiongfx.timeline_export";

/// The method path for a `motiongfx.timeline_import` request.
pub const MOTIONGFX_TIMELINE_IMPORT_METHOD: &str =
    "motiongfx.timeline_import";

/// The method path for a `motiongfx.project_save` request.
pub const MOTIONGFX_PROJECT_SAVE_METHOD: &str =
    "motiongfx.project_save";

/// The method path for a `motiongfx.project_load` request.
pub const MOTIONGFX_PROJECT_LOAD_METHOD: &str =
    "motiongfx.project_load";

/// The method path for a `motiongfx.project_reset` request.
pub const MOTIONGFX_PROJECT_RESET_METHOD: &str =
    "motiongfx.project_reset";

/// The method path for a `motiongfx.marker_set` request.
pub const MOTIONGFX_MARKER_SET_METHOD: &str = "motiongfx.marker_set";

/// The method path for a `motiongfx.marker_remove` request.
pub const MOTIONGFX_MARKER_REMOVE_METHOD: &str =
    "motiongfx.marker_remove";

/// The method path for a `motiongfx.marker_list` request.
pub const MOTIONGFX_MARKER_LIST_METHOD: &str =
    "motiongfx.marker_list";

/// The method path for a `motiongfx.timeline_undo` request.
pub const MOTIONGFX_TIMELINE_UNDO_METHOD: &str =
    "motiongfx.timeline_undo";

/// The method path for a `motiongfx.timeline_redo` request.
pub const MOTIONGFX_TIMELINE_REDO_METHOD: &str =
    "motiongfx.timeline_redo";

/// The method path for a `motiongfx.schema` request.
pub const MOTIONGFX_SCHEMA_METHOD: &str = "motiongfx.schema";

/// BRP Method descriptions:
/// - method name
/// - parameters
/// - return type
/// - documentation
const SCHEMA_TABLE: &[(&str, &str, &str, &str)] = &[
    (
        "motiongfx.list",
        "{}",
        "[TimelineState]",
        "Summary of every timeline.",
    ),
    (
        "motiongfx.get+watch",
        "{id, watcher?}",
        "TimelineState (streamed)",
        "Watch for timeline changes.",
    ),
    (
        "motiongfx.journal+watch",
        "{id, watcher?, marker_events? = true}",
        "{events: [...]} (streamed)",
        "Watch for edit events.",
    ),
    (
        "motiongfx.get",
        "{id}",
        "TimelineState",
        "Playback state + edit version of one timeline.",
    ),
    (
        "motiongfx.seek",
        "{id, time?, track?, marker?}",
        "TimelineState",
        "Set target time/track, or jump to a marker.",
    ),
    (
        "motiongfx.play",
        "{id, time_scale?}",
        "TimelineState",
        "Start the realtime player.",
    ),
    (
        "motiongfx.pause",
        "{id}",
        "TimelineState",
        "Stop the realtime player.",
    ),
    (
        "motiongfx.set_time_scale",
        "{id, time_scale}",
        "TimelineState",
        "Change playback speed/direction (negative = backwards).",
    ),
    (
        "motiongfx.remove",
        "{id}",
        "{removed}",
        "Drop a timeline (and its edit state) from the manager.",
    ),
    (
        "motiongfx.timeline_create",
        "{name?, tracks? = 1, player? = true}",
        "TimelineState",
        "Create an empty timeline.",
    ),
    (
        "motiongfx.timeline_track_add",
        "{id, count? = 1}",
        "{track_count, version}",
        "Append empty tracks.",
    ),
    (
        "motiongfx.timeline_rename",
        "{id, name?}",
        "TimelineState",
        "Set a timeline's display name.",
    ),
    (
        "motiongfx.spawn",
        "{components}",
        "{entity}",
        "Spawn an entity from reflected components.",
    ),
    (
        "motiongfx.animate",
        "{entity|entity_name, component, duration, fields}",
        "{id}",
        "Reflection-driven animation.",
    ),
    (
        "motiongfx.list_animations",
        "{}",
        "[DynAnimation]",
        "Summarise all dynamic animations.",
    ),
    (
        "motiongfx.remove_animation",
        "{id}",
        "{removed}",
        "Remove a dynamic animation.",
    ),
    (
        "motiongfx.animatable_fields",
        "{component?}",
        "[{component, field, target_type, subject}]",
        "What the catalog can remote-edit; subject is `component` or \
      `asset`.",
    ),
    (
        "motiongfx.timeline_inspect",
        "{id, values? = true}",
        "{id, version, curr_track, curr_time, tracks}",
        "The full clip graph, with baked values and eases.",
    ),
    (
        "motiongfx.timeline_insert_action",
        "{id, track, entity|entity_name, component, field, \
      to|to_relative, duration, start_at?, ease?, asset_of?, \
      label?, color?, enabled?}",
        "{action_id, version}",
        "Append a constant-target tween.",
    ),
    (
        "motiongfx.timeline_insert_keyframes",
        "{id, track, entity|entity_name, component, field, keyframes: \
      [{t, value, ease?, hold?}], duration, start_at?, ease?, \
      smooth?, asset_of?, label?, color?, enabled?}",
        "{action_id, version}",
        "Append ONE clip whose value follows the keyframes (t \
      normalized 0..=1 within the clip, duration-weighted by their \
      spacing).",
    ),
    (
        "motiongfx.value_at",
        "{id, time, track? = 0, entity|entity_name, component, field}",
        "{value, position, action_id?, time}",
        "Evaluate one field's baked curve at a given time.",
    ),
    (
        "motiongfx.timeline_remove_action",
        "{id, track, action_id}",
        "{removed, version}",
        "Drop a clip.",
    ),
    (
        "motiongfx.timeline_move_action",
        "{id, track, action_id, start_at, duration?}",
        "{action_id, version}",
        "Reschedule a clip in place.",
    ),
    (
        "motiongfx.timeline_update_action",
        "{id, action_id, to?|to_relative?, ease?, keyframes?, \
      smooth?, label?, color?, enabled?}",
        "{action_id, version}",
        "Tweak a clip's value/ease in place.",
    ),
    (
        "motiongfx.timeline_clear",
        "{id}",
        "{cleared, version}",
        "Drop every clip.",
    ),
    (
        "motiongfx.timeline_gc",
        "{id, dry_run? = false}",
        "{dangling, removed, version?}",
        "Find (if dry_run is true) or remove the clips whose subject entity was \
      despawned.",
    ),
    (
        "motiongfx.timeline_batch",
        "{id, ops: [{op, ...}]}",
        "{version, results}",
        "Apply ops as one request.",
    ),
    (
        "motiongfx.timeline_export",
        "{id}",
        "TimelineDocument",
        "Snapshot the timeline as a portable document.",
    ),
    (
        "motiongfx.timeline_import",
        "{id, document, mode? = replace|append, bindings?}",
        "{version, inserted}",
        "Load a document.",
    ),
    (
        "motiongfx.project_save",
        "{id?, path?}",
        "{entities, path? | document?}",
        "Save the whole project.",
    ),
    (
        "motiongfx.project_load",
        "{id?, path? | document?}",
        "{entities, despawned, timeline, timelines}",
        "Load a project.",
    ),
    (
        "motiongfx.project_reset",
        "{}",
        "{despawned}",
        "Despawn every registered project subject.",
    ),
    (
        "motiongfx.marker_set",
        "{id, name, time, track? = current}",
        "{version}",
        "Create or move a named time anchor.",
    ),
    (
        "motiongfx.marker_remove",
        "{id, name}",
        "{removed, version}",
        "Drop a marker.",
    ),
    (
        "motiongfx.marker_list",
        "{id}",
        "{name: {track, time}}",
        "All markers of a timeline.",
    ),
    (
        "motiongfx.timeline_undo",
        "{id}",
        "{version, undone}",
        "Revert the most recent edit.",
    ),
    (
        "motiongfx.timeline_redo",
        "{id}",
        "{version, redone}",
        "Re-apply the most recently undone edit.",
    ),
    (
        "motiongfx.schema",
        "{}",
        "{protocol, version, methods}",
        "This table.",
    ),
];

/// The protocol version.
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// Registers MotionGfx's reflection types for the Bevy Remote
/// Protocol.
#[derive(Default)]
pub struct MotionGfxRemotePlugin;

/// Custom BRP method schema.
#[derive(Resource, Default)]
pub struct MotionGfxSchema {
    rows: Vec<Value>,
}

pub trait MotionGfxSchemaApp {
    fn describe_brp_method(
        &mut self,
        name: &str,
        params: &str,
        returns: &str,
        doc: &str,
    ) -> &mut Self;
}

impl MotionGfxSchemaApp for App {
    fn describe_brp_method(
        &mut self,
        name: &str,
        params: &str,
        returns: &str,
        doc: &str,
    ) -> &mut Self {
        self.init_resource::<MotionGfxSchema>();
        self.world_mut()
            .resource_mut::<MotionGfxSchema>()
            .rows
            .push(serde_json::json!({
                "name": name,
                "params": params,
                "returns": returns,
                "doc": doc,
                "app_local": true,
            }));
        self
    }
}

pub fn schema(
    In(_): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let mut methods: Vec<Value> = SCHEMA_TABLE
        .iter()
        .map(|(name, params, returns, doc)| {
            serde_json::json!({
                "name": name,
                "params": params,
                "returns": returns,
                "doc": doc
            })
        })
        .collect();
    if let Some(schema) = world.get_resource::<MotionGfxSchema>() {
        methods.extend(schema.rows.iter().cloned());
    }
    Ok(serde_json::json!({
        "protocol": "motiongfx",
        "version": PROTOCOL_VERSION,
        "methods": methods,
    }))
}

#[derive(Deserialize)]
struct TimelineRef {
    id: u64,
}

#[derive(Deserialize)]
struct MotionGfxSeekParams {
    id: u64,
    /// Target time, clamped to `[0, track_duration]` by the timeline.
    #[serde(default)]
    time: Option<f32>,
    /// Target track index, clamped to `[0, last_track]`.
    #[serde(default)]
    track: Option<usize>,
    /// A marker name (see `motiongfx.marker_set`).
    marker: Option<alloc::string::String>,
}

#[derive(Deserialize)]
struct MotionGfxPlayParams {
    id: u64,
    /// Optional new time scale (negative plays backwards).
    #[serde(default)]
    time_scale: Option<f32>,
}

#[derive(Deserialize)]
struct MotionGfxTimeScaleParams {
    id: u64,
    time_scale: f32,
}

/// Serialize snapshot of a timeline's playback state.
#[derive(Serialize)]
struct TimelineState {
    id: u64,
    /// Display name, if one was set.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<alloc::string::String>,
    curr_time: f32,
    target_time: f32,
    curr_track: usize,
    target_track: usize,
    track_count: usize,
    is_complete: bool,
    /// The edit version: bumped every time the timeline is mutated
    /// with a `motiongfx.*` request.
    version: u64,
    /// Present only when a [`RealtimePlayer`] component exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    is_playing: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_scale: Option<f32>,
}

fn parse<T: for<'de> Deserialize<'de>>(
    params: Option<Value>,
) -> Result<T, BrpError> {
    let value = params.ok_or_else(|| BrpError {
        code: error_codes::INVALID_PARAMS,
        message: "Missing params".to_string(),
        data: None,
    })?;
    serde_json::from_value(value).map_err(|e| BrpError {
        code: error_codes::INVALID_PARAMS,
        message: e.to_string(),
        data: None,
    })
}

fn not_found(id: u64) -> BrpError {
    BrpError {
        code: error_codes::RESOURCE_ERROR,
        message: format!("No timeline with id {id}"),
        data: None,
    }
}

fn ok<T: Serialize>(value: T) -> BrpResult {
    serde_json::to_value(value).map_err(|e| BrpError {
        code: error_codes::INTERNAL_ERROR,
        message: e.to_string(),
        data: None,
    })
}

/// Find the entity carrying the [`TimelineId`] with `raw` id, if any.
fn timeline_entity(world: &mut World, raw: u64) -> Option<Entity> {
    let target = TimelineId::from_raw(raw);
    let mut q = world.query::<(Entity, &TimelineId)>();
    q.iter(world).find(|(_, id)| **id == target).map(|(e, _)| e)
}

/// Build a playback-state snapshot of one timeline, if it exists.
fn snapshot(world: &mut World, raw: u64) -> Option<TimelineState> {
    let id = TimelineId::from_raw(raw);
    let manager = world.get_resource::<MotionGfxManager>()?;
    let timeline = manager.get_timeline(&id)?;

    let mut state = TimelineState {
        id: raw,
        name: world
            .get_resource::<MotionGfxEditState>()
            .and_then(|s| s.name(&id).map(ToString::to_string)),
        curr_time: timeline.curr_time(),
        target_time: timeline.target_time(),
        curr_track: timeline.curr_index(),
        target_track: timeline.target_index(),
        track_count: timeline.tracks().len(),
        is_complete: timeline.is_complete(),
        version: world
            .get_resource::<MotionGfxEditState>()
            .map(|s| s.version(&id))
            .unwrap_or(0),
        is_playing: None,
        time_scale: None,
    };

    if let Some(entity) = timeline_entity(world, raw)
        && let Some(player) = world.get::<RealtimePlayer>(entity)
    {
        state.is_playing = Some(player.is_playing);
        state.time_scale = Some(player.time_scale);
    }

    Some(state)
}

/// Build a snapshot and serialize it, or return a not-found error.
fn snapshot_result(world: &mut World, raw: u64) -> BrpResult {
    match snapshot(world, raw) {
        Some(state) => ok(state),
        None => Err(not_found(raw)),
    }
}

impl MotionGfxRemotePlugin {
    /// Extend an existing [`RemotePlugin`] with the `motiongfx.*`
    /// methods. Add [`MotionGfxRemotePlugin`] itself as well, so the
    /// resources and systems the methods rely on are registered.
    pub fn extend(plugin: RemotePlugin) -> RemotePlugin {
        plugin
            .with_method_main(MOTIONGFX_LIST_METHOD, list_timelines)
            .with_method_main(MOTIONGFX_GET_METHOD, get_timeline)
            .with_watching_method_main(
                MOTIONGFX_GET_WATCH_METHOD,
                get_timeline_watching,
            )
            .with_watching_method_main(
                MOTIONGFX_JOURNAL_WATCH_METHOD,
                journal_watching,
            )
            .with_method_main(MOTIONGFX_SEEK_METHOD, seek_timeline)
            .with_method_main(MOTIONGFX_PLAY_METHOD, play_timeline)
            .with_method_main(MOTIONGFX_PAUSE_METHOD, pause_timeline)
            .with_method_main(
                MOTIONGFX_SET_TIME_SCALE_METHOD,
                set_time_scale,
            )
            .with_method_main(MOTIONGFX_REMOVE_METHOD, remove_timeline)
            .with_method_main(
                MOTIONGFX_TIMELINE_CREATE_METHOD,
                timeline_create,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_TRACK_ADD_METHOD,
                edit::timeline_track_add,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_RENAME_METHOD,
                timeline_rename,
            )
            .with_method_main(MOTIONGFX_SPAWN_METHOD, dynamic::brp_spawn)
            .with_method_main(
                MOTIONGFX_ANIMATE_METHOD,
                dynamic::brp_animate,
            )
            .with_method_main(
                MOTIONGFX_LIST_ANIMATIONS_METHOD,
                dynamic::brp_list_animations,
            )
            .with_method_main(
                MOTIONGFX_REMOVE_ANIMATION_METHOD,
                dynamic::brp_remove_animation,
            )
            .with_method_main(
                MOTIONGFX_ANIMATABLE_FIELDS_METHOD,
                edit::animatable_fields,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_INSERT_ACTION_METHOD,
                edit::timeline_insert_action,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_INSERT_KEYFRAMES_METHOD,
                edit::timeline_insert_keyframes,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_REMOVE_ACTION_METHOD,
                edit::timeline_remove_action,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_MOVE_ACTION_METHOD,
                edit::timeline_move_action,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_CLEAR_METHOD,
                edit::timeline_clear,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_GC_METHOD,
                edit::timeline_gc,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_INSPECT_METHOD,
                inspect::timeline_inspect,
            )
            .with_method_main(MOTIONGFX_VALUE_AT_METHOD, inspect::value_at)
            .with_method_main(
                MOTIONGFX_TIMELINE_UPDATE_ACTION_METHOD,
                edit::timeline_update_action,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_BATCH_METHOD,
                batch::timeline_batch,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_EXPORT_METHOD,
                persist::timeline_export,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_IMPORT_METHOD,
                persist::timeline_import,
            )
            .with_method_main(
                MOTIONGFX_MARKER_SET_METHOD,
                persist::marker_set,
            )
            .with_method_main(
                MOTIONGFX_MARKER_REMOVE_METHOD,
                persist::marker_remove,
            )
            .with_method_main(
                MOTIONGFX_MARKER_LIST_METHOD,
                persist::marker_list,
            )
            .with_method_main(
                MOTIONGFX_PROJECT_SAVE_METHOD,
                project::project_save,
            )
            .with_method_main(
                MOTIONGFX_PROJECT_LOAD_METHOD,
                project::project_load,
            )
            .with_method_main(
                MOTIONGFX_PROJECT_RESET_METHOD,
                project::project_reset,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_UNDO_METHOD,
                batch::timeline_undo,
            )
            .with_method_main(
                MOTIONGFX_TIMELINE_REDO_METHOD,
                batch::timeline_redo,
            )
            .with_method_main(MOTIONGFX_SCHEMA_METHOD, schema)
    }
}

impl Plugin for MotionGfxRemotePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<TimelineId>()
            .register_type::<RealtimePlayer>()
            .register_type::<FixedRatePlayer>()
            .register_type::<TimelineComplete>();

        // Catalog of remote-editable fields, populated by
        // `App::register_animatable`.
        app.init_resource::<MotionGfxCatalog>();
        // Per-timeline edit versions, journals and markers.
        app.init_resource::<MotionGfxEditState>();
        // Storage + driver for reflection-driven animations.
        app.init_resource::<DynAnimations>().add_systems(
            bevy_app::PostUpdate,
            dynamic::apply_dyn_animations
                .in_set(crate::MotionGfxSystems::Sample),
        );
    }
}

/// `motiongfx.list` - summary of every known timeline.
fn list_timelines(
    In(_): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let ids: Vec<u64> = {
        let manager = world
            .get_resource::<MotionGfxManager>()
            .ok_or_else(|| BrpError {
                code: error_codes::RESOURCE_ERROR,
                message: "MotionGfxManager resource missing"
                    .to_string(),
                data: None,
            })?;
        manager.iter_ids().map(TimelineId::raw).collect()
    };

    let states: Vec<TimelineState> = ids
        .into_iter()
        .filter_map(|id| snapshot(world, id))
        .collect();
    ok(states)
}

/// `motiongfx.get` - full state of a single timeline.
fn get_timeline(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let TimelineRef { id } = parse(params)?;
    snapshot_result(world, id)
}

#[derive(Deserialize)]
struct WatchRef {
    id: u64,
    /// Distinguishes concurrent watchers of the same timeline.
    /// Each `(id, watcher)` pair keeps its own dedupe state.
    #[serde(default)]
    watcher: Option<alloc::string::String>,
}

fn get_timeline_watching(
    In(params): In<Option<Value>>,
    world: &mut World,
    mut last: Local<HashMap<(u64, alloc::string::String), Value>>,
) -> BrpResult<Option<Value>> {
    let WatchRef { id, watcher } = parse(params)?;
    let Some(state) = snapshot(world, id) else {
        // Erroring ends the stream - the timeline is gone.
        return Err(not_found(id));
    };
    let value =
        serde_json::to_value(state).map_err(|e| BrpError {
            code: error_codes::INTERNAL_ERROR,
            message: e.to_string(),
            data: None,
        })?;
    if last.len() > JOURNAL_CURSOR_CAP {
        last.clear();
    }
    let key = (id, watcher.unwrap_or_default());
    if last.get(&key) == Some(&value) {
        return Ok(None);
    }
    last.insert(key, value.clone());
    Ok(Some(value))
}

#[derive(Deserialize)]
struct MotionGfxJournalWatchParams {
    id: u64,
    /// Distinguishes concurrent watchers. Each `(id, watcher)` pair
    /// gets its own cursor.
    #[serde(default)]
    watcher: Option<alloc::string::String>,
    /// Emit `{"kind": "marker"}` events when the playhead crosses a
    /// marker on the current track (default `true`).
    #[serde(default = "default_true")]
    marker_events: bool,
}

fn default_true() -> bool {
    true
}

/// Journal cursor for [`journal_watching`].
pub struct JournalCursor {
    seq: u64,
    last_time: f32,
    last_track: usize,
}

/// How many `(timeline, watcher)` cursors are kept before the table
/// resets.
const JOURNAL_CURSOR_CAP: usize = 64;

pub fn journal_watching(
    In(params): In<Option<Value>>,
    world: &mut World,
    mut cursors: Local<
        HashMap<(u64, alloc::string::String), JournalCursor>,
    >,
) -> BrpResult<Option<Value>> {
    let p: MotionGfxJournalWatchParams = parse(params)?;
    let tid = TimelineId::from_raw(p.id);

    let (curr_time, curr_track, version) = {
        let manager = world
            .get_resource::<MotionGfxManager>()
            .ok_or_else(|| not_found(p.id))?;
        let timeline = manager
            .get_timeline(&tid)
            .ok_or_else(|| not_found(p.id))?;
        (
            timeline.curr_time(),
            timeline.curr_index(),
            world
                .get_resource::<MotionGfxEditState>()
                .map(|s| s.version(&tid))
                .unwrap_or(0),
        )
    };

    if cursors.len() > JOURNAL_CURSOR_CAP {
        cursors.clear();
    }
    let key = (p.id, p.watcher.unwrap_or_default());
    let state = world.get_resource::<MotionGfxEditState>();
    let seq_now =
        state.map(|s| s.event_seq(&tid)).unwrap_or_default();

    let Some(cursor) = cursors.get_mut(&key) else {
        cursors.insert(
            key,
            JournalCursor {
                seq: seq_now,
                last_time: curr_time,
                last_track: curr_track,
            },
        );
        return Ok(Some(serde_json::json!({
            "events": [{
                "kind": "hello",
                "version": version,
                "seq": seq_now,
            }],
        })));
    };

    let mut events: Vec<Value> = Vec::new();

    if let Some(state) = state {
        if let Some(oldest) = state.oldest_event_seq(&tid)
            && cursor.seq + 1 < oldest
        {
            events.push(serde_json::json!({
                "kind": "lost",
                "missed": oldest - cursor.seq - 1,
            }));
        }
        for event in state.events_since(&tid, cursor.seq) {
            events.push(event.clone());
        }
        cursor.seq = seq_now;

        if p.marker_events {
            if curr_track == cursor.last_track
                && curr_time != cursor.last_time
                && let Some(markers) = state.markers(&tid)
            {
                let (lo, hi) = if curr_time > cursor.last_time {
                    (cursor.last_time, curr_time)
                } else {
                    (curr_time, cursor.last_time)
                };
                for (name, marker) in markers {
                    if marker.track == curr_track
                        && marker.time > lo
                        && marker.time <= hi
                    {
                        events.push(serde_json::json!({
                            "kind": "marker",
                            "name": name,
                            "time": marker.time,
                            "playhead": curr_time,
                        }));
                    }
                }
            }
            cursor.last_time = curr_time;
            cursor.last_track = curr_track;
        }
    }

    if events.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::json!({ "events": events })))
    }
}

pub fn seek_timeline(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let MotionGfxSeekParams {
        id,
        mut time,
        mut track,
        marker,
    } = parse(params)?;
    let tid = TimelineId::from_raw(id);

    if let Some(name) = marker {
        if time.is_some() || track.is_some() {
            return Err(BrpError {
                code: error_codes::INVALID_PARAMS,
                message: "`marker` is mutually exclusive with \
                          `time`/`track`"
                    .to_string(),
                data: None,
            });
        }
        let found = world
            .get_resource::<MotionGfxEditState>()
            .and_then(|s| s.markers(&tid)?.get(&name).copied())
            .ok_or_else(|| BrpError {
                code: error_codes::RESOURCE_ERROR,
                message: format!(
                    "no marker `{name}` on timeline {id}"
                ),
                data: None,
            })?;
        time = Some(found.time);
        track = Some(found.track);
    }

    {
        let mut manager = world
            .get_resource_mut::<MotionGfxManager>()
            .ok_or_else(|| not_found(id))?;
        let timeline = manager
            .get_timeline_mut(&tid)
            .ok_or_else(|| not_found(id))?;

        if let Some(track) = track {
            timeline.set_target_track(track);
        }
        if let Some(time) = time {
            timeline.set_target_time(time);
        }
    }

    snapshot_result(world, id)
}

fn play_timeline(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let MotionGfxPlayParams { id, time_scale } = parse(params)?;
    set_playing(world, id, true, time_scale)?;
    snapshot_result(world, id)
}

fn pause_timeline(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let TimelineRef { id } = parse(params)?;
    set_playing(world, id, false, None)?;
    snapshot_result(world, id)
}

fn set_time_scale(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let MotionGfxTimeScaleParams { id, time_scale } = parse(params)?;
    let entity =
        timeline_entity(world, id).ok_or_else(|| not_found(id))?;
    let mut player = world
        .get_mut::<RealtimePlayer>(entity)
        .ok_or_else(|| BrpError {
            code: error_codes::RESOURCE_ERROR,
            message: format!("Timeline {id} has no RealtimePlayer"),
            data: None,
        })?;
    player.set_time_scale(time_scale);
    snapshot_result(world, id)
}

fn remove_timeline(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let TimelineRef { id } = parse(params)?;
    let tid = TimelineId::from_raw(id);

    let removed = {
        let mut manager = world
            .get_resource_mut::<MotionGfxManager>()
            .ok_or_else(|| not_found(id))?;
        manager.remove_timeline(&tid).is_some()
    };

    if !removed {
        return Err(not_found(id));
    }

    if let Some(mut state) =
        world.get_resource_mut::<MotionGfxEditState>()
    {
        state.forget(&tid);
    }

    if let Some(entity) = timeline_entity(world, id) {
        world.despawn(entity);
    }

    ok(serde_json::json!({ "removed": id }))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MotionGfxCreateParams {
    /// Optional display name.
    name: Option<alloc::string::String>,
    /// Number of empty tracks to start with.
    tracks: Option<usize>,
    /// Spawn the transport entity (paused [`RealtimePlayer`]).
    /// Default `true`.
    player: Option<bool>,
}

pub fn timeline_create(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxCreateParams = match params {
        Some(value) => {
            serde_json::from_value(value).map_err(|e| BrpError {
                code: error_codes::INVALID_PARAMS,
                message: e.to_string(),
                data: None,
            })?
        }
        None => MotionGfxCreateParams::default(),
    };
    edit::require_manager(world)?;

    let tracks = p.tracks.unwrap_or(1);
    if !(1..=1024).contains(&tracks) {
        return Err(BrpError {
            code: error_codes::INVALID_PARAMS,
            message: "tracks must lie in 1..=1024".to_string(),
            data: None,
        });
    }

    let tid = world.resource_scope::<MotionGfxManager, TimelineId>(
        |world, mut manager| {
            let mut builder = manager.create_builder();
            builder.add_tracks((0..tracks).map(|_| {
                motiongfx::track::TrackFragment::new().compile()
            }));
            let timeline = builder.compile();
            let tid = manager.add_timeline(timeline);
            manager.load_pending_timelines(world);
            tid
        },
    );

    if p.player.unwrap_or(true) {
        world.spawn((tid, RealtimePlayer::new().with_playing(false)));
    }
    if let Some(name) = p.name.filter(|n| !n.is_empty()) {
        world
            .get_resource_or_insert_with(MotionGfxEditState::default)
            .set_name(tid, Some(name));
    }

    snapshot_result(world, tid.raw())
}

#[derive(Deserialize)]
struct MotionGfxRenameParams {
    id: u64,
    /// New display name.
    #[serde(default)]
    name: Option<alloc::string::String>,
}

pub fn timeline_rename(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let MotionGfxRenameParams { id, name } = parse(params)?;
    let tid = TimelineId::from_raw(id);
    {
        let manager = world
            .get_resource::<MotionGfxManager>()
            .ok_or_else(|| not_found(id))?;
        if manager.get_timeline(&tid).is_none() {
            return Err(not_found(id));
        }
    }
    let name = name.filter(|n| !n.is_empty());
    let mut state = world
        .get_resource_or_insert_with(MotionGfxEditState::default);
    state.set_name(tid, name.clone());
    let version = state.bump(tid);
    state.push_event(
        tid,
        serde_json::json!({
            "kind": "renamed",
            "name": name,
            "version": version,
        }),
    );
    snapshot_result(world, id)
}

fn set_playing(
    world: &mut World,
    id: u64,
    playing: bool,
    time_scale: Option<f32>,
) -> Result<(), BrpError> {
    let entity =
        timeline_entity(world, id).ok_or_else(|| not_found(id))?;
    let mut player = world
        .get_mut::<RealtimePlayer>(entity)
        .ok_or_else(|| BrpError {
            code: error_codes::RESOURCE_ERROR,
            message: format!("Timeline {id} has no RealtimePlayer"),
            data: None,
        })?;
    player.set_playing(playing);
    if let Some(ts) = time_scale {
        player.set_time_scale(ts);
    }
    Ok(())
}
