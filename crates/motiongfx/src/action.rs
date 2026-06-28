use core::any::{Any, TypeId};
use core::marker::PhantomData;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::prelude::*;
use bevy_ecs::world::DeferredWorld;
use bevy_platform::collections::HashMap;
use field_path::field::UntypedField;

use crate::ThreadSafe;
use crate::subject::SubjectId;
use crate::track::TrackFragment;

/// A type-erased unique Id in the [`IdRegistry`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct UId(u64);

/// A type-erased [`UId`] map and generator for each unique
/// [`SubjectId`]s. It also performs book keeping for all id instances
/// and remove them when there is none left.
#[derive(Resource)]
pub struct IdRegistry<I: SubjectId> {
    /// Maps `SubjectId`s to [`UId`]s .
    uid_map: HashMap<I, UId>,
    /// Maps [`UId`]s to `SubjectId`s.
    id_map: HashMap<UId, I>,
    /// The number of instances using the same [`UId`].
    instance_counts: HashMap<UId, u32>,
    /// The next [`UId`], incremented on every new [`UId`] created.
    next_uid: UId,
}

impl<I: SubjectId> IdRegistry<I> {
    pub fn new() -> Self {
        Self {
            uid_map: HashMap::new(),
            id_map: HashMap::new(),
            instance_counts: HashMap::new(),
            next_uid: UId(0),
        }
    }

    /// Registers the [`SubjectId`] with an intial instance count of 1
    /// if it doesn't exist yet, otherwise, increase the existing
    /// instance count.
    ///
    /// Returns the [`UId`] of the associated [`SubjectId`].
    pub fn register_instance(&mut self, id: I) -> UId {
        let uid = *self.uid_map.entry(id).or_insert_with(|| {
            self.next_uid.0 += 1;
            self.id_map.insert(self.next_uid, id);
            self.instance_counts.insert(self.next_uid, 1);
            self.next_uid
        });

        // SAFETY: `uid_counts` is added for every new UId!
        *self.instance_counts.get_mut(&uid).unwrap() += 1;

        uid
    }

    /// Reduce the instance count of a [`SubjectId`] associated with
    /// the provided [`UId`]. When the instance count reaches 0, the
    /// entire registry will be erased.
    ///
    /// Returns `true` if the instance is being successfully removed,
    /// `false` if the registry doesn't exist in the first place.
    pub fn remove_instance(&mut self, uid: &UId) -> bool {
        let Some(count) = self.instance_counts.get_mut(uid) else {
            return false;
        };

        *count -= 1;

        // Remove the underlying data when it's the last instance.
        if *count == 0 {
            let id = self.id_map.get(uid).unwrap();
            self.uid_map.remove(id);
            self.id_map.remove(uid);
            self.instance_counts.remove(uid);
        }

        true
    }

    /// Checks if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.uid_map.is_empty()
    }

    pub fn get_uid(&self, id: &I) -> Option<&UId> {
        self.uid_map.get(id)
    }

    pub fn get_id(&self, uid: &UId) -> Option<&I> {
        self.id_map.get(uid)
    }
}

impl<I: SubjectId> Default for IdRegistry<I> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct UntypedSubjectId {
    /// The [`TypeId`] of the [`SubjectId`].
    type_id: TypeId,
    /// The type-erased [`UId`] of the [`SubjectId`].
    uid: UId,
}

impl UntypedSubjectId {
    pub const PLACEHOLDER: Self =
        Self::placeholder_with_u64(u64::MAX);

    pub const fn new<I: SubjectId>(uid: UId) -> Self {
        Self {
            type_id: TypeId::of::<I>(),
            uid,
        }
    }

    pub const fn placeholder_with_u64(id: u64) -> Self {
        Self {
            type_id: TypeId::of::<()>(),
            uid: UId(id),
        }
    }

    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    pub const fn uid(&self) -> UId {
        self.uid
    }
}

