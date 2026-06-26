pub mod func_pointers;

use core::any::TypeId;
use core::marker::PhantomData;

use func_pointers::{BakeFnPtr, SampleFnPtr};

use crate::ThreadSafe;
use crate::action::{
    ActionClip, ActionKey, ActionWorld, DisabledStorage, EaseStorage,
    InterpFn, InterpStorage, KeyframesStorage, SampleMode, Segment,
};
use crate::pipeline::func_pointers::{BakeFn, SampleFn};
use crate::registry::AccessorRegistry;
use crate::subject::SubjectId;
use crate::track::Track;
use crate::world::SubjectSource;

pub struct PipelineHandle<W, I, S, T> {
    #[expect(clippy::complexity)]
    _marker: PhantomData<fn() -> (W, I, S, T)>,
}

impl<W, I, S, T> PipelineHandle<W, I, S, T>
where
    W: 'static,
    I: SubjectId,
    S: 'static,
    T: 'static,
{
    pub fn new() -> Self {
        Self {
            _marker: PhantomData,
        }
    }

    pub fn as_key(&self) -> PipelineKey {
        PipelineKey::new::<W, I, S, T>()
    }
}

impl<W, I, S, T> Copy for PipelineHandle<W, I, S, T> {}

impl<W, I, S, T> Clone for PipelineHandle<W, I, S, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<W, I, S, T> Default for PipelineHandle<W, I, S, T>
where
    W: 'static,
    I: SubjectId,
    S: 'static,
    T: 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Uniquely identifies a [`Pipeline`] by its world, subject, source,
/// and target types.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub struct PipelineKey {
    world_id: TypeId,
    subject_id: TypeId,
    source_id: TypeId,
    target_id: TypeId,
}

impl PipelineKey {
    pub fn new<W, I, S, T>() -> Self
    where
        W: 'static,
        I: SubjectId,
        S: 'static,
        T: 'static,
    {
        Self {
            world_id: TypeId::of::<W>(),
            subject_id: TypeId::of::<I>(),
            source_id: TypeId::of::<S>(),
            target_id: TypeId::of::<T>(),
        }
    }

    pub fn from_action_key<W: 'static>(key: ActionKey) -> Self {
        Self {
            world_id: TypeId::of::<W>(),
            subject_id: key.subject_id().type_id(),
            source_id: key.field().source_id(),
            target_id: key.field().target_id(),
        }
    }

    pub(crate) fn world_id(&self) -> TypeId {
        self.world_id
    }

    /// The [`TypeId`] of the subject id type `I`.
    pub fn subject_id_type(&self) -> TypeId {
        self.subject_id
    }

    /// The [`TypeId`] of the source (component/asset) type `S`.
    pub fn source_id_type(&self) -> TypeId {
        self.source_id
    }

    /// The [`TypeId`] of the target (field) type `T`.
    pub fn target_id_type(&self) -> TypeId {
        self.target_id
    }
}

/// A pipeline for baking and sampling actions of type `(I, S, T)`.
/// The world type `W` is erased at storage. It must match at call sites.
#[derive(Debug, Clone, Copy)]
pub struct Pipeline<W, I, S, T> {
    bake: BakeFn<W>,
    sample: SampleFn<W>,
    #[expect(clippy::complexity)]
    _marker: PhantomData<fn() -> (I, S, T)>,
}

impl<W, I, S, T> Pipeline<W, I, S, T> {
    pub fn new() -> Self
    where
        W: SubjectSource<I, S>,
        I: SubjectId,
        S: 'static,
        T: Clone + ThreadSafe,
    {
        Self {
            bake: bake::<W, I, S, T>,
            sample: sample::<W, I, S, T>,
            _marker: PhantomData,
        }
    }

    pub fn untyped(&self) -> PipelineUntyped {
        PipelineUntyped {
            bake: BakeFnPtr::new(self.bake),
            sample: SampleFnPtr::new(self.sample),
        }
    }
}

