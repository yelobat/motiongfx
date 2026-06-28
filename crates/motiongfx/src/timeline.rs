use core::cmp::Ordering;
use core::marker::PhantomData;

use alloc::boxed::Box;
use alloc::vec::Vec;
use bevy_platform::collections::HashMap;
use field_path::field_accessor::FieldAccessor;

use crate::ThreadSafe;
use crate::action::{
    Action, ActionBuilder, ActionId, ActionKey, ActionWorld,
    InterpActionBuilder, SampleMode,
};
use crate::interpolation::Interpolation;
use crate::pipeline::{BakeCtx, PipelineKey, Range, SampleCtx};
use crate::registry::Registry;
use crate::subject::SubjectId;
use crate::track::Track;
use crate::world::SubjectSource;

pub struct Timeline<W> {
    action_world: ActionWorld,
    pipeline_counts: Box<[(PipelineKey, u32)]>,
    /// Track length is guaranteed to be at least 1 by construction.
    /// See [`TimelineBuilder::compile()`].
    tracks: Box<[Track]>,
    /// Cached actions that are queued to be sampled.
    ///
    /// This cache will be cleared everytime [`Timeline::queue_actions`]
    /// is called.
    queue_cache: QueueCache,
    /// The current time of the current track.
    curr_time: f32,
    /// The target time of the target track.
    target_time: f32,
    /// The index of the current track.
    curr_index: usize,
    /// The index of the target track.
    target_index: usize,
    _marker: PhantomData<fn() -> W>,
}

