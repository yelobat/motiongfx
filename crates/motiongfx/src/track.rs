use alloc::boxed::Box;
use alloc::vec::Vec;
use bevy_platform::collections::HashMap;
use field_path::field::UntypedField;

use crate::action::{ActionClip, ActionId, ActionKey};
use crate::remote::RemoteEditError;
use crate::sequence::Sequence;

pub trait TrackOrdering {
    /// Run all [`TrackFragment`]s one after another.
    fn ord_chain(self) -> TrackFragment;
    fn ord_all(self) -> TrackFragment;
    fn ord_any(self) -> TrackFragment;
    fn ord_flow(self, delay: f32) -> TrackFragment;
}

impl<T> TrackOrdering for T
where
    T: IntoIterator<Item = TrackFragment>,
{
    fn ord_chain(self) -> TrackFragment {
        chain(self)
    }

    fn ord_all(self) -> TrackFragment {
        all(self)
    }

    fn ord_any(self) -> TrackFragment {
        any(self)
    }

    fn ord_flow(self, delay: f32) -> TrackFragment {
        flow(delay, self)
    }
}

/// Run all [`TrackFragment`]s one after another.
#[must_use = "This function consumes all the given tracks and returns a modified one."]
pub fn chain(
    tracks: impl IntoIterator<Item = TrackFragment>,
) -> TrackFragment {
    let mut tracks_iter = tracks.into_iter();
    let mut track = tracks_iter.next().unwrap_or_default();

    let mut chain_duration = track.duration;

    for mut other_track in tracks_iter {
        for (key, mut other_sequence) in other_track.sequences.drain()
        {
            other_sequence.delay(chain_duration);
            track = track.upsert_sequence(key, other_sequence);
        }

        chain_duration += other_track.duration;
    }

    track.duration = chain_duration;
    track
}

/// Run all [`Track`]s concurrently and wait for all of them to finish.
#[must_use = "This function consumes all the given tracks and returns a modified one."]
pub fn all(
    tracks: impl IntoIterator<Item = TrackFragment>,
) -> TrackFragment {
    let mut tracks_iter = tracks.into_iter();
    let mut track = tracks_iter.next().unwrap_or_default();

    let mut max_duration = track.duration;

    for mut other_track in tracks_iter {
        max_duration = max_duration.max(other_track.duration);

        for (key, other_sequence) in other_track.sequences.drain() {
            track = track.upsert_sequence(key, other_sequence);
        }
    }

    track.duration = max_duration;
    track
}

/// Run all [`Track`]s concurrently and wait for any of them to finish.
#[must_use = "This function consumes all the given tracks and returns a modified one."]
pub fn any(
    tracks: impl IntoIterator<Item = TrackFragment>,
) -> TrackFragment {
    let mut tracks_iter = tracks.into_iter();
    let mut track = tracks_iter.next().unwrap_or_default();

    let mut min_duration = track.duration;

    for mut other_track in tracks_iter {
        min_duration = min_duration.min(other_track.duration);

        for (key, other_sequence) in other_track.sequences.drain() {
            track = track.upsert_sequence(key, other_sequence);
        }
    }

    track.duration = min_duration;
    track
}

/// Run one [`Track`] after another with a fixed delay time.
#[must_use = "This function consumes all the given tracks and returns a modified one."]
pub fn flow(
    delay: f32,
    tracks: impl IntoIterator<Item = TrackFragment>,
) -> TrackFragment {
    let mut tracks_iter = tracks.into_iter();
    let mut track = tracks_iter.next().unwrap_or_default();

    let mut flow_delay = 0.0;
    let mut final_duration = track.duration;

    for other_track in tracks_iter {
        flow_delay += delay;
        final_duration =
            (flow_delay + other_track.duration).max(final_duration);

        for (key, mut sequence) in other_track.sequences {
            sequence.delay(flow_delay);
            track = track.upsert_sequence(key, sequence);
        }
    }

    track.duration = final_duration;
    track
}

/// Run a [`Track`] after a fixed delay time.
#[must_use = "This function consumes the given track and returns a modified one."]
pub fn delay(delay: f32, mut track: TrackFragment) -> TrackFragment {
    for sequence in track.sequences.values_mut() {
        sequence.delay(delay);
    }

    track
}

