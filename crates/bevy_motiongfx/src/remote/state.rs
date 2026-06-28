extern crate alloc;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use bevy_ecs::prelude::*;
use bevy_platform::collections::HashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::batch::EditOp;
use crate::manager::TimelineId;

/// A named time anchor on a timeline, so clients can say
/// `seek {marker: "scene2"}` instead of hard-coding floats.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Marker {
    pub track: usize,
    pub time: f32,
}

/// Presentation metadata a client may attach to a clip: a
/// display label and an RGB colour. Carried by the insert/update
/// [`EditOp`]s (so undo/redo/export restore it). Stored here so the
/// core stays presentation-free.
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize,
)]
pub struct ClipMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
}

impl ClipMeta {
    /// `true` when no field is set (an empty meta is dropped from the
    /// table rather than stored).
    pub fn is_empty(&self) -> bool {
        self.label.is_none() && self.color.is_none()
    }
}

/// How many journal entries (i.e. undo steps) are kept per timeline.
const JOURNAL_CAP: usize = 100;

/// How many stream events are buffered per timeline. A watcher that falls further behind
/// than this misses events. Its next frame carries a `"lost"` marker
/// so clients know to re-`timeline_inspect`.
const EVENT_CAP: usize = 256;

/// One successful mutating request: the ops as applied (`forward`,
/// for redo) and their inverses in reverse application order
/// (`inverse`, for undo). A batch is a single entry.
pub struct JournalEntry {
    pub forward: Vec<EditOp>,
    pub inverse: Vec<EditOp>,
}

impl JournalEntry {
    /// A compact label for an undo-history panel: the entry's forward
    /// ops by kind (e.g. `"insert"`, `"move + update"`).
    pub fn summary(&self) -> String {
        let mut kinds: Vec<&'static str> = Vec::new();
        for op in &self.forward {
            let k = op.kind_label();
            if !kinds.contains(&k) {
                kinds.push(k);
            }
        }
        if kinds.is_empty() {
            "edit".into()
        } else if self.forward.len() > kinds.len() {
            format!(
                "{} ({} ops)",
                kinds.join(" + "),
                self.forward.len()
            )
        } else {
            kinds.join(" + ")
        }
    }

    /// Rewrite references to action id `old` to `new` in both op
    /// lists.
    pub fn remap_action_id(&mut self, old: u64, new: u64) {
        for op in
            self.forward.iter_mut().chain(self.inverse.iter_mut())
        {
            op.remap_action_id(old, new);
        }
    }
}

/// Per-timeline undo/redo stacks of [`JournalEntry`]s.
#[derive(Default)]
pub struct EditJournal {
    pub undo: VecDeque<JournalEntry>,
    pub redo: Vec<JournalEntry>,
}

impl EditJournal {
    /// Record a new edit: push onto the (bounded) undo stack and
    /// clear the redo stack.
    fn record(&mut self, entry: JournalEntry) {
        if self.undo.len() == JOURNAL_CAP {
            self.undo.pop_front();
        }
        self.undo.push_back(entry);
        self.redo.clear();
    }

    /// Rewrite every reference to action id `old` to `new`, in both
    /// stacks. Undoing a remove re-creates the action under a fresh
    /// [`ActionId`](motiongfx::action::ActionId). Remapping keeps the
    /// rest of the journal pointing at the remote action so longer
    /// undo/redo chains keep working.
    pub fn remap_action_id(&mut self, old: u64, new: u64) {
        let entries =
            self.undo.iter_mut().chain(self.redo.iter_mut());
        for entry in entries {
            entry.remap_action_id(old, new);
        }
    }
}

/// Per-timeline edit state: an always-increasing *edit version*
/// (bumped by every successful mutating `motiongfx.*` request) plus the
/// undo/redo journal. The version is the cheap multi-client sync
/// primitive: clients re-`timeline_inspect` only when it moves.
#[derive(Resource, Default)]
pub struct MotionGfxEditState {
    versions: HashMap<TimelineId, u64>,
    journals: HashMap<TimelineId, EditJournal>,
    /// Markers per timeline. `BTreeMap` so listing/export order is
    /// deterministic. Markers are *not* journaled (they're navigation
    /// aids, not content).
    markers: HashMap<TimelineId, BTreeMap<String, Marker>>,
    /// Display names per timeline (`timeline_create {name}` /
    /// `timeline_rename`). Like markers: remote-layer metadata, not
    /// journaled.
    names: HashMap<TimelineId, String>,
    /// Clip labels/colours per timeline, keyed by raw action id bits.
    /// Maintained by `apply_op` (insert writes, remove deletes, update
    /// edits, clear wipes), so it never outlives its clip.
    clip_meta: HashMap<TimelineId, HashMap<u64, ClipMeta>>,
    /// Bounded per-timeline event log for `journal+watch` streaming
    /// (each event carries its own `seq`).
    events: HashMap<TimelineId, VecDeque<Value>>,
    /// Always-increasing per-timeline event sequence counters.
    event_seqs: HashMap<TimelineId, u64>,
    /// Asset-clip addressing: action id -> (handle-bearing
    /// entity bits, `asset_of` component path), recorded at insert.
    /// The owning entity of an `UntypedAssetId` is not otherwise
    /// recoverable, so this is what lets snapshots/undo/export
    /// restore asset clips.
    asset_refs: HashMap<TimelineId, HashMap<u64, (u64, String)>>,
}

impl MotionGfxEditState {
    /// Record a successful edit on `id`, returning the new version.
    pub fn bump(&mut self, id: TimelineId) -> u64 {
        let version = self.versions.entry(id).or_insert(0);
        *version += 1;
        *version
    }

