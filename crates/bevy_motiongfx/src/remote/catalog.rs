extern crate alloc;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::TypeId;

use serde::de::DeserializeSeed;
use serde_json::Value;

use bevy_app::prelude::*;
use bevy_ecs::component::Mutable;
use bevy_ecs::prelude::*;
use bevy_platform::collections::HashMap;

use motiongfx::ThreadSafe;
use motiongfx::action::{ActionId, ActionWorld, Ease};
use motiongfx::field_path::field::UntypedField;
use motiongfx::field_path::field_accessor::FieldAccessor;
use motiongfx::interpolation::Interpolation;
use motiongfx::remote::{RemoteFieldKey, RemoteTarget};

use bevy_reflect::serde::{
    TypedReflectDeserializer, TypedReflectSerializer,
};
use bevy_reflect::{
    FromReflect, GetTypeRegistration, TypePath, TypeRegistry,
};
use motiongfx::pipeline::PipelineKey;

use crate::manager::MotionGfxManager;
use crate::world::BevyWorld;

/// Converts a JSON value into a [`RemoteTarget`].
pub type DeserializeFn = Box<
    dyn Fn(&Value, &TypeRegistry) -> Result<RemoteTarget, CatalogError>
        + Send
        + Sync,
>;

/// Converts a [`RemoteTarget`] into JSON.
pub type SerializeFn = Box<
    dyn Fn(
            &ActionWorld,
            ActionId,
            &TypeRegistry,
        ) -> Option<(Value, Value)>
        + Send
        + Sync,
>;

/// Keyframe serialization function.
pub type SerializeKeyframesFn = Box<
    dyn Fn(
            &ActionWorld,
            ActionId,
            &TypeRegistry,
        ) -> Option<Vec<(f32, Value, Option<Ease>, bool)>>
        + Send
        + Sync,
>;

/// Failure modes when bridging a JSON value into a [`RemoteTarget`].
#[derive(Debug)]
pub enum CatalogError {
    /// The target type `T` is not in the [`TypeRegistry`].
    TypeNotRegistered,
    /// The JSON value did not deserialize against `T`'s reflected type.
    Deserialize(String),
    /// The reflected value could not be turned back into a concrete `T`.
    FromReflect,
}

/// What kind of subject a catalog entry animates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectKind {
    /// A component on an entity (`I = Entity`).
    Component,
    /// An asset (`I = UntypedAssetId`).
    Asset,
    /// A [`Resource`] (`I = ResourceSubject`).
    Resource,
}

/// One animatable field
pub struct CatalogEntry {
    /// The four `TypeId`s identifying the pipeline `(W, I, S, T)`.
    pub pipeline: PipelineKey,
    /// The field inside the component.
    pub field: UntypedField,
    /// `T`s reflected type path, surfaced by `animatable_fields`.
    pub target_type_path: String,
    /// Weather `S` is a component or an asset.
    pub subject: SubjectKind,
    /// Turns the request's JSON value into a typed [`RemoteTarget`].
    pub deserialize: DeserializeFn,
    /// Reads an action's baked segment back out as JSON.
    pub serialize: SerializeFn,
    /// Reads a keyframed action's points back out as JSON.
    pub serialize_keyframes: SerializeKeyframesFn,
}

impl CatalogEntry {
    /// The [`RemoteFieldKey`] handed to
    /// [`Timeline::insert_constant_action`].
    #[inline]
    pub fn key(&self) -> RemoteFieldKey {
        RemoteFieldKey::new(self.pipeline, self.field)
    }
}

/// Maps `(component_type_path, reflection_field_path)` to the
/// knowledge needed to remote-edit that field.
#[derive(Resource, Default)]
pub struct MotionGfxCatalog {
    entries: HashMap<(String, String), CatalogEntry>,
    by_field: HashMap<UntypedField, (String, String)>,
}

impl MotionGfxCatalog {
    /// Record a catalog entry for component `S`, field `T`.
    pub fn insert<S, T>(
        &mut self,
        pipeline: PipelineKey,
        field: UntypedField,
    ) where
        S: TypePath,
        T: FromReflect + TypePath + ThreadSafe,
    {
        self.insert_with_subject::<S, T>(
            pipeline,
            field,
            SubjectKind::Component,
        );
    }