impl<W: 'static> Timeline<W> {
    pub fn bake_actions(
        &mut self,
        registry: &Registry,
        subject_world: &W,
    ) {
        for key in self.pipeline_counts.iter().map(|(key, _)| key) {
            for track in self.tracks.iter() {
                let ok = registry.pipeline.bake(
                    key,
                    BakeCtx {
                        world: subject_world,
                        track,
                        action_world: &mut self.action_world,
                        accessor_registry: &registry.accessor,
                    },
                );
                debug_assert!(
                    ok,
                    "pipeline not found for key {key:?}"
                );
            }
        }
    }

    /// Determines which actions are active at the current target time
    /// and marks them for sampling.
    ///
    /// This step is intentionally separate from
    /// [`Self::sample_queued_actions`] so that multiple timelines can
    /// queue concurrently. Queuing only requires `&mut self`, whereas
    /// sampling requires `&mut W`, which would prevent parallel
    /// execution across timelines sharing the same world.
    pub fn queue_actions(&mut self) {
        if self.tracks.is_empty() {
            return;
        }

        self.reset_queues();
        // Current time will change if the track index changes.
        let mut curr_time = self.curr_time();

        // Handle index changes.
        if self.target_index() != self.curr_index() {
            let (sample_mode, track_range) = if self.target_index()
                > self.curr_index()
            {
                // From the start.
                curr_time = 0.0;
                (
                    SampleMode::End,
                    self.curr_index()..self.target_index(),
                )
            } else {
                // From the end.
                curr_time = self.tracks[self.target_index].duration();
                (
                    SampleMode::Start,
                    (self.target_index() + 1)
                        ..(self.curr_index() + 1),
                )
            };

            for i in track_range {
                for (key, span) in self.tracks[i].sequences_spans() {
                    if span.len == 0 {
                        continue;
                    }

                    let clips = self.tracks[i].clips(*span);

                    // SAFETY: `clips` is not empty.
                    let clip = match sample_mode {
                        SampleMode::Start => clips.first().unwrap(),
                        SampleMode::End => clips.last().unwrap(),
                        SampleMode::Interp(_) => unreachable!(),
                    };

                    self.queue_cache.cache(
                        *key,
                        clip.id,
                        &mut self.action_world,
                    );

                    self.action_world
                        .edit_action(clip.id)
                        .mark(sample_mode);
                }
            }

            self.curr_index = self.target_index;
        }

        let time_range = Range {
            start: curr_time.min(self.target_time()),
            end: curr_time.max(self.target_time()),
        };

        for (key, span) in
            self.tracks[self.curr_index].sequences_spans()
        {
            if span.len == 0 {
                continue;
            }

            let clips = self.tracks[self.curr_index].clips(*span);

            // SAFETY: `clips` is not empty.
            let clips_range = Range {
                start: clips.first().unwrap().start,
                end: clips.last().unwrap().end(),
            };

            if !time_range.overlap(&clips_range) {
                continue;
            }

            // If the returned `index` is `Ok`, the target time is
            // within `span[index]`.
            //
            // If the returned `index` is `Err`, the target time is
            // before the sequence if `index == 0`, otherwise,
            // after `span[index - 1]`
            let index = clips.binary_search_by(|clip| {
                if self.target_time() < clip.start {
                    Ordering::Greater
                } else if self.target_time() > clip.end() {
                    Ordering::Less
                } else {
                    Ordering::Equal
                }
            });

            match index {
                // `target_time` is within a segment.
                Ok(index) => {
                    let clip = &clips[index];

                    let t = (self.target_time - clip.start)
                        / (clip.end() - clip.start);

                    self.queue_cache.cache(
                        *key,
                        clip.id,
                        &mut self.action_world,
                    );

                    self.action_world
                        .edit_action(clip.id)
                        .mark(SampleMode::Interp(t));
                }
                // `target_time` is out of bounds.
                Err(index) => {
                    let clip = &clips[index.saturating_sub(1)];

                    let clip_range = Range {
                        start: clip.start,
                        end: clip.end(),
                    };

                    // The clip on the far side of the gap (if one exists).
                    let crossed_from_right =
                        clips.get(index).is_some_and(|next| {
                            time_range.overlap(&Range {
                                start: next.start,
                                end: next.end(),
                            })
                        });
                    // Skip if the the animation range does not
                    // overlap with the span range.
                    if !time_range.overlap(&clip_range)
                        && !crossed_from_right
                    {
                        continue;
                    }

                    self.queue_cache.cache(
                        *key,
                        clip.id,
                        &mut self.action_world,
                    );
                    let mut action_cmd =
                        self.action_world.edit_action(clip.id);

                    if index == 0 {
                        // Target time is before the entire sequence.
                        action_cmd.mark(SampleMode::Start);
                    } else {
                        // Target time is after `index - 1`.
                        // Indexing taken care by the saturating sub
                        // above.
                        action_cmd.mark(SampleMode::End);
                    }
                }
            }
        }

        self.curr_time = self.target_time;
    }

    pub fn sample_queued_actions(
        &self,
        registry: &Registry,
        subject_world: &mut W,
    ) {
        let mut keys: Vec<PipelineKey> = self
            .pipeline_counts
            .iter()
            .map(|(key, _)| *key)
            .collect();
        for action_key in self.action_world.emptied_baseline_keys() {
            let pkey = PipelineKey::from_action_key::<W>(action_key);
            if !keys.contains(&pkey) {
                keys.push(pkey);
            }
        }

        for key in &keys {
            let ok = registry.pipeline.sample(
                key,
                SampleCtx {
                    world: subject_world,
                    action_world: &self.action_world,
                    accessor_registry: &registry.accessor,
                },
            );
            debug_assert!(ok, "pipeline not found for key {key:?}");
        }
    }

    fn reset_queues(&mut self) {
        self.queue_cache.clear();
        self.action_world.clear_all_marks();
    }

    /// Number of tracks in this timeline.
    #[inline]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Mutable access to the underlying [`ActionWorld`]. Exposed
    /// so the remote editing layer can perform remote edits.
    #[inline]
    pub fn action_world_mut(&mut self) -> &mut ActionWorld {
        &mut self.action_world
    }

    /// Immutable access to the underlying [`ActionWorld`].
    #[inline]
    pub fn action_world(&self) -> &ActionWorld {
        &self.action_world
    }

    /// Increment the reference count for `key`, adding it if absent.
    fn bump_pipeline(&mut self, key: PipelineKey) {
        let mut counts: Vec<(PipelineKey, u32)> =
            self.pipeline_counts.to_vec();
        match counts.iter_mut().find(|(k, _)| *k == key) {
            Some((_, c)) => *c += 1,
            None => counts.push((key, 1)),
        }
        self.pipeline_counts = counts.into_boxed_slice();
    }

    /// Decrement the reference count for `key`, removing it at zero.
    fn decrement_pipeline(&mut self, key: PipelineKey) {
        let mut counts: Vec<(PipelineKey, u32)> =
            self.pipeline_counts.to_vec();
        if let Some(pos) = counts.iter().position(|(k, _)| *k == key)
        {
            counts[pos].1 = counts[pos].1.saturating_sub(1);
            if counts[pos].1 == 0 {
                counts.remove(pos);
            }
        }
        self.pipeline_counts = counts.into_boxed_slice();
    }
}