pub struct TrackFragment {
    sequences: HashMap<ActionKey, Sequence>,
    duration: f32,
}

impl TrackFragment {
    pub fn new() -> Self {
        Self {
            sequences: HashMap::new(),
            duration: 0.0,
        }
    }

    pub fn single(key: ActionKey, clip: ActionClip) -> Self {
        Self {
            duration: clip.duration,
            sequences: [(key, Sequence::new(clip))].into(),
        }
    }

    /// Updates or inserts a [`Sequence`] in a track.
    ///
    /// If the [`ActionKey`] already exists, this method appends the
    /// clips of the `new_sequence` to the existing sequence.
    /// If the [`ActionKey`] does not exist, a new entry is created
    /// for the `new_sequence`.
    ///
    /// This method consumes `self` and returns a modified instance,
    /// following a builder pattern.
    ///
    /// # Parameters
    ///
    /// * `key`: The unique identifier for the track.
    /// * `new_sequence`: The sequence to be added or extended.
    pub fn upsert_sequence(
        mut self,
        key: ActionKey,
        new_sequence: Sequence,
    ) -> Self {
        match self.sequences.get_mut(&key) {
            Some(sequence) => {
                sequence.extend(new_sequence);
            }
            None => {
                self.sequences.insert(key, new_sequence);
            }
        }

        self
    }

    pub fn compile(self) -> Track {
        if self.sequences.is_empty() {
            return Track {
                field_lookups: Box::new([]),
                sequence_spans: Box::new([]),
                clip_arena: Box::new([]),
                duration: self.duration,
            };
        }

        let mut sequences =
            self.sequences.into_iter().collect::<Vec<_>>();
        sequences.sort_by_key(|(key, _)| *key.field());

        let mut seq_offset = 0;
        let mut sequence_spans = Vec::with_capacity(sequences.len());

        let mut field = sequences[0].0.field();
        let mut field_offset = 0;
        let mut field_len = 0;
        let mut field_lookups = Vec::new();

        for (key, seq) in sequences.iter() {
            sequence_spans.push((
                *key,
                Span {
                    offset: seq_offset,
                    len: seq.len(),
                },
            ));
            seq_offset += seq.len();

            if key.field() != field {
                field_lookups.push((
                    *field,
                    Span {
                        offset: field_offset,
                        len: field_len,
                    },
                ));

                field = key.field();
                field_offset = field_len;
                field_len = 0;
            }
            field_len += 1;
        }

        // Final field.
        field_lookups.push((
            *field,
            Span {
                offset: field_offset,
                len: field_len,
            },
        ));

        let clip_arena = sequences
            .into_iter()
            .flat_map(|(_, clips)| clips)
            .collect();

        Track {
            field_lookups: field_lookups.into_boxed_slice(),
            sequence_spans: sequence_spans.into_boxed_slice(),
            clip_arena,
            duration: self.duration,
        }
    }
}

impl Default for TrackFragment {
    fn default() -> Self {
        Self::new()
    }
}

/// A compiled dense action sequences, optimized for playback and
/// queries.
///
/// A `Track` is created from a [`TrackFragment`] and provides an
/// immutable, space-efficient layout. [`ActionClip`]s are stored
/// in a flat array with spans for quick access.
#[derive(Debug)]
pub struct Track {
    // TODO: Use this to optimized baking/sampling? (There are no
    // use case for the lookups atm!)
    /// Lookup from each field to the range of actions affecting it.
    ///
    /// Each entry holds an [`UntypedField`] and a [`Span`] into
    /// `clip_spans`.
    field_lookups: Box<[(UntypedField, Span)]>,

    /// [`ActionClip`]s grouped by [`ActionKey`] in sorted order.
    ///
    /// Each entry holds an [`ActionKey`] and a [`Span`] into
    /// `clip_arena`.
    sequence_spans: Box<[(ActionKey, Span)]>,

    /// Contiguous storage of all action clips.
    clip_arena: Box<[ActionClip]>,

    /// Total duration of the track in seconds.
    duration: f32,
}

impl Track {
    pub fn lookup_field_spans(
        &self,
        field: impl Into<UntypedField>,
    ) -> Option<&[(ActionKey, Span)]> {
        let index = self
            .field_lookups
            .binary_search_by_key(&field.into(), |(f, _)| *f)
            .ok()?;

        let (_, span) = &self.field_lookups[index];

        Some(
            &self.sequence_spans[span.offset..span.offset + span.len],
        )
    }