/// Key that uniquely identifies a sequence of non-overlapping
/// actions.
#[derive(
    Component,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
)]
#[component(immutable)]
pub struct ActionKey {
    /// The subject Id of the action.
    subject_id: UntypedSubjectId,
    /// The source and target field related to the subject.
    field: UntypedField,
}

impl ActionKey {
    pub fn new(
        subject_id: UntypedSubjectId,
        field: UntypedField,
    ) -> Self {
        Self { subject_id, field }
    }

    pub fn subject_id(&self) -> &UntypedSubjectId {
        &self.subject_id
    }

    pub fn field(&self) -> &UntypedField {
        &self.field
    }
}

#[derive(Component, Debug, Clone, Copy)]
#[component(immutable, on_remove = on_remove_id_type::<I>)]
pub struct IdType<I: SubjectId>(PhantomData<I>);

impl<I: SubjectId> IdType<I> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<I: SubjectId> Default for IdType<I> {
    fn default() -> Self {
        Self::new()
    }
}

/// Remove an instance of the target [`SubjectId`] when an action
/// entity is being despawned.
fn on_remove_id_type<I: SubjectId>(
    mut world: DeferredWorld<'_>,
    ctx: HookContext,
) {
    let uid = world
        .entity(ctx.entity)
        .get::<ActionKey>()
        .expect("Should have an `ActionKey`!")
        .subject_id
        .uid;

    let mut registry = world.resource_mut::<IdRegistry<I>>();
    registry.remove_instance(&uid);

    if registry.is_empty() {
        world.commands().remove_resource::<IdRegistry<I>>();
    }
}

#[derive(Default)]
pub struct ActionWorld {
    world: World,
    /// Per-sequence baseline: the field's value the first time
    /// that sequence was baked.
    baselines: HashMap<ActionKey, Box<dyn Any + Send + Sync>>,
}

impl ActionWorld {
    pub fn new() -> Self {
        let mut world = World::new();
        // EaseStorage is optional, so it needs to be registered
        // manually for the sample query to be valid.
        world.register_component::<EaseStorage>();

        // Same for the disabled (mute) marker.
        world.register_component::<DisabledStorage>();

        Self {
            world,
            baselines: HashMap::new(),
        }
    }

    pub fn add<I, T>(
        &mut self,
        target: I,
        field: impl Into<UntypedField>,
        action: impl Action<T>,
    ) -> ActionBuilder<'_, T>
    where
        I: SubjectId,
        T: ThreadSafe,
    {
        let field = field.into();

        self.world.register_component::<KeyframesStorage<T>>();

        let uid = self
            .world
            .get_resource_or_insert_with(|| IdRegistry::new())
            .register_instance(target);

        let key =
            ActionKey::new(UntypedSubjectId::new::<I>(uid), field);
        let world = self.world.spawn((
            key,
            IdType::<I>::new(),
            ActionStorage::new(action),
        ));

        ActionBuilder {
            world,
            key,
            _phantom: PhantomData,
        }
    }

    pub fn remove(&mut self, id: ActionId) -> Option<ActionKey> {
        let entity = id.entity();

        let key = *self
            .world
            .get_entity(entity)
            .ok()?
            .get::<ActionKey>()
            .expect("All actions should have an `ActionKey`!");

        self.world.despawn(id.entity());
        // Apply associated commands from hooks and observer when
        // despawning.
        self.world.flush();

        Some(key)
    }

    pub fn get_action<T: ThreadSafe>(
        &self,
        id: ActionId,
    ) -> Option<&impl Action<T>> {
        self.world
            .get::<ActionStorage<T>>(id.entity())
            .map(|a| &a.action)
    }

    pub fn get_id<I: SubjectId>(&self, uid: &UId) -> Option<&I> {
        self.world.get_resource::<IdRegistry<I>>()?.get_id(uid)
    }

    /// Returns the [`ActionKey`] of an existing action, if present.
    pub fn action_key(&self, id: ActionId) -> Option<&ActionKey> {
        self.world.get::<ActionKey>(id.entity())
    }