// Getter methods.
impl<W> Timeline<W> {
    /// Returns the current queue cache.
    #[inline]
    pub fn queue_cache(&self) -> &QueueCache {
        &self.queue_cache
    }

    /// Returns the current playback time.
    #[inline]
    pub fn curr_time(&self) -> f32 {
        self.curr_time
    }

    /// Returns the target playback time.
    #[inline]
    pub fn target_time(&self) -> f32 {
        self.target_time
    }

    /// Returns the current track index.
    #[inline]
    pub fn curr_index(&self) -> usize {
        self.curr_index
    }

    /// Returns the target track index.
    #[inline]
    pub fn target_index(&self) -> usize {
        self.target_index
    }

    /// Returns a reference slice to all tracks.
    #[inline]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Returns a reference the current playing track.
    #[inline]
    pub fn curr_track(&self) -> &Track {
        // SAFETY: Track length is garuanteed to be at least 1.
        &self.tracks[self.curr_index]
    }

    /// Get the index of the last track. This is essentially the largest
    /// index you can provide in [`Timeline::set_target_track`].
    #[inline]
    pub fn last_track_index(&self) -> usize {
        // SAFETY: Track length is garuanteed to be at least 1.
        self.tracks.len() - 1
    }

    /// Returns `true` if the current track is the last track.
    #[inline]
    pub fn is_last_track(&self) -> bool {
        self.curr_index == self.last_track_index()
    }

    /// Has [`Self::curr_time()`] reached the end of the track at
    /// [`Self::curr_index()`]?
    #[inline]
    pub fn is_track_end(&self) -> bool {
        // SAFETY: Track length is garuanteed to be at least 1.
        self.curr_time >= self.tracks[self.curr_index()].duration()
    }

    /// Is [`Self::is_last_track()`] and [`Self::is_track_end()`].
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.is_last_track() && self.is_track_end()
    }
}

// Setter methods.
impl<W> Timeline<W> {
    /// Set the target time of the current track, clamping the value
    /// within \[0.0..=track.duration\]
    pub fn set_target_time(&mut self, target_time: f32) -> &mut Self {
        let duration = self.tracks[self.target_index].duration();

        self.target_time = target_time.clamp(0.0, duration);
        self
    }

    /// Set the target track index, clamping the value within
    /// \[0..=track_count - 1\].
    pub fn set_target_track(
        &mut self,
        target_index: usize,
    ) -> &mut Self {
        let max_index = self.last_track_index();

        self.target_index = target_index.clamp(0, max_index);
        self
    }
}