    #[inline]
    pub fn field_lookups(&self) -> &[(UntypedField, Span)] {
        &self.field_lookups
    }

    #[inline]
    pub fn sequences_spans(&self) -> &[(ActionKey, Span)] {
        &self.sequence_spans
    }

    #[inline]
    pub fn clips(&self, span: Span) -> &[ActionClip] {
        &self.clip_arena[span.offset..span.offset + span.len]
    }

    #[inline]
    pub fn duration(&self) -> f32 {
        self.duration
    }

    /// Reconstruct an editable [`TrackFragment`] from this
    /// compiled track. Used by remote editing to add/remove clips
    /// and recompile.
    pub fn to_fragment(&self) -> TrackFragment {
        let mut sequences: HashMap<ActionKey, Sequence> =
            HashMap::new();

        for (key, span) in self.sequences_spans() {
            let clips = self.clips(*span);
            if clips.is_empty() {
                continue;
            }

            let mut seq = Sequence::new(clips[0]);
            for clip in &clips[1..] {
                seq.push(*clip);
            }
            sequences.insert(*key, seq);
        }

        TrackFragment {
            sequences,
            duration: self.duration,
        }
    }
}

impl TrackFragment {
    /// Append a clip for `key`, scheduled to start at the
    /// current end of that key's sequence (or at `start_at` if later).
    /// Extends the track duration if needed. Returns the scheduled start time.
    pub fn append_clip(
        &mut self,
        key: ActionKey,
        clip: ActionClip,
        start_at: f32,
    ) -> f32 {
        let start = match self.sequences.get(&key) {
            Some(seq) => seq.end().max(start_at),
            None => start_at.max(0.0),
        };
        let mut clip = clip;
        clip.start = start;

        match self.sequences.get_mut(&key) {
            Some(seq) => seq.push(clip),
            None => {
                self.sequences.insert(key, Sequence::new(clip));
            }
        }

        self.duration = self.duration.max(clip.end());
        start
    }

    /// Remove the clip with the given [`ActionId`] from `key`'s
    /// sequence. Returns `true` if a clip was removed. If the sequence
    /// becomes empty it is dropped entirely.
    pub fn remove_clip(
        &mut self,
        key: &ActionKey,
        id: ActionId,
    ) -> bool {
        let Some(seq) = self.sequences.get(key) else {
            return false;
        };

        let remaining: Vec<ActionClip> = seq
            .clips
            .iter()
            .copied()
            .filter(|c| c.id != id)
            .collect();

        if remaining.len() == seq.clips.len() {
            return false;
        }

        match remaining.split_first() {
            Some((first, rest)) => {
                let mut new_seq = Sequence::new(*first);
                for c in rest {
                    new_seq.push(*c);
                }
                self.sequences.insert(*key, new_seq);
            }
            None => {
                self.sequences.remove(key);
            }
        }

        self.duration = self
            .sequences
            .values()
            .map(|s| s.end())
            .fold(0.0_f32, f32::max);

        true
    }

    /// Reschedule the clip with the given [`ActionId`] to a new start
    /// time (and, optionally, a new duration), keeping it in the same
    /// sequence.
    ///
    /// The clip stays attached to the same `(subject, field)` action.
    pub fn reschedule_clip(
        &mut self,
        id: ActionId,
        new_start: f32,
        new_duration: Option<f32>,
    ) -> Result<(), RemoteEditError> {
        let Some(key) = self.sequences.iter().find_map(|(k, seq)| {
            seq.clips.iter().any(|c| c.id == id).then_some(*k)
        }) else {
            return Err(RemoteEditError::NotFound);
        };

        let mut clips: Vec<ActionClip> =
            self.sequences[&key].clips.iter().copied().collect();
        for clip in clips.iter_mut() {
            if clip.id == id {
                clip.start = new_start.max(0.0);
                if let Some(duration) = new_duration {
                    clip.duration = duration.max(0.0);
                }
            }
        }

        clips.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(core::cmp::Ordering::Equal)
        });

        let (first, rest) = clips.split_first().expect("non-empty");
        let mut new_seq = Sequence::new(*first);
        for clip in rest {
            new_seq.try_push(*clip).map_err(|conflict| {
                let other =
                    if conflict.id == id { *clip } else { conflict };
                RemoteEditError::Overlap { conflict: other.id }
            })?;
        }
        *self.sequences.get_mut(&key).unwrap() = new_seq;

        self.duration = self
            .sequences
            .values()
            .map(|s| s.end())
            .fold(0.0_f32, f32::max);
        Ok(())
    }
}