    /// The baked [`Segment<T>`] of an action.
    ///
    /// Returns `None` if the action does not exist.
    pub fn get_segment<T: ThreadSafe>(
        &self,
        id: ActionId,
    ) -> Option<&Segment<T>> {
        self.world.get::<Segment<T>>(id.entity())
    }

    /// The easing of an action, if the action exists and has one.
    /// If `None`, then it is implied to be linear.
    pub fn get_ease(&self, id: ActionId) -> Option<Ease> {
        self.world.get::<EaseStorage>(id.entity()).map(|e| e.0)
    }

    /// The keyframe points of an action, when it is a keyframed
    /// one of target type `T`.
    pub fn get_keyframes<T: ThreadSafe>(
        &self,
        id: ActionId,
    ) -> Option<&KeyframesStorage<T>> {
        self.world.get::<KeyframesStorage<T>>(id.entity())
    }

    /// Set of replace an action's keyframe points. Returns `false`
    /// if no action with `id` exists.
    pub fn set_keyframes<T: ThreadSafe>(
        &mut self,
        id: ActionId,
        keyframes: KeyframesStorage<T>,
    ) -> bool {
        let Ok(mut entity) = self.world.get_entity_mut(id.entity())
        else {
            return false;
        };

        if entity.get::<ActionKey>().is_none() {
            return false;
        }
        entity.insert(keyframes);
        true
    }

    /// Whether the action is disabled (muted). `false` also when
    /// no action with `id` exists.
    pub fn is_disabled(&self, id: ActionId) -> bool {
        self.world.get::<DisabledStorage>(id.entity()).is_some()
    }

    /// Disable (mute) or re-enable an action.
    ///
    /// Returns `false` if no action with `id` exists. The baked
    /// segments are stale afterwards.
    pub fn set_disabled(
        &mut self,
        id: ActionId,
        disabled: bool,
    ) -> bool {
        let Ok(mut entity) = self.world.get_entity_mut(id.entity())
        else {
            return false;
        };

        if entity.get::<ActionKey>().is_none() {
            return false;
        }

        if disabled {
            entity.insert(DisabledStorage);
        } else {
            entity.remove::<DisabledStorage>();
        }
        true
    }

    /// Set, replace or clear (`None` = linear) the easing of an action.
    pub fn set_ease(
        &mut self,
        id: ActionId,
        ease: Option<Ease>,
    ) -> bool {
        let Ok(mut entity) = self.world.get_entity_mut(id.entity())
        else {
            return false;
        };
        if entity.get::<ActionKey>().is_none() {
            return false;
        }

        match ease {
            Some(ease) => {
                entity.insert(EaseStorage(ease));
            }
            None => {
                entity.remove::<EaseStorage>();
            }
        }
        true
    }

    /// Replace an action.
    pub fn replace_action<T: ThreadSafe>(
        &mut self,
        id: ActionId,
        action: impl Action<T>,
    ) -> bool {
        let Ok(mut entity) = self.world.get_entity_mut(id.entity())
        else {
            return false;
        };
        if entity.get::<ActionStorage<T>>().is_none() {
            return false;
        }

        entity.insert(ActionStorage::new(action));
        true
    }

    /// The cached baseline for a sequence, if one exists.
    pub(crate) fn get_baseline<T: ThreadSafe>(
        &self,
        key: &ActionKey,
    ) -> Option<&T> {
        self.baselines.get(key)?.downcast_ref::<T>()
    }

    /// Capture a sequence's baseline.
    pub(crate) fn set_baseline<T: ThreadSafe>(
        &mut self,
        key: ActionKey,
        value: T,
    ) {
        self.baselines.entry(key).or_insert_with(|| Box::new(value));
    }

