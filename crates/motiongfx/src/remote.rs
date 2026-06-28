use core::any::{Any, TypeId};

use alloc::boxed::Box;
use alloc::vec::Vec;
use bevy_platform::collections::HashMap;
use field_path::field::UntypedField;

use crate::ThreadSafe;
use crate::action::{
    ActionId, ActionWorld, Ease, Keyframe, KeyframesStorage,
};
use crate::interpolation::Interpolation;
use crate::pipeline::PipelineKey;
use crate::subject::SubjectId;

/// A type-erased target.
pub struct RemoteTarget {
    pub end: Box<dyn Any + Send + Sync>,
}

impl RemoteTarget {
    pub fn new<T: ThreadSafe>(end: T) -> Self {
        Self { end: Box::new(end) }
    }
}

/// A type-erased keyframe.
pub struct RemoteKeyframe {
    /// Normalised time within the clip, `0..=1`.
    pub t: f32,
    /// The value at `t`.
    pub value: RemoteTarget,
    /// Ease of the segment ending at this keyframe.
    pub ease: Option<Ease>,
    pub hold: bool,
}

/// Errors from a remote action construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteActionError {
    /// No constructor registered for the requested key/field.
    Unregistered,
    /// The boxed [`RemoteTarget`] value was not the expected type `T`.
    TypeMismatch,
    /// A keyframe list was rejected.
    InvalidKeyframes(&'static str),
}

/// Errors from a structural remote edit on a compiled
/// [`Timeline`](crate::timeline::Timeline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteEditError {
    /// Constructor the action failed.
    Action(RemoteActionError),
    /// The edit would make the clip overlap an existing clip
    /// for the same subject + field.
    Overlap { conflict: ActionId },
    /// No action with the given id exists on the given track.
    NotFound,
    /// The track index is out of range.
    TrackOutOfRange,
}

impl From<RemoteActionError> for RemoteEditError {
    fn from(err: RemoteActionError) -> Self {
        Self::Action(err)
    }
}

/// A type-erased constructor that inserts a constant-target action
/// into an [`ActionWorld`].
type ConstructFn = Box<
    dyn Fn(
            &mut ActionWorld,
            &dyn Any,
            UntypedField,
            RemoteTarget,
            Option<Ease>,
        ) -> Result<ActionId, RemoteActionError>
        + Send
        + Sync,
>;

/// A type-erased *updater*: replaces an existing action's closure with
/// a new constant-target one, keeping its id, key, timing and easing.
type UpdateFn = Box<
    dyn Fn(
            &mut ActionWorld,
            ActionId,
            RemoteTarget,
        ) -> Result<(), RemoteActionError>
        + Send
        + Sync,
>;

/// A type-erased constructor for a **keyframed** action.
type KeyframesCtorFn = Box<
    dyn Fn(
            &mut ActionWorld,
            &dyn Any,
            UntypedField,
            Vec<RemoteKeyframe>,
            Option<Ease>,
        ) -> Result<ActionId, RemoteActionError>
        + Send
        + Sync,
>;

/// Replace an existing keyframed action's point list in place.
type KeyframesUpdateFn = Box<
    dyn Fn(
            &mut ActionWorld,
            ActionId,
            Vec<RemoteKeyframe>,
        ) -> Result<(), RemoteActionError>
        + Send
        + Sync,
>;

/// The type-erased closures registered per pipeline key.
struct RemoteEntry {
    construct: ConstructFn,
    update: UpdateFn,
    construct_keyframes: KeyframesCtorFn,
    update_keyframes: KeyframesUpdateFn,
}

/// Registry of type-erased remote-action constructors.
#[derive(Default)]
pub struct RemoteActionRegistry {
    entries: HashMap<PipelineKey, RemoteEntry>,
}