impl IntoIterator for Track {
    type Item = Self;

    type IntoIter = core::array::IntoIter<Self::Item, 1>;

    fn into_iter(self) -> Self::IntoIter {
        [self].into_iter()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub offset: usize,
    pub len: usize,
}

#[cfg(test)]
mod tests {
    use bevy_ecs::entity::Entity;

    use crate::action::{ActionId, IdRegistry, UntypedSubjectId};

    use super::*;

    fn key(path: &'static str) -> ActionKey {
        ActionKey::new(
            UntypedSubjectId::PLACEHOLDER,
            UntypedField::placeholder_with_path(path),
        )
    }

    const fn clip(duration: f32) -> ActionClip {
        ActionClip::new(ActionId::PLACEHOLDER, duration)
    }

    #[test]
    fn track_key_uniqueness() {
        // Sequence with 0 duration to prevent overlaps.
        const DUMMY_SEQ: Sequence = Sequence::new(clip(0.0));

        let entity1 = Entity::from_raw_u32(1).unwrap();
        let entity2 = Entity::from_raw_u32(2).unwrap();
        let field_u32_a = UntypedField::placeholder_with_path("a");
        let field_u32_b = UntypedField::placeholder_with_path("b");

        let mut id_registry = IdRegistry::new();
        let id1 = id_registry.register_instance(entity1);
        let id2 = id_registry.register_instance(entity2);

        let k1 = ActionKey::new(
            UntypedSubjectId::new::<Entity>(id1),
            field_u32_a,
        );
        let k2 = ActionKey::new(
            UntypedSubjectId::new::<Entity>(id2),
            field_u32_a,
        );
        let k3 = ActionKey::new(
            UntypedSubjectId::new::<Entity>(id1),
            field_u32_b,
        );

        let track = TrackFragment::new()
            .upsert_sequence(k1, DUMMY_SEQ.clone())
            .upsert_sequence(k2, DUMMY_SEQ.clone())
            .upsert_sequence(k3, DUMMY_SEQ.clone())
            // Similar key with the first sequence.
            .upsert_sequence(k1, DUMMY_SEQ.clone());

        assert_eq!(track.sequences.len(), 3);
    }

    #[test]
    fn chain_duration_and_delay() {
        let track1 = TrackFragment::single(key("a"), clip(1.0));
        let track2 = TrackFragment::single(key("b"), clip(2.0));

        let track = [track1, track2].ord_chain();

        assert_eq!(track.duration, 3.0);
        let seq_b = &track.sequences[&key("b")];
        // `seq_b` should be delayed by 1.0 (duration of `track1`).
        assert_eq!(seq_b.start(), 1.0);
    }

    #[test]
    fn all_duration_max() {
        let track1 = TrackFragment::single(key("a"), clip(1.0));
        let track2 = TrackFragment::single(key("b"), clip(3.0));

        let track = [track1, track2].ord_all();
        assert_eq!(track.duration, 3.0);
    }

    #[test]
    fn any_duration_min() {
        let track1 = TrackFragment::single(key("a"), clip(1.0));
        let track2 = TrackFragment::single(key("b"), clip(3.0));

        let track = [track1, track2].ord_any();
        assert_eq!(track.duration, 1.0);
    }

    #[test]
    fn flow_with_delay() {
        let track1 = TrackFragment::single(key("a"), clip(1.0));
        let track2 = TrackFragment::single(key("b"), clip(1.0));

        let track = [track1, track2].ord_flow(0.5);

        assert_eq!(track.duration, 1.5); // 0.5 delay + 1.0 duration
        let seq_b = &track.sequences[&key("b")];
        // `seq_b` should be delayed by 0.5
        assert_eq!(seq_b.start(), 0.5);
    }

