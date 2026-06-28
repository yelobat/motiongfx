extern crate alloc;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use motiongfx::action::{Ease, EaseFn};
use motiongfx::remote::{RemoteActionError, RemoteEditError};

use bevy_ecs::prelude::*;
use bevy_remote::{BrpError, BrpResult, error_codes};

use super::batch::EditOp;
use super::catalog::{CatalogError, MotionGfxCatalog, SubjectKind};
use crate::manager::{MotionGfxManager, TimelineId};

pub(crate) fn err(code: i16, message: impl Into<String>) -> BrpError {
    BrpError {
        code,
        message: message.into(),
        data: None,
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> BrpError {
    err(error_codes::INVALID_PARAMS, message)
}

pub(crate) fn parse<T: for<'de> Deserialize<'de>>(
    params: Option<Value>,
) -> Result<T, BrpError> {
    let value = params.ok_or_else(|| invalid("Missing params"))?;
    serde_json::from_value(value).map_err(|e| invalid(e.to_string()))
}

pub(crate) fn ok<T: Serialize>(value: T) -> BrpResult {
    serde_json::to_value(value)
        .map_err(|e| err(error_codes::INTERNAL_ERROR, e.to_string()))
}

pub(crate) fn map_catalog_err(e: CatalogError) -> BrpError {
    match e {
        CatalogError::TypeNotRegistered => err(
            error_codes::COMPONENT_ERROR,
            "target type is not in the type registry",
        ),
        CatalogError::Deserialize(m) => {
            invalid(format!("could not deserialize `to`: {m}"))
        }
        CatalogError::FromReflect => {
            invalid("`to` value did not match the target field type")
        }
    }
}

/// The name easing functions.
pub(crate) const EASE_TABLE: &[(&str, EaseFn)] = {
    use motiongfx::ease::*;
    &[
        ("linear", linear as EaseFn),
        ("sine_in", sine::ease_in as EaseFn),
        ("sine_out", sine::ease_out as EaseFn),
        ("sine_in_out", sine::ease_in_out as EaseFn),
        ("quad_in", quad::ease_in as EaseFn),
        ("quad_out", quad::ease_out as EaseFn),
        ("quad_in_out", quad::ease_in_out as EaseFn),
        ("cubic_in", cubic::ease_in as EaseFn),
        ("cubic_out", cubic::ease_out as EaseFn),
        ("cubic_in_out", cubic::ease_in_out as EaseFn),
        ("quart_in", quart::ease_in as EaseFn),
        ("quart_out", quart::ease_out as EaseFn),
        ("quart_in_out", quart::ease_in_out as EaseFn),
        ("quint_in", quint::ease_in as EaseFn),
        ("quint_out", quint::ease_out as EaseFn),
        ("quint_in_out", quint::ease_in_out as EaseFn),
        ("expo_in", expo::ease_in as EaseFn),
        ("expo_out", expo::ease_out as EaseFn),
        ("expo_in_out", expo::ease_in_out as EaseFn),
        ("circ_in", circ::ease_in as EaseFn),
        ("circ_out", circ::ease_out as EaseFn),
        ("circ_in_out", circ::ease_in_out as EaseFn),
        ("back_in", back::ease_in as EaseFn),
        ("back_out", back::ease_out as EaseFn),
        ("back_in_out", back::ease_in_out as EaseFn),
        ("elastic_in", elastic::ease_in as EaseFn),
        ("elastic_out", elastic::ease_out as EaseFn),
        ("elastic_in_out", elastic::ease_in_out as EaseFn),
    ]
};

/// The name of a built-in easing function, if `f` matches
/// an [`EASE_TABLE`] entry by pointer.
pub fn ease_name(f: EaseFn) -> Option<&'static str> {
    EASE_TABLE
        .iter()
        .find(|(_, table_fn)| core::ptr::fn_addr_eq(*table_fn, f))
        .map(|(name, _)| *name)
}

/// The [`EaseFn`] for a named built-in ease.
pub fn ease_fn_from_name(name: &str) -> Option<EaseFn> {
    EASE_TABLE.iter().find(|(n, _)| *n == name).map(|(_, f)| *f)
}

/// Resolve a named easing function to its [`EaseFn`].
fn parse_ease(name: &str) -> Result<EaseFn, BrpError> {
    ease_fn_from_name(name)
        .ok_or_else(|| invalid(format!("unknown ease `{name}`")))
}

/// An `ease` request parameter: either a name or a parameterised
/// curve in the form `{"cubic_bezier": [x1, y1, x2, y2]}`.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum EaseParam {
    Named(String),
    Curve { cubic_bezier: [f32; 4] },
}

