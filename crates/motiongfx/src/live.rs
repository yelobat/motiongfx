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
pub struct LiveTarget {
    pub end: Box<dyn Any + Send + Sync>,
}

impl LiveTarget {
    pub fn new<T: ThreadSafe>(end: T) -> Self {
        Self { end: Box::new(end) }
    }
}

/// A type-erased keyframe.
pub struct LiveKeyframe {
    /// Normalised time within the clip, `0..=1`.
    pub t: f32,
    /// The value at `t`.
    pub value: LiveTarget,
    /// Ease of the segment ending at this keyframe.
    pub ease: Option<Ease>,
    pub hold: bool,
}

/// Errors from a live action construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveActionError {
    /// No constructor registered for the requested key/field.
    Unregistered,
    /// The boxed [`LiveTarget`] value was not the expected type `T`.
    TypeMismatch,
    /// A keyframe list was rejected.
    InvalidKeyframes(&'static str),
}

/// Errors from a structural live edit on a compiled
/// [`Timeline`](crate::timeline::Timeline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveEditError {
    /// Constructor the action failed.
    Action(LiveActionError),
    /// The edit would make the clip overlap an existing clip
    /// for the same subject + field.
    Overlap { conflict: ActionId },
    /// No action with the given id exists on the given track.
    NotFound,
    /// The track index is out of range.
    TrackOutOfRange,
}

impl From<LiveActionError> for LiveEditError {
    fn from(err: LiveActionError) -> Self {
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
            LiveTarget,
            Option<Ease>,
        ) -> Result<ActionId, LiveActionError>
        + Send
        + Sync,
>;

/// A type-erased *updater*: replaces an existing action's closure with
/// a new constant-target one, keeping its id, key, timing and easing.
type UpdateFn = Box<
    dyn Fn(
            &mut ActionWorld,
            ActionId,
            LiveTarget,
        ) -> Result<(), LiveActionError>
        + Send
        + Sync,
>;

/// A type-erased constructor for a **keyframed** action.
type KeyframesCtorFn = Box<
    dyn Fn(
            &mut ActionWorld,
            &dyn Any,
            UntypedField,
            Vec<LiveKeyframe>,
            Option<Ease>,
        ) -> Result<ActionId, LiveActionError>
        + Send
        + Sync,
>;

/// Replace an existing keyframed action's point list in place.
type KeyframesUpdateFn = Box<
    dyn Fn(
            &mut ActionWorld,
            ActionId,
            Vec<LiveKeyframe>,
        ) -> Result<(), LiveActionError>
        + Send
        + Sync,
>;

/// The type-erased closures registered per pipeline key.
struct LiveEntry {
    construct: ConstructFn,
    update: UpdateFn,
    construct_keyframes: KeyframesCtorFn,
    update_keyframes: KeyframesUpdateFn,
}

/// Registry of type-erased live-action constructors.
#[derive(Default)]
pub struct LiveActionRegistry {
    entries: HashMap<PipelineKey, LiveEntry>,
}

impl LiveActionRegistry {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Register a constant-target constructor for `(W, I, S, T)`.
    /// A field is only live-editable if it is registered here.
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
             target: LiveTarget,
             ease: Option<Ease>| {
                let subject = subject
                    .downcast_ref::<I>()
                    .ok_or(LiveActionError::TypeMismatch)?;
                let end = target
                    .end
                    .downcast::<T>()
                    .map_err(|_| LiveActionError::TypeMismatch)?;
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
             target: LiveTarget| {
                let end = target
                    .end
                    .downcast::<T>()
                    .map_err(|_| LiveActionError::TypeMismatch)?;
                let end = *end;

                let action = move |_start: &T| end.clone();
                if !action_world.replace_action::<T>(id, action) {
                    return Err(LiveActionError::TypeMismatch);
                }
                Ok(())
            },
        );