// Remote editing methods
impl<W: 'static> Timeline<W> {
    /// Insert a constant-target action onto `track_index` at runtime.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_constant_action(
        &mut self,
        track_index: usize,
        remote: &crate::remote::RemoteActionRegistry,
        key: crate::remote::RemoteFieldKey,
        subject: &dyn core::any::Any,
        target: crate::remote::RemoteTarget,
        duration: f32,
        start_at: f32,
        ease: Option<crate::action::Ease>,
    ) -> Result<ActionId, crate::remote::RemoteEditError> {
        if track_index >= self.tracks.len() {
            return Err(crate::remote::RemoteEditError::TrackOutOfRange);
        }

        if !remote.contains(&key.pipeline) {
            return Err(
                crate::remote::RemoteActionError::Unregistered.into()
            );
        }

        // Build the typed action inside the action world.
        let id = remote.construct(
            &key.pipeline,
            &mut self.action_world,
            subject,
            key.field,
            target,
            ease,
        )?;

        // Bump the pipeline reference count.
        self.bump_pipeline(key.pipeline);

        // Recompile the affected track with the new clip appended.
        // The remote constructor created the ActionKey from the same
        // subject + field, so read it back from the action world.
        let action_key = *self
            .action_world
            .action_key(id)
            .expect("action just inserted");
        let mut fragment = self.tracks[track_index].to_fragment();
        fragment.append_clip(
            action_key,
            crate::action::ActionClip::new(id, duration),
            start_at,
        );
        self.tracks[track_index] = fragment.compile();

        Ok(id)
    }

    /// Insert a remote keyframed action onto `track_index` at runtime.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_keyframes_action(
        &mut self,
        track_index: usize,
        remote: &crate::remote::RemoteActionRegistry,
        key: crate::remote::RemoteFieldKey,
        subject: &dyn core::any::Any,
        keyframes: alloc::vec::Vec<crate::remote::RemoteKeyframe>,
        duration: f32,
        start_at: f32,
        ease: Option<crate::action::Ease>,
    ) -> Result<ActionId, crate::remote::RemoteEditError> {
        if track_index >= self.tracks.len() {
            return Err(crate::remote::RemoteEditError::TrackOutOfRange);
        }

        if !remote.contains(&key.pipeline) {
            return Err(
                crate::remote::RemoteActionError::Unregistered.into()
            );
        }

        let id = remote.construct_keyframes(
            &key.pipeline,
            &mut self.action_world,
            subject,
            key.field,
            keyframes,
            ease,
        )?;

        self.bump_pipeline(key.pipeline);

        let action_key = *self
            .action_world
            .action_key(id)
            .expect("action just inserted");
        let mut fragment = self.tracks[track_index].to_fragment();
        fragment.append_clip(
            action_key,
            crate::action::ActionClip::new(id, duration),
            start_at,
        );
        self.tracks[track_index] = fragment.compile();
        Ok(id)
    }

    /// Replace an existing keyframed action's points in place.
    pub fn update_keyframes_action(
        &mut self,
        id: ActionId,
        remote: &crate::remote::RemoteActionRegistry,
        keyframes: alloc::vec::Vec<crate::remote::RemoteKeyframe>,
    ) -> Result<(), crate::remote::RemoteEditError> {
        let Some(action_key) = self.action_world.action_key(id)
        else {
            return Err(crate::remote::RemoteEditError::NotFound);
        };
        let pkey = PipelineKey::from_action_key::<W>(*action_key);

        remote.update_keyframes(
            &pkey,
            &mut self.action_world,
            id,
            keyframes,
        )
        .map_err(Into::into)
    }

    /// Remove an action from `track_index` at runtime.
    pub fn remove_action(
        &mut self,
        track_index: usize,
        id: ActionId,
    ) -> Result<(), crate::remote::RemoteEditError> {
        if track_index >= self.tracks.len() {
            return Err(crate::remote::RemoteEditError::TrackOutOfRange);
        }

        let Some(action_key) = self.action_world.action_key(id)
        else {
            return Err(crate::remote::RemoteEditError::NotFound);
        };
        let action_key = *action_key;

        let mut fragment = self.tracks[track_index].to_fragment();
        if !fragment.remove_clip(&action_key, id) {
            return Err(crate::remote::RemoteEditError::NotFound);
        }
        self.tracks[track_index] = fragment.compile();

        // Drop the action entity and decrement the pipeline count.
        if let Some(removed_key) = self.action_world.remove(id) {
            let pkey = PipelineKey::from_action_key::<W>(removed_key);
            self.decrement_pipeline(pkey);
        }

        Ok(())
    }

    /// Move an existing action to a new start time
    /// (and an optional new duration) on `track_index`, in place.
    pub fn reschedule_action(
        &mut self,
        track_index: usize,
        id: ActionId,
        new_start: f32,
        new_duration: Option<f32>,
    ) -> Result<(), crate::remote::RemoteEditError> {
        if track_index >= self.tracks.len() {
            return Err(crate::remote::RemoteEditError::TrackOutOfRange);
        }

        let mut fragment = self.tracks[track_index].to_fragment();
        fragment.reschedule_clip(id, new_start, new_duration)?;
        self.tracks[track_index] = fragment.compile();
        Ok(())
    }

    /// Replace the target value of an existing action in place.
    pub fn update_action(
        &mut self,
        id: ActionId,
        remote: &crate::remote::RemoteActionRegistry,
        target: crate::remote::RemoteTarget,
    ) -> Result<(), crate::remote::RemoteEditError> {
        let Some(action_key) = self.action_world.action_key(id)
        else {
            return Err(crate::remote::RemoteEditError::NotFound);
        };
        let pkey = PipelineKey::from_action_key::<W>(*action_key);
        remote.update(&pkey, &mut self.action_world, id, target)
            .map_err(Into::into)
    }

    /// Set, replace or clear the easing of an existing action.
    pub fn set_action_ease(
        &mut self,
        id: ActionId,
        ease: Option<crate::action::Ease>,
    ) -> Result<(), crate::remote::RemoteEditError> {
        if self.action_world.set_ease(id, ease) {
            Ok(())
        } else {
            Err(crate::remote::RemoteEditError::NotFound)
        }
    }

    /// Enable or disable an action in place.
    pub fn set_action_enabled(
        &mut self,
        id: ActionId,
        enabled: bool,
    ) -> Result<(), crate::remote::RemoteEditError> {
        if self.action_world.set_disabled(id, !enabled) {
            Ok(())
        } else {
            Err(crate::remote::RemoteEditError::NotFound)
        }
    }

    /// Whether an action is enabled. `None` if no action with
    /// `id` exists.
    pub fn is_action_enabled(&self, id: ActionId) -> Option<bool> {
        self.action_world
            .action_key(id)
            .map(|_| !self.action_world.is_disabled(id))
    }

    /// Grow the timeline to at least `count` tracks by appending
    /// empty tracks.
    pub fn ensure_track_count(&mut self, count: usize) {
        if self.tracks.len() >= count {
            return;
        }

        let mut tracks: Vec<Track> =
            core::mem::take(&mut self.tracks).into_vec();
        while tracks.len() < count {
            tracks.push(crate::track::TrackFragment::new().compile());
        }
        self.tracks = tracks.into_boxed_slice();
    }

    /// Remove every single action and reset the timeline to a single
    /// empty track.
    pub fn clear_actions(&mut self) {
        self.action_world = ActionWorld::new();
        self.pipeline_counts = Box::default();
        self.tracks =
            Box::new([crate::track::TrackFragment::new().compile()]);
        self.queue_cache.clear();
        self.curr_time = 0.0;
        self.target_time = 0.0;
        self.curr_index = 0;
        self.target_index = 0;
    }
}