impl<W, I, S, T> Default for Pipeline<W, I, S, T>
where
    W: SubjectSource<I, S>,
    I: SubjectId,
    S: 'static,
    T: Clone + ThreadSafe,
{
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PipelineUntyped {
    bake: BakeFnPtr,
    sample: SampleFnPtr,
}

impl PipelineUntyped {
    /// # Safety
    ///
    /// `W` must match the type used when registering this pipeline.
    pub(crate) unsafe fn bake<W>(&self, ctx: BakeCtx<W>) {
        let f = unsafe { self.bake.typed_unchecked::<W>() };
        f(ctx)
    }

    /// # Safety
    ///
    /// `W` must match the type used when registering this pipeline.
    pub(crate) unsafe fn sample<W>(&self, ctx: SampleCtx<W>) {
        let f = unsafe { self.sample.typed_unchecked::<W>() };
        f(ctx)
    }
}

pub struct BakeCtx<'a, W> {
    pub world: &'a W,
    pub track: &'a Track,
    pub action_world: &'a mut ActionWorld,
    pub accessor_registry: &'a AccessorRegistry,
}

pub fn bake<W, I, S, T>(ctx: BakeCtx<W>)
where
    W: SubjectSource<I, S>,
    I: SubjectId,
    S: 'static,
    T: Clone + ThreadSafe,
{
    for (key, span) in ctx.track.sequences_spans() {
        let Some(accessor) =
            ctx.accessor_registry.get::<S, T>(key.field())
        else {
            continue;
        };

        let Some(&id) =
            ctx.action_world.get_id(&key.subject_id().uid())
        else {
            continue;
        };

        let mut start =
            match ctx.action_world.get_baseline::<T>(key).cloned() {
                Some(baseline) => baseline,
                None => {
                    let Some(source) = ctx.world.get_source(id)
                    else {
                        continue;
                    };
                    let baseline = accessor.get_ref(source).clone();
                    ctx.action_world
                        .set_baseline::<T>(*key, baseline.clone());
                    baseline
                }
            };

        for ActionClip { id, .. } in ctx.track.clips(*span) {
            if ctx.action_world.is_disabled(*id) {
                ctx.action_world.edit_action(*id).set_segment(
                    Segment::new(start.clone(), start.clone()),
                );
                continue;
            }
            let Some(action) = ctx.action_world.get_action::<T>(*id)
            else {
                continue;
            };

            let end = action(&start);
            let segment = Segment::new(start.clone(), end.clone());

            ctx.action_world.edit_action(*id).set_segment(segment);

            start = end;
        }
    }
}

pub struct SampleCtx<'a, W> {
    pub world: &'a mut W,
    pub action_world: &'a ActionWorld,
    pub accessor_registry: &'a AccessorRegistry,
}

pub fn sample<W, I, S, T>(ctx: SampleCtx<W>)
where
    W: SubjectSource<I, S>,
    I: SubjectId,
    S: 'static,
    T: Clone + ThreadSafe,
{
    let Some(mut q) = ctx.action_world.world().try_query::<(
        &ActionKey,
        &SampleMode,
        &Segment<T>,
        &InterpStorage<T>,
        Option<&EaseStorage>,
        Option<&KeyframesStorage<T>>,
        Option<&DisabledStorage>,
    )>() else {
        return;
    };

    for (
        key,
        sample_mode,
        segment,
        interp,
        ease,
        keyframes,
        disabled,
    ) in q.iter(ctx.action_world.world())
    {
        let Some(accessor) =
            ctx.accessor_registry.get::<S, T>(key.field())
        else {
            continue;
        };

        let Some(&id) =
            ctx.action_world.get_id(&key.subject_id().uid())
        else {
            continue;
        };

        let target = if disabled.is_some() {
            segment.start.clone()
        } else {
            match sample_mode {
                SampleMode::Start => segment.start.clone(),
                SampleMode::End => segment.end.clone(),
                SampleMode::Interp(t) => {
                    let t = match ease {
                        Some(ease) => ease.0.eval(*t),
                        None => *t,
                    };

                    match keyframes {
                        Some(kf) => sample_keyframes(
                            &kf.points,
                            &segment.start,
                            interp.0,
                            t,
                        ),
                        None => {
                            interp.0(&segment.start, &segment.end, t)
                        }
                    }
                }
            }
        };

        ctx.world.apply_source(id, |source| {
            *accessor.get_mut(source) = target;
        });
    }

    for key in ctx.action_world.emptied_baseline_keys() {
        let Some(accessor) =
            ctx.accessor_registry.get::<S, T>(key.field())
        else {
            continue;
        };

        let Some(&id) =
            ctx.action_world.get_id(&key.subject_id().uid())
        else {
            continue;
        };
        let Some(baseline) = ctx.action_world.get_baseline::<T>(&key)
        else {
            continue;
        };
        let target = baseline.clone();
        ctx.world.apply_source(id, |source| {
            *accessor.get_mut(source) = target;
        });
    }
}