        let kf_ctor: KeyframesCtorFn = Box::new(
            |action_world: &mut ActionWorld,
             subject: &dyn Any,
             field: UntypedField,
             keyframes: Vec<LiveKeyframe>,
             ease: Option<Ease>| {
                let subject = subject
                    .downcast_ref::<I>()
                    .ok_or(LiveActionError::TypeMismatch)?;
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
             keyframes: Vec<LiveKeyframe>| {
                if action_world.get_keyframes::<T>(id).is_none() {
                    return Err(LiveActionError::TypeMismatch);
                }
                let points = downcast_keyframes::<T>(keyframes)?;

                let last = points[points.len() - 1].value.clone();
                let action = move |_start: &T| last.clone();
                if !action_world.replace_action::<T>(id, action) {
                    return Err(LiveActionError::TypeMismatch);
                }

                action_world
                    .set_keyframes(id, KeyframesStorage { points });
                Ok(())
            },
        );

        self.entries.insert(
            key,
            LiveEntry {
                construct: ctor,
                update,
                construct_keyframes: kf_ctor,
                update_keyframes: kf_update,
            },
        );
    }

    /// Construct and insert a live action, returning its [`ActionId`].
    pub fn construct(
        &self,
        key: &PipelineKey,
        action_world: &mut ActionWorld,
        subject: &dyn Any,
        field: UntypedField,
        target: LiveTarget,
        ease: Option<Ease>,
    ) -> Result<ActionId, LiveActionError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(LiveActionError::Unregistered)?;
        (entry.construct)(action_world, subject, field, target, ease)
    }

    /// Replace an existing action's target value with `target`.
    pub fn update(
        &self,
        key: &PipelineKey,
        action_world: &mut ActionWorld,
        id: ActionId,
        target: LiveTarget,
    ) -> Result<(), LiveActionError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(LiveActionError::Unregistered)?;
        (entry.update)(action_world, id, target)
    }

    /// Construct and insert a live **keyframed** action, returning its
    /// [`ActionId`].
    pub fn construct_keyframes(
        &self,
        key: &PipelineKey,
        action_world: &mut ActionWorld,
        subject: &dyn Any,
        field: UntypedField,
        keyframes: Vec<LiveKeyframe>,
        ease: Option<Ease>,
    ) -> Result<ActionId, LiveActionError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(LiveActionError::Unregistered)?;
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
        keyframes: Vec<LiveKeyframe>,
    ) -> Result<(), LiveActionError> {
        let entry = self
            .entries
            .get(key)
            .ok_or(LiveActionError::Unregistered)?;
        (entry.update_keyframes)(action_world, id, keyframes)
    }

    /// Whether a constructor exists for `key`.
    pub fn contains(&self, key: &PipelineKey) -> bool {
        self.entries.contains_key(key)
    }
}

/// Downcast a [`LiveKeyframe`] list to conrete points, then sort
/// by `t` and clamp each `t` to `0..=1`.
fn downcast_keyframes<T: ThreadSafe>(
    keyframes: Vec<LiveKeyframe>,
) -> Result<Vec<Keyframe<T>>, LiveActionError> {
    if keyframes.is_empty() {
        return Err(LiveActionError::InvalidKeyframes(
            "keyframe list is empty",
        ));
    }

    let mut points = Vec::with_capacity(keyframes.len());
    for kf in keyframes {
        if !kf.t.is_finite() {
            return Err(LiveActionError::InvalidKeyframes(
                "keyframe `t` is not finite",
            ));
        }
        let value = kf
            .value
            .end
            .downcast::<T>()
            .map_err(|_| LiveActionError::TypeMismatch)?;
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
pub struct LiveFieldKey {
    pub pipeline: PipelineKey,
    pub field: UntypedField,
}

impl LiveFieldKey {
    pub fn new(pipeline: PipelineKey, field: UntypedField) -> Self {
        Self { pipeline, field }
    }

    pub fn subject_type(&self) -> TypeId {
        self.pipeline.subject_id_type()
    }
}