/// Cached actions that are queued to be sampled.
///
/// This cache prevents duplicated samples on the same [`ActionKey`]
/// which result in sampling the same target field on the same entity
/// more than once. This is crucial as the sampling pipeline happens
/// in an unordered manner.
#[derive(Debug)]
pub struct QueueCache {
    cache: HashMap<ActionKey, ActionId>,
}

impl QueueCache {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    pub fn iter(
        &self,
    ) -> impl Iterator<Item = (&ActionKey, &ActionId)> {
        self.cache.iter()
    }

    pub fn iter_keys(&self) -> impl Iterator<Item = &ActionKey> {
        self.cache.keys()
    }

    pub fn iter_ids(&self) -> impl Iterator<Item = &ActionId> {
        self.cache.values()
    }

    /// Clear all the cached contents.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Cache an [`ActionKey`] while deduplicating the old cache if
    /// it exists.
    pub fn cache(
        &mut self,
        key: ActionKey,
        id: ActionId,
        action_world: &mut ActionWorld,
    ) {
        if let Some(prev_id) = self.cache.insert(key, id) {
            action_world.edit_action(prev_id).clear_mark();
        }
    }
}

impl Default for QueueCache {
    fn default() -> Self {
        Self::new()
    }
}

pub struct TimelineBuilder<'a, W> {
    registry: &'a mut Registry,
    action_world: ActionWorld,
    pipeline_counts: HashMap<PipelineKey, u32>,
    tracks: Vec<Track>,
    _marker: PhantomData<fn() -> W>,
}