    #[test]
    fn delay_applies_offset() {
        let track = TrackFragment::single(key("a"), clip(2.0));

        let track = delay(1.5, track);
        let seq_a = &track.sequences[&key("a")];

        assert_eq!(seq_a.start(), 1.5);
        assert_eq!(seq_a.end(), 3.5);
        assert_eq!(track.duration, 2.0);
    }

    #[test]
    fn roundtrip_to_fragment_preserves_clips() {
        let frag = [
            TrackFragment::single(key("a"), clip(1.0)),
            TrackFragment::single(key("b"), clip(2.0)),
        ]
        .ord_all();
        let compiled = frag.compile();
        let rebuilt = compiled.to_fragment();

        assert_eq!(rebuilt.sequences.len(), 2);
        assert_eq!(rebuilt.duration, 2.0);
        assert_eq!(rebuilt.sequences[&key("a")].end(), 1.0);
        assert_eq!(rebuilt.sequences[&key("b")].end(), 2.0);
    }

    #[test]
    fn append_clip_schedules_after_existing() {
        let mut frag = TrackFragment::single(key("a"), clip(1.0))
            .compile()
            .to_fragment();

        let id = ActionId::PLACEHOLDER;
        let start =
            frag.append_clip(key("a"), ActionClip::new(id, 2.0), 0.0);
        assert_eq!(start, 1.0);
        assert_eq!(frag.duration, 3.0);
        assert_eq!(frag.sequences[&key("a")].len(), 2);
    }

    #[test]
    fn append_clip_new_key_starts_at_start_at() {
        let mut frag = TrackFragment::single(key("a"), clip(1.0))
            .compile()
            .to_fragment();
        let id = ActionId::PLACEHOLDER;
        let start =
            frag.append_clip(key("b"), ActionClip::new(id, 1.0), 0.5);
        assert_eq!(start, 0.5);
        assert_eq!(frag.duration, 1.5);
    }

    #[test]
    fn remove_clip_empties_sequence() {
        let id = Entity::from_raw_u32(7).unwrap();
        let action_id = ActionId::new(id);
        let mut frag = TrackFragment::single(
            key("a"),
            ActionClip::new(action_id, 1.0),
        )
        .compile()
        .to_fragment();

        assert!(frag.remove_clip(&key("a"), action_id));
        assert!(frag.sequences.is_empty());
        assert_eq!(frag.duration, 0.0);
        let _ = frag.compile();
    }

    #[test]
    fn reschedule_clip_moves_start_and_updates_duration() {
        let id = Entity::from_raw_u32(9).unwrap();
        let action_id = ActionId::new(id);
        let mut frag = TrackFragment::single(
            key("a"),
            ActionClip::new(action_id, 1.0),
        )
        .compile()
        .to_fragment();

        assert_eq!(frag.duration, 1.0);
        assert_eq!(
            frag.reschedule_clip(action_id, 2.0, Some(3.0)),
            Ok(())
        );
        assert_eq!(frag.duration, 5.0);
        assert_eq!(frag.sequences[&key("a")].start(), 2.0);
        assert_eq!(frag.sequences[&key("a")].end(), 5.0);

        let other = ActionId::new(Entity::from_raw_u32(10).unwrap());
        assert_eq!(
            frag.reschedule_clip(other, 0.0, None),
            Err(RemoteEditError::NotFound)
        );
    }

    #[test]
    fn reschedule_clip_overlap_fails_without_mutating() {
        let first = ActionId::new(Entity::from_raw_u32(11).unwrap());
        let second = ActionId::new(Entity::from_raw_u32(12).unwrap());
        let mut frag = TrackFragment::single(
            key("a"),
            ActionClip::new(first, 1.0),
        );
        frag.append_clip(key("a"), ActionClip::new(second, 1.0), 0.0);

        assert_eq!(
            frag.reschedule_clip(second, 0.5, None),
            Err(RemoteEditError::Overlap { conflict: first })
        );

        assert_eq!(frag.sequences[&key("a")].start(), 0.0);
        assert_eq!(frag.sequences[&key("a")].end(), 2.0);
        assert_eq!(frag.duration, 2.0);

        assert_eq!(
            frag.reschedule_clip(first, 0.5, None),
            Err(RemoteEditError::Overlap { conflict: second })
        );
    }
}