    pub(crate) fn emptied_baseline_keys(&self) -> Vec<ActionKey> {
        if self.baselines.is_empty() {
            return Vec::new();
        }

        let mut active: Vec<ActionKey> = Vec::new();
        if let Some(mut q) = self.world.try_query::<&ActionKey>() {
            for key in q.iter(&self.world) {
                active.push(*key);
            }
        }
        self.baselines
            .keys()
            .filter(|k| !active.contains(*k))
            .copied()
            .collect()
    }
}

impl ActionWorld {
    /// Returns a immutable reference to the underlying world.
    pub(crate) fn world(&self) -> &World {
        &self.world
    }

    /// Create an [`ActionCommand`] from an [`ActionId`].
    ///
    /// # Panics
    ///
    /// Panics if the action does not exists in the world.
    ///
    /// In general, this should not be an issue as this is only used
    /// internally within the crate.
    pub(crate) fn edit_action(
        &mut self,
        id: ActionId,
    ) -> ActionCommand<'_> {
        ActionCommand {
            world: self.world.entity_mut(id.entity()),
        }
    }

    /// Remove [`SampleMode`] component from all marked actions.
    pub(crate) fn clear_all_marks(&mut self) {
        let Some(mut q) = self
            .world
            .try_query_filtered::<Entity, With<SampleMode>>()
        else {
            return;
        };

        let entities = q.iter(&self.world).collect::<Vec<_>>();
        for entity in entities {
            self.world.entity_mut(entity).remove::<SampleMode>();
        }
    }
}

pub(crate) struct ActionCommand<'w> {
    world: EntityWorldMut<'w>,
}

impl ActionCommand<'_> {
    pub(crate) fn mark(
        &mut self,
        sample_mode: SampleMode,
    ) -> &mut Self {
        self.world.insert(sample_mode);
        self
    }

    pub(crate) fn clear_mark(&mut self) -> &mut Self {
        self.world.remove::<SampleMode>();
        self
    }

    /// Add or replace the segment of the action.
    pub(crate) fn set_segment<T>(
        &mut self,
        segment: Segment<T>,
    ) -> &mut Self
    where
        T: ThreadSafe,
    {
        self.world.insert(segment);
        self
    }
}

pub struct ActionBuilder<'w, T> {
    world: EntityWorldMut<'w>,
    key: ActionKey,
    _phantom: PhantomData<T>,
}

/// A builder struct to insert an interpolation method for the action
/// before compiling into an [`InterpActionBuilder`].
impl<T> ActionBuilder<'_, T> {
    /// Get the [`ActionId`] of the containing action.
    pub fn id(&self) -> ActionId {
        ActionId::new(self.world.id())
    }
}

impl<'w, T> ActionBuilder<'w, T>
where
    T: 'static,
{
    /// Set the interpolation method of the action.
    pub fn with_interp(
        mut self,
        interp: InterpFn<T>,
    ) -> InterpActionBuilder<'w, T> {
        self.world.insert(InterpStorage(interp));
        InterpActionBuilder { inner: self }
    }
}

/// An action builder that has interpolation added. This builder
/// exposes more customizations for the action and allows it to be
/// compiled into a [`TrackFragment`].
pub struct InterpActionBuilder<'w, T> {
    inner: ActionBuilder<'w, T>,
}