    /// Record a catalog entry for `S`, field `T` with an explicit
    /// [`SubjectKind`]
    pub fn insert_with_subject<S, T>(
        &mut self,
        pipeline: PipelineKey,
        field: UntypedField,
        subject: SubjectKind,
    ) where
        S: TypePath,
        T: FromReflect + TypePath + ThreadSafe,
    {
        let component = S::type_path().to_string();
        let reflect_path =
            motiongfx_path_to_reflect(field.field_path());

        self.by_field
            .insert(field, (component.clone(), reflect_path.clone()));
        self.entries.insert(
            (component, reflect_path),
            CatalogEntry {
                pipeline,
                field,
                target_type_path: T::type_path().to_string(),
                subject,
                deserialize: make_deserialize::<T>(),
                serialize: make_serialize::<T>(),
                serialize_keyframes: make_serialize_keyframes::<T>(),
            },
        );
    }

    /// Look up the entry for a `(component, reflection field path)` pair.
    pub fn get(
        &self,
        component: &str,
        field: &str,
    ) -> Option<&CatalogEntry> {
        self.entries
            .get(&(component.to_string(), field.to_string()))
    }

    /// The `(component, field)` strings for a [`UntypedField`].
    pub fn field_names(
        &self,
        field: &UntypedField,
    ) -> Option<&(String, String)> {
        self.by_field.get(field)
    }

    /// Look up an entry directly from an [`UntypedField`].
    pub fn get_by_field(
        &self,
        field: &UntypedField,
    ) -> Option<(&(String, String), &CatalogEntry)> {
        let names = self.by_field.get(field)?;
        self.entries.get(names).map(|entry| (names, entry))
    }

    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (&(String, String), &CatalogEntry)>
    {
        self.entries.iter()
    }
}

/// Build the JSON to [`RemoteTarget`] decoder for `T`.
fn make_deserialize<T>() -> DeserializeFn
where
    T: FromReflect + TypePath + ThreadSafe,
{
    Box::new(|value: &Value, registry: &TypeRegistry| {
        let registration = registry
            .get(TypeId::of::<T>())
            .ok_or(CatalogError::TypeNotRegistered)?;
        let seed =
            TypedReflectDeserializer::new(registration, registry);
        let partial = seed
            .deserialize(value)
            .map_err(|e| CatalogError::Deserialize(e.to_string()))?;
        let value = T::from_reflect(partial.as_ref())
            .ok_or(CatalogError::FromReflect)?;
        Ok(RemoteTarget::new::<T>(value))
    })
}

/// Build the [`Segment<T>`] to JSON encoder for `T`.
fn make_serialize<T>() -> SerializeFn
where
    T: FromReflect + TypePath + ThreadSafe,
{
    Box::new(
        |action_world: &ActionWorld,
         id: ActionId,
         registry: &TypeRegistry| {
            let segment = action_world.get_segment::<T>(id)?;
            let start = serde_json::to_value(
                TypedReflectSerializer::new(&segment.start, registry),
            )
            .ok()?;
            let end = serde_json::to_value(
                TypedReflectSerializer::new(&segment.end, registry),
            )
            .ok()?;
            Some((start, end))
        },
    )
}

/// Build the keyframe read-back for `T`.
fn make_serialize_keyframes<T>() -> SerializeKeyframesFn
where
    T: FromReflect + TypePath + ThreadSafe,
{
    Box::new(
        |action_world: &ActionWorld,
         id: ActionId,
         registry: &TypeRegistry| {
            let storage = action_world.get_keyframes::<T>(id)?;
            let mut points = Vec::with_capacity(storage.points.len());
            for point in &storage.points {
                let value = serde_json::to_value(
                    TypedReflectSerializer::new(
                        &point.value,
                        registry,
                    ),
                )
                .ok()?;
                points.push((point.t, value, point.ease, point.hold));
            }
            Some(points)
        },
    )
}

/// Convert a field path like `::translation::x` into
/// a `bevy_reflect` compatible reflection path `translation.x`
fn motiongfx_path_to_reflect(path: &str) -> String {
    path.trim_start_matches("::").replace("::", ".")
}

/// App trait to declare a field remote-editable over BRP.
pub trait MotionGfxAnimatableApp {
    /// Declare `(S, T)` at `field_acc` editable from a remote client.
    fn register_animatable<S, T, M>(
        &mut self,
        field_acc: FieldAccessor<S, T>,
    ) -> &mut Self
    where
        S: Component<Mutability = Mutable> + TypePath,
        T: Interpolation<M>
            + Clone
            + ThreadSafe
            + FromReflect
            + TypePath
            + GetTypeRegistration,
        M: 'static;