pub(crate) fn ease_to_json(ease: Ease) -> Value {
    match ease {
        Ease::Fn(f) => Value::String(
            ease_name(f).unwrap_or("custom").to_string(),
        ),
        Ease::CubicBezier(points) => {
            serde_json::json!({ "cubic_bezier": points })
        }
    }
}

/// Resolve an [`EaseParam`] to an [`Ease`].
pub(crate) fn resolve_ease(p: &EaseParam) -> Result<Ease, BrpError> {
    match p {
        EaseParam::Named(name) => parse_ease(name).map(Ease::Fn),
        EaseParam::Curve { cubic_bezier } => {
            let [x1, _, x2, _] = *cubic_bezier;
            if !(0.0..=1.0).contains(&x1)
                || !(0.0..=1.0).contains(&x2)
            {
                return Err(invalid(
                    "cubic_bezier x1/x2 must lie in [0,1]",
                ));
            }
            Ok(Ease::CubicBezier(*cubic_bezier))
        }
    }
}

fn map_live_err(e: RemoteActionError) -> BrpError {
    match e {
        RemoteActionError::Unregistered => err(
            error_codes::COMPONENT_ERROR,
            "field is not registered for remote editing",
        ),
        RemoteActionError::TypeMismatch => {
            invalid("subject or target type mismatch")
        }
        RemoteActionError::InvalidKeyframes(reason) => {
            invalid(format!("invalid keyframes: {reason}"))
        }
    }
}

pub(crate) fn map_edit_err(e: RemoteEditError) -> BrpError {
    match e {
        RemoteEditError::Action(e) => map_live_err(e),
        RemoteEditError::Overlap { conflict } => BrpError {
            code: error_codes::INVALID_PARAMS,
            message: format!(
                "clip would overlap action {}",
                conflict.to_bits()
            ),
            data: Some(serde_json::json!({
                "conflict_action_id": conflict.to_bits(),
            })),
        },
        RemoteEditError::NotFound => err(
            error_codes::RESOURCE_ERROR,
            "no action with that id on the given track",
        ),
        RemoteEditError::TrackOutOfRange => err(
            error_codes::RESOURCE_ERROR,
            "track index out of range",
        ),
    }
}

pub(crate) fn resolve_subject_entity(
    world: &mut World,
    entity: Option<u64>,
    entity_name: Option<&str>,
) -> Result<u64, BrpError> {
    match (entity, entity_name) {
        (Some(bits), None) => Ok(bits),
        (Some(_), Some(_)) => Err(invalid(
            "`entity` and `entity_name` are mutually exclusive",
        )),
        (None, None) => Err(invalid(
            "one of `entity` or `entity_name` is required",
        )),
        (None, Some(name)) => {
            let mut q = world.query::<(Entity, &Name)>();
            let matches: Vec<u64> = q
                .iter(world)
                .filter(|(_, n)| n.as_str() == name)
                .map(|(e, _)| e.to_bits())
                .collect();
            match matches.as_slice() {
                [bits] => Ok(*bits),
                [] => Err(err(
                    error_codes::ENTITY_NOT_FOUND,
                    format!("no entity named `{name}`"),
                )),
                _ => Err(BrpError {
                    code: error_codes::INVALID_PARAMS,
                    message: format!(
                        "{} entities are named `{name}`. \
                    use raw `entity` bits instead.",
                        matches.len()
                    ),
                    data: Some(serde_json::json!({
                        "matches": matches,
                    })),
                }),
            }
        }
    }
}