impl<T> InterpActionBuilder<'_, T> {
    /// Set the easing method of the action.
    pub fn with_ease(self, ease: EaseFn) -> Self {
        self.with_easing(Ease::Fn(ease))
    }

    pub fn with_easing(mut self, ease: Ease) -> Self {
        self.inner.world.insert(EaseStorage(ease));
        self
    }

    /// Get the [`ActionId`] of the containing action.
    pub fn id(&self) -> ActionId {
        self.inner.id()
    }

    /// Confirms the configuration of the action and creates a
    /// [`TrackFragment`].
    pub fn play(self, duration: f32) -> TrackFragment {
        TrackFragment::single(
            self.inner.key,
            ActionClip::new(self.id(), duration),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActionId(Entity);

impl ActionId {
    pub const PLACEHOLDER: Self = ActionId(Entity::PLACEHOLDER);

    #[inline(always)]
    pub(crate) fn new(entity: Entity) -> Self {
        Self(entity)
    }

    #[inline(always)]
    pub(crate) fn entity(&self) -> Entity {
        self.0
    }

    /// Stable `u64` representation for transport (e.g. over BRP).
    #[inline]
    #[must_use]
    pub fn to_bits(self) -> u64 {
        self.0.to_bits()
    }

    /// Reconstruct an [`ActionId`] from the bits produced by
    /// [`ActionId::to_bits`].
    #[inline]
    #[must_use]
    pub fn from_bits(bits: u64) -> Self {
        Self(Entity::from_bits(bits))
    }
}

/// An action trait which consists of a function for getting
/// the target value based on an intial value.
pub trait Action<T>: ThreadSafe + Fn(&T) -> T {}

impl<T, U> Action<T> for U where U: ThreadSafe + Fn(&T) -> T {}

/// A storage component for an [`Action`].
#[derive(Component)]
#[component(immutable)]
pub struct ActionStorage<T> {
    pub action: Box<dyn Action<T>>,
}

impl<T> ActionStorage<T> {
    pub fn new(action: impl Action<T>) -> Self {
        Self {
            action: Box::new(action),
        }
    }
}

/// Function for interpolating a type based on a [`f32`] time.
pub type InterpFn<T> = fn(start: &T, end: &T, t: f32) -> T;

/// A storage component for a custom [`InterpFn`].
///
/// This can be optionally inserted alongside [`ActionStorage`]
/// to customize the action.
#[derive(Component, Debug, Clone, Copy)]
#[component(immutable)]
pub struct InterpStorage<T>(pub InterpFn<T>);

/// Easing function on a [`f32`] time.
pub type EaseFn = fn(t: f32) -> f32;

/// An easing applied to the interpolation parameter `t`.
///
/// Either a plain easing function or a parameterized cubic-bezier
/// curve.
#[derive(Debug, Clone, Copy)]
pub enum Ease {
    Fn(EaseFn),
    CubicBezier([f32; 4]),
}

impl Ease {
    /// Evaluate the easing at `t` (expected in \[0, 1\]).
    #[inline]
    pub fn eval(&self, t: f32) -> f32 {
        match self {
            Self::Fn(f) => f(t),
            Self::CubicBezier([x1, y1, x2, y2]) => {
                cubic_bezier_ease(*x1, *y1, *x2, *y2, t)
            }
        }
    }
}

impl From<EaseFn> for Ease {
    fn from(f: EaseFn) -> Self {
        Self::Fn(f)
    }
}

impl PartialEq for Ease {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fn(a), Self::Fn(b)) => {
                core::ptr::fn_addr_eq(*a, *b)
            }
            (Self::CubicBezier(a), Self::CubicBezier(b)) => a == b,
            _ => false,
        }
    }
}

fn cubic_bezier_ease(
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    t: f32,
) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }

    if t >= 1.0 {
        return 1.0;
    }

    #[inline]
    fn coefficients(c1: f32, c2: f32) -> (f32, f32, f32) {
        let c = 3.0 * c1;
        let b = 3.0 * (c2 - c1) - c;
        let a = 1.0 - c - b;
        (a, b, c)
    }

    #[inline]
    fn sample(a: f32, b: f32, c: f32, u: f32) -> f32 {
        ((a * u + b) * u + c) * u
    }

    #[inline]
    fn slope(a: f32, b: f32, c: f32, u: f32) -> f32 {
        (3.0 * a * u + 2.0 * b) * u + c
    }

    let (ax, bx, cx) = coefficients(x1, x2);
    let (ay, by, cy) = coefficients(y1, y2);

    // Newton-Raphson
    let mut u = t;
    for _ in 0..8 {
        let x = sample(ax, bx, cx, u) - t;
        if x.abs() < 1e-6 {
            return sample(ay, by, cy, u);
        }
        let d = slope(ax, bx, cx, u);
        if d.abs() < 1e-6 {
            break;
        }
        u = (u - x / d).clamp(0.0, 1.0);
    }

    // Bisection fallback (x(u) is monotonic for x1, x2 in [0, 1])
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    u = t;
    for _ in 0..32 {
        let x = sample(ax, bx, cx, u);
        if (x - t).abs() < 1e-6 {
            break;
        }
        if x < t {
            lo = u;
        } else {
            hi = u;
        }
        u = 0.5 * (lo + hi);
    }
    sample(ay, by, cy, u)
}