    /// The current edit version of `id` (`0` if never edited).
    pub fn version(&self, id: &TimelineId) -> u64 {
        self.versions.get(id).copied().unwrap_or(0)
    }

    /// Journal a successful edit (one entry per request).
    pub fn record(&mut self, id: TimelineId, entry: JournalEntry) {
        self.journals.entry(id).or_default().record(entry);
    }

    /// The journal for `id`, if it ever recorded an edit.
    pub fn journal_mut(
        &mut self,
        id: &TimelineId,
    ) -> Option<&mut EditJournal> {
        self.journals.get_mut(id)
    }

    /// Read-only view of `id`'s journal (for an undo-history panel:
    /// `undo`/`redo` stack depths and entry contents).
    pub fn journal(&self, id: &TimelineId) -> Option<&EditJournal> {
        self.journals.get(id)
    }

    /// The markers of `id` (empty if none were set).
    pub fn markers(
        &self,
        id: &TimelineId,
    ) -> Option<&BTreeMap<String, Marker>> {
        self.markers.get(id)
    }

    /// Mutable access to `id`'s markers, creating the map on demand.
    pub fn markers_mut(
        &mut self,
        id: TimelineId,
    ) -> &mut BTreeMap<String, Marker> {
        self.markers.entry(id).or_default()
    }

    /// The display name of `id`, if one was set.
    pub fn name(&self, id: &TimelineId) -> Option<&str> {
        self.names.get(id).map(String::as_str)
    }

    /// Set (`Some`) or clear (`None`) the display name of `id`.
    pub fn set_name(&mut self, id: TimelineId, name: Option<String>) {
        match name {
            Some(name) => {
                self.names.insert(id, name);
            }
            None => {
                self.names.remove(&id);
            }
        }
    }

    /// The metadata of one clip, if any was attached.
    pub fn clip_meta(
        &self,
        id: &TimelineId,
        action: u64,
    ) -> Option<&ClipMeta> {
        self.clip_meta.get(id)?.get(&action)
    }

    /// Every clip's metadata on `id` (for inspect/export walks).
    pub fn clip_meta_map(
        &self,
        id: &TimelineId,
    ) -> Option<&HashMap<u64, ClipMeta>> {
        self.clip_meta.get(id)
    }

    /// Attach `meta` to a clip. An empty meta deletes the entry.
    pub fn set_clip_meta(
        &mut self,
        id: TimelineId,
        action: u64,
        meta: ClipMeta,
    ) {
        if meta.is_empty() {
            self.remove_clip_meta(id, action);
        } else {
            self.clip_meta
                .entry(id)
                .or_default()
                .insert(action, meta);
        }
    }

    /// Drop one clip's metadata (when the clip is removed).
    pub fn remove_clip_meta(
        &mut self,
        id: TimelineId,
        action: u64,
    ) -> Option<ClipMeta> {
        self.clip_meta.get_mut(&id)?.remove(&action)
    }

    /// Drop every clip's metadata on `id` (timeline cleared).
    pub fn clear_clip_meta(&mut self, id: &TimelineId) {
        self.clip_meta.remove(id);
    }

    /// Append a stream event (a JSON object). Its `seq` is assigned
    /// here. Returns the sequence number.
    pub fn push_event(
        &mut self,
        id: TimelineId,
        mut event: Value,
    ) -> u64 {
        let seq = self.event_seqs.entry(id).or_insert(0);
        *seq += 1;
        event["seq"] = (*seq).into();
        let queue = self.events.entry(id).or_default();
        if queue.len() == EVENT_CAP {
            queue.pop_front();
        }
        queue.push_back(event);
        *seq
    }

    /// The latest event sequence number of `id` (`0` = no events yet).
    pub fn event_seq(&self, id: &TimelineId) -> u64 {
        self.event_seqs.get(id).copied().unwrap_or(0)
    }

    /// All buffered events with `seq > after`, oldest first.
    pub fn events_since(
        &self,
        id: &TimelineId,
        after: u64,
    ) -> impl Iterator<Item = &Value> {
        self.events.get(id).into_iter().flatten().filter(
            move |event| event["seq"].as_u64().unwrap_or(0) > after,
        )
    }

    /// The oldest buffered sequence number (to detect watchers that
    /// fell off the back of the ring buffer).
    pub fn oldest_event_seq(&self, id: &TimelineId) -> Option<u64> {
        self.events
            .get(id)?
            .front()
            .and_then(|event| event["seq"].as_u64())
    }

    /// The asset addressing of one clip, when it animates an asset.
    pub fn asset_ref(
        &self,
        id: &TimelineId,
        action: u64,
    ) -> Option<&(u64, String)> {
        self.asset_refs.get(id)?.get(&action)
    }

    /// Record an asset clip's addressing at insert time.
    pub fn set_asset_ref(
        &mut self,
        id: TimelineId,
        action: u64,
        entity: u64,
        asset_of: String,
    ) {
        self.asset_refs
            .entry(id)
            .or_default()
            .insert(action, (entity, asset_of));
    }

    /// Drop one clip's asset addressing (when the clip is removed).
    pub fn remove_asset_ref(&mut self, id: TimelineId, action: u64) {
        if let Some(map) = self.asset_refs.get_mut(&id) {
            map.remove(&action);
        }
    }

    /// Drop every clip's asset addressing on `id` (timeline cleared).
    pub fn clear_asset_refs(&mut self, id: &TimelineId) {
        self.asset_refs.remove(id);
    }

    /// Drop all state for a removed timeline.
    pub fn forget(&mut self, id: &TimelineId) {
        self.versions.remove(id);
        self.journals.remove(id);
        self.markers.remove(id);
        self.names.remove(id);
        self.clip_meta.remove(id);
        self.events.remove(id);
        self.event_seqs.remove(id);
        self.asset_refs.remove(id);
    }
}
