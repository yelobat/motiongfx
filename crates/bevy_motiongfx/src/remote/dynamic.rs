extern crate alloc;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use bevy_ecs::prelude::*;
use bevy_ecs::reflect::{AppTypeRegistry, ReflectComponent};
use bevy_ecs::system::In;
use bevy_platform::collections::HashMap;
use bevy_reflect::serde::TypedReflectDeserializer;
use bevy_reflect::{GetPath, PartialReflect, TypeRegistry};
use bevy_remote::{BrpError, BrpResult, error_codes};
use bevy_time::Time;
use serde::de::DeserializeSeed;
use serde::{Deserialize, Serialize};
use serde_json::Value;


/// A unique id for a dynamic animation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
pub struct DynAnimId(pub u64);

/// What a single field should do over the course of the animation.
enum FieldKind {
    /// Linear numeric tween from `from` to `to`.
    Tween { from: f64, to: f64 },
    /// Apply a reflected value once `t >= 1.0`.
    Discrete {
        applied: bool,
        to: Box<dyn PartialReflect>,
    },
}

/// One animated field of a component on an entity.
struct FieldAnim {
    /// Reflection path into the component, e.g. `translation.x`. An
    /// empty string targets the component itself.
    path: String,
    kind: FieldKind,
}

/// An animation targeting one component on one entity.
struct DynAnimation {
    entity: Entity,
    /// Fully-qualified component type path.
    component: String,
    duration: f32,
    elapsed: f32,
    playing: bool,
    fields: Vec<FieldAnim>,
}

/// Resource holding all reflection-driven animations.
#[derive(Resource, Default)]
pub struct DynAnimations {
    next: u64,
    anims: HashMap<DynAnimId, DynAnimation>,
}

impl DynAnimations {
    fn insert(&mut self, anim: DynAnimation) -> DynAnimId {
        let id = DynAnimId(self.next);
        self.next = self.next.wrapping_add(1);
        self.anims.insert(id, anim);
        id
    }
}


/// Read a numeric reflect leaf as `f64`, trying every primitive type.
fn read_number(value: &dyn PartialReflect) -> Option<f64> {
    macro_rules! try_ty {
        ($($t:ty),*) => {$(
            if let Some(v) = value.try_downcast_ref::<$t>() {
                return Some(*v as f64);
            }
        )*};
    }
    try_ty!(
        f32, f64, i8, i16, i32, i64, i128, u8, u16, u32, u64, u128
    );
    None
}

/// Write `n` into a numeric reflect leaf, matching its concrete type.
/// Returns `false` if the leaf is not a supported numeric primitive.
fn write_number(value: &mut dyn PartialReflect, n: f64) -> bool {
    macro_rules! try_ty {
        ($($t:ty),*) => {$(
            if let Some(v) = value.try_downcast_mut::<$t>() {
                *v = n as $t;
                return true;
            }
        )*};
    }
    try_ty!(
        f32, f64, i8, i16, i32, i64, i128, u8, u16, u32, u64, u128
    );
    false
}


fn err(code: i16, message: impl Into<String>) -> BrpError {
    BrpError {
        code,
        message: message.into(),
        data: None,
    }
}

fn invalid(message: impl Into<String>) -> BrpError {
    err(error_codes::INVALID_PARAMS, message)
}

fn parse<T: for<'de> Deserialize<'de>>(
    params: Option<Value>,
) -> Result<T, BrpError> {
    let value = params.ok_or_else(|| invalid("Missing params"))?;
    serde_json::from_value(value).map_err(|e| invalid(e.to_string()))
}

fn ok<T: Serialize>(value: T) -> BrpResult {
    serde_json::to_value(value)
        .map_err(|e| err(error_codes::INTERNAL_ERROR, e.to_string()))
}


#[derive(Deserialize)]
struct MotionGfxSpawnParams {
    /// Map of fully-qualified component type path -> component value.
    components: serde_json::Map<String, Value>,
}

#[derive(Serialize)]
struct MotionGfxSpawnResult {
    entity: u64,
}