/// A storage component for a custom [`EaseFn`].
///
/// This can be optionally inserted alongside [`ActionStorage`]
/// to customize the action.
#[derive(Component, Debug, Clone, Copy)]
#[component(immutable)]
pub struct EaseStorage(pub Ease);

/// Marker component: the action is disabled (muted).
///
/// A disabled action bakes to an identity [`Segment`]
/// behaving exactly as if the clip were absent.
#[derive(Component, Debug, Clone, Copy)]
#[component(immutable)]
pub struct DisabledStorage;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ActionClip {
    pub id: ActionId,
    pub start: f32,
    pub duration: f32,
}

impl ActionClip {
    pub const fn new(id: ActionId, duration: f32) -> Self {
        Self {
            id,
            start: 0.0,
            duration,
        }
    }

    #[inline]
    pub fn end(&self) -> f32 {
        self.start + self.duration
    }
}

#[derive(Component)]
#[component(immutable)]
pub struct Segment<T> {
    /// The starting value.
    pub start: T,
    /// The ending value.
    pub end: T,
}

impl<T> Segment<T> {
    pub fn new(start: T, end: T) -> Self {
        Self { start, end }
    }
}

/// One point of a keyframed action (see [`KeyframesStorage`]).
#[derive(Debug, Clone)]
pub struct Keyframe<T> {
    /// Normalised time within the clip, `0..=1`.
    pub t: f32,
    /// Absolute value at `t`.
    pub value: T,
    /// Ease of the segment *ending* at this keyframe.
    pub ease: Option<Ease>,
    /// Step interpolation: the segment ending here keeps the previous
    /// value for its whole span and snaps to `value` exactly at `t`.
    pub hold: bool,
}

/// Optional multi-keyframe payload of an action: when present, the
/// sample path interpolates through these points instead of one.
#[derive(Component)]
#[component(immutable)]
pub struct KeyframesStorage<T> {
    /// Non-empty, sorted ascending by `t`, each `t` in `0..=1`.
    /// Construction (the remote path) sorts and clamps.
    pub points: Vec<Keyframe<T>>,
}

/// Determines how a [`Segment`] should be sampled.
#[derive(Component, Debug, Clone, Copy)]
#[component(storage = "SparseSet", immutable)]
pub enum SampleMode {
    Start,
    End,
    Interp(f32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_bezier_endpoints_and_clamping() {
        let ease = Ease::CubicBezier([0.42, 0.0, 0.58, 1.0]);
        assert_eq!(ease.eval(0.0), 0.0);
        assert_eq!(ease.eval(1.0), 1.0);
        assert_eq!(ease.eval(-0.5), 0.0);
        assert_eq!(ease.eval(1.5), 1.0);
    }

    #[test]
    fn cubic_bezier_symmetric_curve_midpoint() {
        // ease-in-out style curve is symmetric about (0.5, 0.5).
        let ease = Ease::CubicBezier([0.42, 0.0, 0.58, 1.0]);
        assert!((ease.eval(0.5) - 0.5).abs() < 1e-4);
        // Symmetry: f(t) + f(1-t) == 1.
        for t in [0.1, 0.25, 0.4] {
            let sum = ease.eval(t) + ease.eval(1.0 - t);
            assert!(
                (sum - 1.0).abs() < 1e-3,
                "asymmetric at {t}: {sum}"
            );
        }
    }
}