pub(crate) fn resolve_entity_name_in_op(
    world: &mut World,
    op: &mut Value,
) -> Result<(), BrpError> {
    let has_name = op
        .as_object()
        .is_some_and(|o| o.contains_key("entity_name"));
    if !has_name {
        return Ok(());
    }

    let obj = op.as_object_mut().unwrap();
    if obj.contains_key("entity") {
        return Err(invalid(
            "`entity` and `entity_name` are mutually exclusive",
        ));
    }

    let name = obj
        .get("entity_name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("`entity_name` must be a string"))?
        .to_string();
    let bits = resolve_subject_entity(world, None, Some(&name))?;
    let obj = op.as_object_mut().unwrap();
    obj.remove("entity_name");
    obj.insert("entity".to_string(), bits.into());
    Ok(())
}

/// Whether `(component, field)` is registered as a resource.
pub(crate) fn is_resource_field(
    world: &World,
    component: &str,
    field: &str,
) -> bool {
    world
        .get_resource::<MotionGfxCatalog>()
        .and_then(|c| c.get(component, field))
        .is_some_and(|e| matches!(e.subject, SubjectKind::Resource))
}

pub(crate) fn require_manager(world: &World) -> Result<(), BrpError> {
    if !world.contains_resource::<MotionGfxManager>() {
        return Err(err(
            error_codes::RESOURCE_ERROR,
            "MotionGfxManager resource missing: add BevyMotionGfxPlugin",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct AnimatableParams {
    /// Optional component type-path filter.
    #[serde(default)]
    component: Option<String>,
}

#[derive(Serialize)]
struct AnimatableField {
    component: String,
    field: String,
    target_type: String,
    subject: &'static str,
}

#[cfg(feature = "asset")]
pub(crate) fn resolve_asset_of(
    world: &World,
    entity: Entity,
    component_path: &str,
) -> Result<bevy_asset::UntypedAssetId, BrpError> {
    use bevy_ecs::reflect::AppTypeRegistry;
    use bevy_reflect::{PartialReflect, ReflectRef};

    let app_registry = world.resource::<AppTypeRegistry>().clone();
    let registry = app_registry.read();
    let registration = registry
        .get_with_type_path(component_path)
        .ok_or_else(|| {
            err(
                error_codes::COMPONENT_ERROR,
                format!(
                    "`{component_path}` is not a registered type"
                ),
            )
        })?;
    let reflect_component = registration
        .data::<bevy_ecs::reflect::ReflectComponent>()
        .ok_or_else(|| {
            err(
                error_codes::COMPONENT_ERROR,
                format!("`{component_path}` is not a component"),
            )
        })?;
    let entity_ref = world.get_entity(entity).map_err(|_| {
        err(
            error_codes::ENTITY_NOT_FOUND,
            format!("entity {} does not exist", entity.to_bits()),
        )
    })?;
    let reflected =
        reflect_component.reflect(entity_ref).ok_or_else(|| {
            err(
                error_codes::COMPONENT_ERROR,
                format!(
                    "entity {} has no `{component_path}`",
                    entity.to_bits()
                ),
            )
        })?;

    let fields: Vec<&dyn PartialReflect> =
        match reflected.reflect_ref() {
            ReflectRef::Struct(s) => s.iter_fields().collect(),
            ReflectRef::TupleStruct(t) => t.iter_fields().collect(),
            _ => Vec::new(),
        };
    for candidate in fields
        .into_iter()
        .chain(core::iter::once(reflected.as_partial_reflect()))
    {
        let Some(any) =
            candidate.try_as_reflect().map(|r| r.as_any())
        else {
            continue;
        };
        if let Some(handle) = registry
            .get_type_data::<bevy_asset::ReflectHandle>(any.type_id())
            .and_then(|rh| rh.downcast_handle_untyped(any))
        {
            return Ok(handle.id());
        }
    }
    Err(err(
        error_codes::COMPONENT_ERROR,
        format!(
            "no `Handle<_>` found in `{component_path}` on entity\
{} (is the handle type reflect-registered?)",
            entity.to_bits()
        ),
    ))
}

/// List editable `(component, field)`s.
pub fn animatable_fields(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let filter = match params {
        Some(value) => {
            let p: AnimatableParams =
                serde_json::from_value(value)
                    .map_err(|e| invalid(e.to_string()))?;
            p.component
        }
        None => None,
    };

    let catalog = world.get_resource::<MotionGfxCatalog>()
        .ok_or_else(|| {
            err(error_codes::RESOURCE_ERROR, "MotionGfxCatalog resource missing: add MotionGfxRemotePlugin")
        })?;

    let fields: Vec<AnimatableField> = catalog
        .iter()
        .filter(|((component, _), _)| {
            filter.as_ref().is_none_or(|c| c == component)
        })
        .map(|((component, field), entry)| AnimatableField {
            component: component.clone(),
            field: field.clone(),
            target_type: entry.target_type_path.clone(),
            subject: match entry.subject {
                SubjectKind::Component => "component",
                SubjectKind::Asset => "asset",
                SubjectKind::Resource => "resource",
            },
        })
        .collect();

    ok(fields)
}

#[derive(Deserialize)]
struct MotionGfxInsertParams {
    /// Raw [`TimelineId`].
    id: u64,
    /// Track index within the timeline.
    track: usize,
    /// Target entity (raw bits).
    #[serde(default)]
    entity: Option<u64>,
    /// Target entity by its unique [`Name`].
    #[serde(default)]
    entity_name: Option<String>,
    /// Fully-qualified component type path.
    component: String,
    /// Reflection field path within the component.
    field: String,
    /// Asset addressing: the type path of the handle-bearing component
    #[serde(default)]
    asset_of: Option<String>,
    /// Target value the field should animate to.
    #[serde(default)]
    to: Value,
    /// Relative target: animate to `base + to_relative`.
    #[serde(default)]
    to_relative: Option<f64>,
    /// Duration of the tween in seconds.
    duration: f32,
    /// Optional earliest start time (default `0.0`).
    #[serde(default)]
    start_at: Option<f32>,
    /// Optional easing applied, defaults to linear.
    #[serde(default)]
    ease: Option<Value>,
    /// Optional display label.
    #[serde(default)]
    label: Option<String>,
    /// Optional display colour `[r, g, b]` (0..=255).
    #[serde(default)]
    color: Option<[u8; 3]>,
    /// Whether this clip is enabled or not.
    #[serde(default)]
    enabled: Option<bool>,
}

pub fn timeline_insert_action(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxInsertParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);
    let entity = if is_resource_field(world, &p.component, &p.field) {
        0
    } else {
        resolve_subject_entity(
            world,
            p.entity,
            p.entity_name.as_deref(),
        )?
    };

    super::batch::single_edit(
        world,
        tid,
        EditOp::Insert {
            track: p.track,
            entity,
            component: p.component,
            field: p.field,
            asset_of: p.asset_of,
            to: p.to,
            to_relative: p.to_relative,
            duration: p.duration,
            start_at: p.start_at,
            ease: p.ease,
            label: p.label,
            color: p.color,
            enabled: p.enabled,
            exact: false,
            restore_id: None,
        },
    )
}

#[derive(Deserialize)]
struct MotionGfxInsertKeyframesParams {
    id: u64,
    track: usize,
    /// Raw entity bits.
    #[serde(default)]
    entity: Option<u64>,
    /// Target entity by its unique [`Name`].
    #[serde(default)]
    entity_name: Option<String>,
    component: String,
    field: String,
    #[serde(default)]
    asset_of: Option<String>,
    keyframes: Vec<super::batch::KeyframeDoc>,
    duration: f32,
    #[serde(default)]
    start_at: Option<f32>,
    #[serde(default)]
    ease: Option<Value>,
    #[serde(default)]
    smooth: Option<bool>,
    /// Optional display label.
    #[serde(default)]
    label: Option<String>,
    /// Optional display colour `[r, g, b]` (0..=255).
    #[serde(default)]
    color: Option<[u8; 3]>,
    /// Whether this clip is enabled or not.
    #[serde(default)]
    enabled: Option<bool>,
}

pub fn timeline_insert_keyframes(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let mut p: MotionGfxInsertKeyframesParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);
    let entity = if is_resource_field(world, &p.component, &p.field) {
        0
    } else {
        resolve_subject_entity(
            world,
            p.entity,
            p.entity_name.as_deref(),
        )?
    };

    if p.smooth == Some(true) {
        super::batch::smooth_keyframes(&mut p.keyframes)?;
    }

    super::batch::single_edit(
        world,
        tid,
        EditOp::InsertKeyframes {
            track: p.track,
            entity,
            component: p.component,
            field: p.field,
            asset_of: p.asset_of,
            keyframes: p.keyframes,
            duration: p.duration,
            start_at: p.start_at,
            ease: p.ease,
            label: p.label,
            color: p.color,
            enabled: p.enabled,
            exact: false,
            restore_id: None,
        },
    )
}

#[derive(Deserialize)]
struct MotionGfxRemoveParams {
    id: u64,
    track: usize,
    action_id: u64,
}

pub fn timeline_remove_action(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxRemoveParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    super::batch::single_edit(
        world,
        tid,
        EditOp::Remove {
            track: p.track,
            action_id: p.action_id,
        },
    )
}

#[derive(Deserialize)]
struct MotionGfxMoveParams {
    id: u64,
    track: usize,
    action_id: u64,
    /// New start time for the action.
    start_at: f32,
    /// Optional new duration. Keeps the existing one if omitted.
    #[serde(default)]
    duration: Option<f32>,
}

/// Reschedule an action in place (same id).
pub fn timeline_move_action(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxMoveParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    super::batch::single_edit(
        world,
        tid,
        EditOp::Move {
            track: p.track,
            action_id: p.action_id,
            start_at: p.start_at,
            duration: p.duration,
        },
    )
}

#[derive(Deserialize)]
struct MotionGfxUpdateParams {
    id: u64,
    action_id: u64,
    /// New target value. Updating a closure-authored action flattens it
    /// to a constant tween. Rejected on keyframed actions.
    #[serde(default)]
    to: Option<Value>,
    /// Relative retarget: new target = current baked end + delta.
    #[serde(default)]
    to_relative: Option<f64>,
    /// New easing. `"linear"` clears it.
    #[serde(default)]
    ease: Option<Value>,
    /// Replace a keyframed action's points in place.
    #[serde(default)]
    keyframes: Option<Vec<super::batch::KeyframeDoc>>,
    /// With `keyframes`: auto-tangent the segment eases.
    #[serde(default)]
    smooth: Option<bool>,
    /// Set (string) or clear (`null`) the label. Absent = unchanged.
    #[serde(default, deserialize_with = "super::batch::double_option")]
    label: Option<Option<String>>,
    /// Set (`[r, g, b]`) or clear (`null`) the colour. Absent =
    /// unchanged.
    #[serde(default, deserialize_with = "super::batch::double_option")]
    color: Option<Option<[u8; 3]>>,
    /// Mute (`false`) / unmute (`true`). Absent = unchanged.
    #[serde(default)]
    enabled: Option<bool>,
}

/// Tweak a clip's value and/or easing in place. The [`ActionId`] and
/// clip graph stay put, so only a re-bake happens.
pub fn timeline_update_action(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let mut p: MotionGfxUpdateParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);
    if p.smooth == Some(true) {
        match p.keyframes.as_mut() {
            Some(keyframes) => {
                super::batch::smooth_keyframes(keyframes)?;
            }
            None => {
                return Err(invalid("`smooth` requires `keyframes`"));
            }
        }
    }

    super::batch::single_edit(
        world,
        tid,
        EditOp::Update {
            action_id: p.action_id,
            to: p.to,
            to_relative: p.to_relative,
            ease: p.ease,
            keyframes: p.keyframes,
            label: p.label,
            color: p.color,
            enabled: p.enabled,
        },
    )
}

fn default_track_add_count() -> usize {
    1
}

#[derive(Deserialize)]
struct MotionGfxTrackAddParams {
    id: u64,
    /// How many empty tracks to append (default 1).
    #[serde(default = "default_track_add_count")]
    count: usize,
}

/// Append empty tracks. Version-bumped but not journaled: an empty
/// track is inert, nothing for undo to restore.
pub fn timeline_track_add(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxTrackAddParams = parse(params)?;
    require_manager(world)?;
    if !(1..=1024).contains(&p.count) {
        return Err(invalid("count must lie in 1..=1024"));
    }
    let tid = TimelineId::from_raw(p.id);

    let track_count = {
        let mut manager = world.resource_mut::<MotionGfxManager>();
        let timeline =
            manager.get_timeline_mut(&tid).ok_or_else(|| {
                err(
                    error_codes::RESOURCE_ERROR,
                    format!("No timeline with id {}", p.id),
                )
            })?;
        let target = timeline.tracks().len() + p.count;
        timeline.ensure_track_count(target);
        timeline.tracks().len()
    };

    let mut state = world.get_resource_or_insert_with(
        super::state::MotionGfxEditState::default,
    );
    let version = state.bump(tid);
    state.push_event(
        tid,
        serde_json::json!({
            "kind": "track_add",
            "track_count": track_count,
            "version": version,
        }),
    );
    ok(serde_json::json!({
        "track_count": track_count,
        "version": version,
    }))
}

#[derive(Deserialize)]
struct MotionGfxGcParams {
    id: u64,
    /// Report what would be removed without changing anything.
    #[serde(default)]
    dry_run: bool,
}

/// Find (and, unless `dry_run`, remove) clips whose subject entity was
/// despawned. Removal journals as one entry, so a gc is one undo away.
pub fn timeline_gc(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    use core::any::TypeId;

    let p: MotionGfxGcParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, mut manager| {
            let mut dangling: Vec<(usize, u64)> = Vec::new();
            {
                let timeline =
                    manager.get_timeline(&tid).ok_or_else(|| {
                        err(
                            error_codes::RESOURCE_ERROR,
                            format!("No timeline with id {}", p.id),
                        )
                    })?;
                let aw = timeline.action_world();
                for (t, track) in timeline.tracks().iter().enumerate()
                {
                    for (key, span) in track.sequences_spans() {
                        let entity = (key.subject_id().type_id()
                            == TypeId::of::<Entity>())
                        .then(|| {
                            aw.get_id::<Entity>(
                                &key.subject_id().uid(),
                            )
                            .copied()
                        })
                        .flatten();
                        let Some(entity) = entity else {
                            continue;
                        };
                        if world.get_entity(entity).is_ok() {
                            continue;
                        }
                        for clip in track.clips(*span) {
                            dangling.push((t, clip.id.to_bits()));
                        }
                    }
                }
            }

            let ids: Vec<u64> =
                dangling.iter().map(|(_, id)| *id).collect();
            if p.dry_run || dangling.is_empty() {
                return ok(serde_json::json!({
                    "dangling": ids,
                    "removed": false,
                }));
            }

            // Remove as one journaled entry. Unwind on failure.
            let mut forward = Vec::with_capacity(dangling.len());
            let mut inverses = Vec::with_capacity(dangling.len());
            for (track, action_id) in &dangling {
                let op = EditOp::Remove {
                    track: *track,
                    action_id: *action_id,
                };
                match super::batch::apply_op(
                    world,
                    &mut manager,
                    tid,
                    &op,
                ) {
                    Ok((_, inverse)) => {
                        forward.push(op);
                        inverses.push(inverse);
                    }
                    Err(e) => {
                        for inv in inverses.iter().rev() {
                            let _ = super::batch::apply_op(
                                world,
                                &mut manager,
                                tid,
                                inv,
                            );
                        }
                        return Err(e);
                    }
                }
            }
            inverses.reverse();
            let version = super::batch::finish_edit(
                world,
                &mut manager,
                tid,
                forward,
                inverses,
            );
            ok(serde_json::json!({
                "dangling": ids,
                "removed": true,
                "version": version,
            }))
        },
    )
}

#[derive(Deserialize)]
struct MotionGfxClearParams {
    id: u64,
}

/// Drop every action, resetting the timeline to a single empty track
/// (it keeps its id). Journals as one coarse entry whose inverse
/// re-inserts every clip the catalog can describe.
pub fn timeline_clear(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let p: MotionGfxClearParams = parse(params)?;
    require_manager(world)?;
    let tid = TimelineId::from_raw(p.id);

    world.resource_scope::<MotionGfxManager, BrpResult>(
        |world, mut manager| {
            // Snapshot before clearing: the inverse re-inserts the
            // whole timeline.
            let inverse = super::batch::snapshot_all_clips(
                world,
                &mut manager,
                tid,
            )?;
            let (mut result, _) = super::batch::apply_op(
                world,
                &mut manager,
                tid,
                &EditOp::Clear {},
            )?;
            // Markers are navigation, not content. Clear them too.
            if let Some(mut state) = world
                .get_resource_mut::<super::state::MotionGfxEditState>()
            {
                state.markers_mut(tid).clear();
            }
            let version = super::batch::finish_edit(
                world,
                &mut manager,
                tid,
                alloc::vec![EditOp::Clear {}],
                inverse,
            );
            result["version"] = version.into();
            Ok(result)
        },
    )
}