impl<'a, W: 'static> TimelineBuilder<'a, W> {
    /// Creates an empty timeline builder.
    pub fn new(registry: &'a mut Registry) -> Self {
        Self {
            registry,
            action_world: ActionWorld::new(),
            pipeline_counts: HashMap::new(),
            tracks: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Add an [`Action`] with interpolation using
    /// [`Interpolation::interp`].
    pub fn act<I, S, T, M>(
        &mut self,
        target: I,
        field_acc: FieldAccessor<S, T>,
        action: impl Action<T>,
    ) -> InterpActionBuilder<'_, T>
    where
        W: SubjectSource<I, S> + 'static,
        I: SubjectId,
        S: 'static,
        T: Interpolation<M> + Clone + ThreadSafe,
    {
        // Register the remote constructor so this field can also
        // be edited at runtime.
        self.registry.remote.register::<W, I, S, T, M>();
        self.act_builder(target, field_acc, action)
            .with_interp(T::interp)
    }

    /// Add an [`Action`] using step interpolation.
    pub fn act_step<I, S, T>(
        &mut self,
        target: I,
        field_acc: FieldAccessor<S, T>,
        action: impl Action<T>,
    ) -> InterpActionBuilder<'_, T>
    where
        W: SubjectSource<I, S> + 'static,
        I: SubjectId,
        S: 'static,
        T: Clone + ThreadSafe,
    {
        self.act_builder(target, field_acc, action).with_interp(
            |a, b, t| {
                if t < 1.0 { a.clone() } else { b.clone() }
            },
        )
    }

    /// Add an [`Action`] without interpolation, returning an
    /// [`ActionBuilder`] for manual configuration.
    pub fn act_builder<I, S, T>(
        &mut self,
        target: I,
        field_acc: FieldAccessor<S, T>,
        action: impl Action<T>,
    ) -> ActionBuilder<'_, T>
    where
        W: SubjectSource<I, S> + 'static,
        I: SubjectId,
        S: 'static,
        T: Clone + ThreadSafe,
    {
        let field = field_acc.field;
        self.registry.register::<W, I, S, T>(field_acc);
        let key = PipelineKey::new::<W, I, S, T>();

        match self.pipeline_counts.get_mut(&key) {
            Some(count) => *count += 1,
            None => {
                self.pipeline_counts.insert(key, 1);
            }
        }

        self.action_world.add(target, field, action)
    }

    /// Remove an [`Action`].
    pub fn unact(&mut self, id: ActionId) -> bool {
        if let Some(key) = self.action_world.remove(id) {
            let pipeline_key = PipelineKey::from_action_key::<W>(key);

            let count = self
                .pipeline_counts
                .get_mut(&pipeline_key)
                .unwrap_or_else(|| {
                    panic!(
                        "Field counts not registered for {:?}!",
                        key.field()
                    )
                });

            *count -= 1;
            if *count == 0 {
                self.pipeline_counts.remove(&pipeline_key);
            }

            return true;
        }

        false
    }

    /// Add [`Track`]\(s\) to the timeline.
    pub fn add_tracks(
        &mut self,
        tracks: impl IntoIterator<Item = Track>,
    ) {
        self.tracks.extend(tracks);
    }

    /// Compile into a [`Timeline`].
    ///
    /// ## Panic
    ///
    /// Panics if the track is empty.
    /// Use [`Self::try_compile`] to explicitly handle the case where
    /// the track may be empty.
    pub fn compile(self) -> Timeline<W> {
        // TODO(nixon): What happens when track is empty?
        debug_assert!(
            !self.tracks.is_empty(),
            "Track cannot be empty!"
        );

        Timeline {
            action_world: self.action_world,
            pipeline_counts: self
                .pipeline_counts
                .into_iter()
                .collect(),
            tracks: self.tracks.into_boxed_slice(),
            queue_cache: QueueCache::new(),
            curr_time: 0.0,
            target_time: 0.0,
            curr_index: 0,
            target_index: 0,
            _marker: PhantomData,
        }
    }

    /// Similar to [`Self::compile`] but return `None` instead of
    /// panicking.
    pub fn try_compile(self) -> Option<Timeline<W>> {
        (!self.tracks.is_empty()).then(|| self.compile())
    }
}

#[cfg(test)]
mod tests {}