pub(crate) fn sample_keyframes<T: Clone>(
    points: &[crate::action::Keyframe<T>],
    start: &T,
    interp: InterpFn<T>,
    t: f32,
) -> T {
    let next = points.partition_point(|p| p.t <= t);
    if next >= points.len() {
        return points[points.len() - 1].value.clone();
    }
    let (prev_t, prev_value) = match next.checked_sub(1) {
        Some(i) => (points[i].t, &points[i].value),
        None => (0.0, start),
    };
    let point = &points[next];
    if point.hold {
        return prev_value.clone();
    }
    let span = point.t - prev_t;
    let local = if span <= f32::EPSILON {
        1.0
    } else {
        ((t - prev_t) / span).clamp(0.0, 1.0)
    };
    let local = match &point.ease {
        Some(ease) => ease.eval(local),
        None => local,
    };
    interp(prev_value, &point.value, local)
}

#[derive(Default, Debug, PartialEq, Clone, Copy)]
pub struct Range {
    pub start: f32,
    pub end: f32,
}

impl Range {
    /// Calculate if 2 [`Range`]s overlap.
    pub fn overlap(&self, other: &Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::Keyframe;

    fn kf(t: f32, value: f32) -> Keyframe<f32> {
        Keyframe {
            t,
            value,
            ease: None,
            hold: false,
        }
    }

    #[test]
    fn keyframes_interpolate_with_implicit_anchor_and_hold() {
        let lerp: InterpFn<f32> = |a, b, t| a + (b - a) * t;
        let points = [kf(0.5, 20.0), kf(0.75, 40.0)];

        assert_eq!(sample_keyframes(&points, &10.0, lerp, 0.0), 10.0);
        assert_eq!(
            sample_keyframes(&points, &10.0, lerp, 0.25),
            15.0
        );
        assert_eq!(
            sample_keyframes(&points, &10.0, lerp, 0.625),
            30.0
        );
        assert_eq!(
            sample_keyframes(&points, &10.0, lerp, 0.75),
            40.0
        );
        assert_eq!(sample_keyframes(&points, &10.0, lerp, 0.9), 40.0);
        assert_eq!(sample_keyframes(&points, &10.0, lerp, 1.0), 40.0);
    }

    #[test]
    fn keyframes_segment_ease_and_snap() {
        let lerp: InterpFn<f32> = |a, b, t| a + (b - a) * t;
        let points = [Keyframe {
            t: 1.0,
            value: 100.0,
            ease: Some(crate::action::Ease::Fn(
                crate::ease::quad::ease_in,
            )),
            hold: false,
        }];

        assert_eq!(sample_keyframes(&points, &0.0, lerp, 0.5), 25.0);

        let points = [kf(0.0, 50.0), kf(1.0, 100.0)];
        assert_eq!(sample_keyframes(&points, &0.0, lerp, 0.0), 50.0);
        assert_eq!(sample_keyframes(&points, &0.0, lerp, 0.5), 75.0);

        let points = [kf(0.5, 1.0), kf(0.5, 9.0), kf(1.0, 9.0)];
        assert_eq!(sample_keyframes(&points, &1.0, lerp, 0.4), 1.0);
        assert_eq!(sample_keyframes(&points, &1.0, lerp, 0.6), 9.0);
    }

    #[test]
    fn range_overlap_behavior() {
        let a = Range {
            start: 0.0,
            end: 5.0,
        };
        let b = Range {
            start: 3.0,
            end: 8.0,
        };
        let c = Range {
            start: 6.0,
            end: 10.0,
        };
        let d = Range {
            start: 5.0,
            end: 5.0,
        }; // touching boundary

        assert!(
            a.overlap(&b),
            "Overlapping ranges should return true"
        );
        assert!(
            !a.overlap(&c),
            "Separated ranges should return false"
        );
        assert!(
            a.overlap(&d),
            "Touching at end should count as overlap"
        );
    }
}