/// `motiongfx.spawn` - spawn a new entity with reflected components.
/// Mirrors `bevy/spawn` but in the `motiongfx.` namespace so a client
/// can spawn a subject then animate it. Errors (spawning nothing) if
/// a component can't be deserialised or lacks `ReflectComponent`.
pub fn brp_spawn(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let MotionGfxSpawnParams { components } = parse(params)?;

    let app_registry = world.resource::<AppTypeRegistry>().clone();
    let registry = app_registry.read();

    // Deserialise everything first so a failure spawns nothing. Keep the
    // type path alongside each value for the insert pass.
    let mut reflected: Vec<(String, Box<dyn PartialReflect>)> =
        Vec::with_capacity(components.len());
    for (type_path, value) in &components {
        let registration = registry
            .get_with_type_path(type_path)
            .ok_or_else(|| {
                err(
                    error_codes::COMPONENT_ERROR,
                    format!("Unknown/unregistered type: {type_path}"),
                )
            })?;

        if registration.data::<ReflectComponent>().is_none() {
            return Err(err(
                error_codes::COMPONENT_ERROR,
                format!("`{type_path}` is not a ReflectComponent"),
            ));
        }

        let seed =
            TypedReflectDeserializer::new(registration, &registry);
        let partial = seed
            .deserialize(value)
            .map_err(|e| invalid(format!("`{type_path}`: {e}")))?;
        reflected.push((type_path.clone(), partial));
    }

    let mut entity_mut = world.spawn_empty();
    for (type_path, partial) in &reflected {
        // Presence of ReflectComponent verified above.
        let registration =
            registry.get_with_type_path(type_path).unwrap();
        let reflect_component =
            registration.data::<ReflectComponent>().unwrap();
        reflect_component.insert(
            &mut entity_mut,
            partial.as_ref(),
            &registry,
        );
    }
    let entity = entity_mut.id();

    ok(MotionGfxSpawnResult {
        entity: entity.to_bits(),
    })
}