    /// Declare field `T` of an asset editable from a remote client.
    #[cfg(feature = "asset")]
    fn register_animatable_asset<A, T, M>(
        &mut self,
        field_acc: FieldAccessor<A, T>,
    ) -> &mut Self
    where
        A: bevy_asset::Asset + TypePath,
        T: Interpolation<M>
            + Clone
            + ThreadSafe
            + FromReflect
            + TypePath
            + GetTypeRegistration,
        M: 'static;

    /// Declare field `T` of [`Resource`] `R` editable from a remote client.
    fn register_animatable_resource<R, T, M>(
        &mut self,
        field_acc: FieldAccessor<R, T>,
    ) -> &mut Self
    where
        R: Resource + TypePath,
        T: Interpolation<M>
            + Clone
            + ThreadSafe
            + FromReflect
            + TypePath
            + GetTypeRegistration,
        M: 'static;
}

impl MotionGfxAnimatableApp for App {
    fn register_animatable<S, T, M>(
        &mut self,
        field_acc: FieldAccessor<S, T>,
    ) -> &mut Self
    where
        S: Component<Mutability = Mutable> + TypePath,
        T: Interpolation<M>
            + Clone
            + ThreadSafe
            + FromReflect
            + TypePath
            + GetTypeRegistration,
        M: 'static,
    {
        // The catalog and manager must exist before they are populated.
        self.init_resource::<MotionGfxManager>();
        self.init_resource::<MotionGfxCatalog>();
        self.register_type::<T>();

        let field = field_acc.field.untyped();
        let pipeline = PipelineKey::new::<BevyWorld, Entity, S, T>();

        self.world_mut().resource_scope::<MotionGfxManager, ()>(
            |world, mut manager| {
                manager
                    .registry_mut()
                    .register_remote::<BevyWorld, Entity, S, T, M>(
                        field_acc,
                    );
                world
                    .resource_mut::<MotionGfxCatalog>()
                    .insert::<S, T>(pipeline, field);
            },
        );

        self
    }

    #[cfg(feature = "asset")]
    fn register_animatable_asset<A, T, M>(
        &mut self,
        field_acc: FieldAccessor<A, T>,
    ) -> &mut Self
    where
        A: bevy_asset::Asset + TypePath,
        T: Interpolation<M>
            + Clone
            + ThreadSafe
            + FromReflect
            + TypePath
            + GetTypeRegistration,
        M: 'static,
    {
        use bevy_asset::UntypedAssetId;

        self.init_resource::<MotionGfxManager>();
        self.init_resource::<MotionGfxCatalog>();
        self.register_type::<T>();

        let field = field_acc.field.untyped();
        let pipeline =
            PipelineKey::new::<BevyWorld, UntypedAssetId, A, T>();

        self.world_mut().resource_scope::<MotionGfxManager, ()>(
            |world, mut manager| {
                manager.registry_mut().register_remote::<BevyWorld, UntypedAssetId, A, T, M>(field_acc);
                world.resource_mut::<MotionGfxCatalog>()
                    .insert_with_subject::<A, T>(
                        pipeline, field, SubjectKind::Asset,
                    );
            },
        );
        self
    }

    fn register_animatable_resource<R, T, M>(
        &mut self,
        field_acc: FieldAccessor<R, T>,
    ) -> &mut Self
    where
        R: Resource + TypePath,
        T: Interpolation<M>
            + Clone
            + ThreadSafe
            + FromReflect
            + TypePath
            + GetTypeRegistration,
        M: 'static,
    {
        use crate::world::ResourceSubject;

        self.init_resource::<MotionGfxManager>();
        self.init_resource::<MotionGfxCatalog>();
        self.register_type::<T>();

        let field = field_acc.field.untyped();
        let pipeline =
            PipelineKey::new::<BevyWorld, ResourceSubject, R, T>();

        self.world_mut().resource_scope::<MotionGfxManager, ()>(
            |world, mut manager| {
                manager.registry_mut().register_remote::<BevyWorld, ResourceSubject,
                    R, T, M>(field_acc);
                world.resource_mut::<MotionGfxCatalog>()
                    .insert_with_subject::<R, T>(
                        pipeline,
                        field,
                        SubjectKind::Resource,
                    )
            }
        );
        self
    }
}