impl RemoteActionRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Register a constant-target constructor for `(W, I, S, T)`.
    /// A field is only remote-editable if it is registered here.
    pub fn register<W, I, S, T, M>(&mut self)
    where
        W: 'static,
        I: SubjectId,
        S: 'static,
        T: Interpolation<M> + Clone + ThreadSafe,
    {
        let key = PipelineKey::new::<W, I, S, T>();
        if self.entries.contains_key(&key) {
            return;
        }

        let ctor: ConstructFn = Box::new(
            |action_world: &mut ActionWorld,
             subject: &dyn Any,
             field: UntypedField,
             target: RemoteTarget,
             ease: Option<Ease>| {
                let subject = subject
                    .downcast_ref::<I>()
                    .ok_or(RemoteActionError::TypeMismatch)?;
                let end = target
                    .end
                    .downcast::<T>()
                    .map_err(|_| RemoteActionError::TypeMismatch)?;
                let end = *end;

                let action = move |_start: &T| end.clone();

                let mut builder = action_world
                    .add::<I, T>(*subject, field, action)
                    .with_interp(T::interp);

                if let Some(ease) = ease {
                    builder = builder.with_easing(ease);
                }

                Ok(builder.id())
            },
        );

        let update: UpdateFn = Box::new(
            |action_world: &mut ActionWorld,
             id: ActionId,
             target: RemoteTarget| {
                let end = target
                    .end
                    .downcast::<T>()
                    .map_err(|_| RemoteActionError::TypeMismatch)?;
                let end = *end;

                let action = move |_start: &T| end.clone();
                if !action_world.replace_action::<T>(id, action) {
                    return Err(RemoteActionError::TypeMismatch);
                }
                Ok(())
            },
        );

        let kf_ctor: KeyframesCtorFn = Box::new(
            |action_world: &mut ActionWorld,
             subject: &dyn Any,
             field: UntypedField,
             keyframes: Vec<RemoteKeyframe>,
             ease: Option<Ease>| {
                let subject = subject
                    .downcast_ref::<I>()
                    .ok_or(RemoteActionError::TypeMismatch)?;
                let points = downcast_keyframes::<T>(keyframes)?;

                let last = points[points.len() - 1].value.clone();
                let action = move |_start: &T| last.clone();

                let mut builder = action_world
                    .add::<I, T>(*subject, field, action)
                    .with_interp(T::interp);
                if let Some(ease) = ease {
                    builder = builder.with_easing(ease);
                }
                let id = builder.id();
                action_world
                    .set_keyframes(id, KeyframesStorage { points });
                Ok(id)
            },
        );

        let kf_update: KeyframesUpdateFn = Box::new(
            |action_world: &mut ActionWorld,
             id: ActionId,
             keyframes: Vec<RemoteKeyframe>| {
                if action_world.get_keyframes::<T>(id).is_none() {
                    return Err(RemoteActionError::TypeMismatch);
                }
                let points = downcast_keyframes::<T>(keyframes)?;

                let last = points[points.len() - 1].value.clone();
                let action = move |_start: &T| last.clone();
                if !action_world.replace_action::<T>(id, action) {
                    return Err(RemoteActionError::TypeMismatch);
                }

                action_world
                    .set_keyframes(id, KeyframesStorage { points });
                Ok(())
            },
        );

        self.entries.insert(
            key,
            RemoteEntry {
                construct: ctor,
                update,
                construct_keyframes: kf_ctor,
                update_keyframes: kf_update,
            },
        );
    }

    /// Construct and insert a remote action, returning its [`ActionId`].
    pub fn construct(
        &self,
        key: &PipelineKey,
        action_world: &mut ActionWorld,
        subject: &dyn Any,
        field: UntypedField,
        target: RemoteTarget,
        ease: Option<Ease>,
    ) -> Result<ActionId, RemoteActionError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(RemoteActionError::Unregistered)?;
        (entry.construct)(action_world, subject, field, target, ease)
    }

    /// Replace an existing action's target value with `target`.
    pub fn update(
        &self,
        key: &PipelineKey,
        action_world: &mut ActionWorld,
        id: ActionId,
        target: RemoteTarget,
    ) -> Result<(), RemoteActionError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(RemoteActionError::Unregistered)?;
        (entry.update)(action_world, id, target)
    }

    /// Construct and insert a remote **keyframed** action, returning its
    /// [`ActionId`].
    pub fn construct_keyframes(
        &self,
        key: &PipelineKey,
        action_world: &mut ActionWorld,
        subject: &dyn Any,
        field: UntypedField,
        keyframes: Vec<RemoteKeyframe>,
        ease: Option<Ease>,
    ) -> Result<ActionId, RemoteActionError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(RemoteActionError::Unregistered)?;
        (entry.construct_keyframes)(
            action_world,
            subject,
            field,
            keyframes,
            ease,
        )
    }

    /// Replace an existing keyframed action's points in place.
    pub fn update_keyframes(
        &self,
        key: &PipelineKey,
        action_world: &mut ActionWorld,
        id: ActionId,
        keyframes: Vec<RemoteKeyframe>,
    ) -> Result<(), RemoteActionError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(RemoteActionError::Unregistered)?;
        (entry.update_keyframes)(action_world, id, keyframes)
    }

    /// Whether a constructor exists for `key`.
    pub fn contains(&self, key: &PipelineKey) -> bool {
        self.entries.contains_key(key)
    }
}

/// Downcast a [`RemoteKeyframe`] list to conrete points, then sort
/// by `t` and clamp each `t` to `0..=1`.
fn downcast_keyframes<T: ThreadSafe>(
    keyframes: Vec<RemoteKeyframe>,
) -> Result<Vec<Keyframe<T>>, RemoteActionError> {
    if keyframes.is_empty() {
        return Err(RemoteActionError::InvalidKeyframes(
            "keyframe list is empty",
        ));
    }

    let mut points = Vec::with_capacity(keyframes.len());
    for kf in keyframes {
        if !kf.t.is_finite() {
            return Err(RemoteActionError::InvalidKeyframes(
                "keyframe `t` is not finite",
            ));
        }
        let value = kf
            .value
            .end
            .downcast::<T>()
            .map_err(|_| RemoteActionError::TypeMismatch)?;
        points.push(Keyframe {
            t: kf.t.clamp(0.0, 1.0),
            value: *value,
            ease: kf.ease,
            hold: kf.hold,
        });
    }

    points.sort_by(|a, b| {
        a.t.partial_cmp(&b.t).unwrap_or(core::cmp::Ordering::Equal)
    });
    Ok(points)
}

/// Identifies an animatable field at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RemoteFieldKey {
    pub pipeline: PipelineKey,
    pub field: UntypedField,
}

impl RemoteFieldKey {
    pub fn new(pipeline: PipelineKey, field: UntypedField) -> Self {
        Self { pipeline, field }
    }

    pub fn subject_type(&self) -> TypeId {
        self.pipeline.subject_id_type()
    }
}