#[derive(Deserialize)]
struct MotionGfxAnimateParams {
    /// Target entity, as returned by `motiongfx.spawn` (raw bits).
    /// Mutually exclusive with `entity_name`.
    #[serde(default)]
    entity: Option<u64>,
    /// Target entity by its unique `Name` (alternative to `entity`).
    #[serde(default)]
    entity_name: Option<String>,
    /// Fully-qualified component type path to animate.
    component: String,
    /// Animation duration in seconds.
    duration: f32,
    /// Whether to start playing immediately (default: true).
    #[serde(default = "default_true")]
    playing: bool,
    /// Fields to animate within the component.
    fields: Vec<FieldParam>,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct FieldParam {
    /// Reflection path within the component, e.g. `translation.x`.
    /// Use an empty string to target the component itself.
    path: String,
    /// Target value. A JSON number requests a numeric tween. Any other
    /// JSON value requests a discrete set at the end of the animation.
    to: Value,
}

#[derive(Serialize)]
struct MotionGfxAnimateResult {
    id: u64,
    fields: usize,
}

/// `motiongfx.animate` - add a reflection-driven animation to an
/// existing entity. Resolves and validates every field up-front. On an
/// invalid path, missing component, or non-numeric leaf it errors and
/// registers nothing.
pub fn brp_animate(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let MotionGfxAnimateParams {
        entity,
        entity_name,
        component,
        duration,
        playing,
        fields,
    } = parse(params)?;

    if duration <= 0.0 {
        return Err(invalid("duration must be > 0"));
    }
    if fields.is_empty() {
        return Err(invalid("no fields to animate"));
    }

    let entity =
        Entity::from_bits(super::edit::resolve_subject_entity(
            world,
            entity,
            entity_name.as_deref(),
        )?);

    let app_registry = world.resource::<AppTypeRegistry>().clone();
    let registry = app_registry.read();

    let registration =
        registry.get_with_type_path(&component).ok_or_else(|| {
            err(
                error_codes::COMPONENT_ERROR,
                format!("Unknown/unregistered type: {component}"),
            )
        })?;
    let reflect_component =
        registration.data::<ReflectComponent>().ok_or_else(|| {
            err(
                error_codes::COMPONENT_ERROR,
                format!("`{component}` is not a ReflectComponent"),
            )
        })?;

    // Make sure the entity exists before touching it.
    world.get_entity(entity).map_err(|_| {
        err(
            error_codes::ENTITY_NOT_FOUND,
            format!("No such entity: {}", entity.to_bits()),
        )
    })?;

    // A reflected handle is enough to read the current ("from")
    // values. This is the same call `bevy/mutate_components` uses.
    let reflected = reflect_component
        .reflect_mut(world.entity_mut(entity))
        .ok_or_else(|| {
            err(
                error_codes::COMPONENT_NOT_PRESENT,
                format!(
                    "Entity {} has no `{component}`",
                    entity.to_bits()
                ),
            )
        })?;

    let mut field_anims: Vec<FieldAnim> =
        Vec::with_capacity(fields.len());
    for FieldParam { path, to } in &fields {
        let leaf = reflected
            .reflect_path(path.as_str())
            .map_err(|e| invalid(format!("path `{path}`: {e}")))?;

        let kind = if let Some(target) = to.as_f64() {
            // Numeric tween: leaf must be numeric.
            let from = read_number(leaf).ok_or_else(|| {
                invalid(format!(
                    "field `{path}` is not numeric; cannot tween"
                ))
            })?;
            FieldKind::Tween { from, to: target }
        } else {
            // Discrete set: deserialise `to` against the leaf's type.
            let leaf_type = leaf.reflect_type_path();
            let leaf_reg = registry
                .get_with_type_path(leaf_type)
                .ok_or_else(|| {
                    err(
                        error_codes::COMPONENT_ERROR,
                        format!(
                            "field `{path}` type `{leaf_type}` not registered"
                        ),
                    )
                })?;
            let seed =
                TypedReflectDeserializer::new(leaf_reg, &registry);
            let value = seed.deserialize(to).map_err(|e| {
                invalid(format!("field `{path}`: {e}"))
            })?;
            FieldKind::Discrete {
                applied: false,
                to: value,
            }
        };

        field_anims.push(FieldAnim {
            path: path.clone(),
            kind,
        });
    }

    // `reflected` (the component borrow) and `registry` are not used past
    // this point, so NLL ends their borrows here, freeing `world` for the
    // animations resource below.
    let count = field_anims.len();
    let mut anims = world.resource_mut::<DynAnimations>();
    let id = anims.insert(DynAnimation {
        entity,
        component,
        duration,
        elapsed: 0.0,
        playing,
        fields: field_anims,
    });

    ok(MotionGfxAnimateResult {
        id: id.0,
        fields: count,
    })
}


#[derive(Serialize)]
struct DynAnimState {
    id: u64,
    entity: u64,
    component: String,
    duration: f32,
    elapsed: f32,
    playing: bool,
    fields: usize,
}

/// `motiongfx.list_animations` - summarise all dynamic animations.
pub fn brp_list_animations(
    In(_): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let anims = world.resource::<DynAnimations>();
    let states: Vec<DynAnimState> = anims
        .anims
        .iter()
        .map(|(id, a)| DynAnimState {
            id: id.0,
            entity: a.entity.to_bits(),
            component: a.component.clone(),
            duration: a.duration,
            elapsed: a.elapsed,
            playing: a.playing,
            fields: a.fields.len(),
        })
        .collect();
    ok(states)
}


#[derive(Deserialize)]
struct MotionGfxRemoveAnimParams {
    id: u64,
}

/// `motiongfx.remove_animation` - drop a dynamic animation.
pub fn brp_remove_animation(
    In(params): In<Option<Value>>,
    world: &mut World,
) -> BrpResult {
    let MotionGfxRemoveAnimParams { id } = parse(params)?;
    let mut anims = world.resource_mut::<DynAnimations>();
    if anims.anims.remove(&DynAnimId(id)).is_none() {
        return Err(err(
            error_codes::RESOURCE_ERROR,
            format!("No animation with id {id}"),
        ));
    }
    ok(serde_json::json!({ "removed": id }))
}


/// Advances and applies all dynamic animations each frame.
///
/// Runs with exclusive world access so it can clone the type registry,
/// resolve `ReflectComponent`s and mutate arbitrary components through
/// reflection.
pub fn apply_dyn_animations(world: &mut World) {
    let delta = world.resource::<Time>().delta_secs();

    // Take the animations out so we don't hold a resource borrow while
    // mutating components through the world.
    let mut anims = {
        let mut store = world.resource_mut::<DynAnimations>();
        if store.anims.is_empty() {
            return;
        }
        core::mem::take(&mut store.anims)
    };

    let app_registry = world.resource::<AppTypeRegistry>().clone();
    let registry = app_registry.read();

    for anim in anims.values_mut() {
        if anim.playing {
            anim.elapsed = (anim.elapsed + delta).min(anim.duration);
            if anim.elapsed >= anim.duration {
                anim.playing = false;
            }
        }
        apply_one(anim, &registry, world);
    }

    drop(registry);

    // Put the (mutated) animations back, keeping any registered while we
    // were running.
    let mut store = world.resource_mut::<DynAnimations>();
    if store.anims.is_empty() {
        store.anims = anims;
    } else {
        store.anims.extend(anims);
    }
}

fn apply_one(
    anim: &mut DynAnimation,
    registry: &TypeRegistry,
    world: &mut World,
) {
    let Some(registration) =
        registry.get_with_type_path(&anim.component)
    else {
        return;
    };
    let Some(reflect_component) =
        registration.data::<ReflectComponent>()
    else {
        return;
    };

    if world.get_entity(anim.entity).is_err() {
        // Entity gone: stop driving it.
        anim.playing = false;
        return;
    }

    let Some(mut reflected) =
        reflect_component.reflect_mut(world.entity_mut(anim.entity))
    else {
        return;
    };

    let t = (anim.elapsed / anim.duration).clamp(0.0, 1.0) as f64;

    for field in anim.fields.iter_mut() {
        let Ok(leaf) =
            reflected.reflect_path_mut(field.path.as_str())
        else {
            continue;
        };

        match &mut field.kind {
            FieldKind::Tween { from, to } => {
                let v = *from + (*to - *from) * t;
                let _ = write_number(leaf, v);
            }
            FieldKind::Discrete { applied, to } => {
                if t >= 1.0 && !*applied {
                    let _ = leaf.try_apply(to.as_ref());
                    *applied = true;
                }
            }
        }
    }
}
