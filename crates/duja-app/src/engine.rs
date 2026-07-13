//! The controller actor: one thread owning the
//! [`DisplayManager`](duja_core::manager::DisplayManager) and all policy state.
//!
//! # Select loop, zero idle wakeups
//!
//! Each iteration first fires any due debounce and any due watchdog, then
//! computes a single deadline as the earliest of (a) the debounce fire instant
//! and (b) the earliest in-flight write's watchdog deadline. It then selects
//! over the command / ack / platform channels **with that deadline** — and with
//! *no* deadline (a plain blocking select) when neither timer is armed. There
//! are no ticking timer channels, so an idle engine parks with zero wakeups.
//!
//! # Coalescing and the watchdog
//!
//! The engine forwards each [`EngineCommand::SetUserLevel`] straight to the
//! display's worker, overwriting the *single* in-flight slot for that
//! `(display, feature)`. Workers coalesce latest-wins, so superseded writes are
//! replaced in the slot before they could ever be acked and can never trip the
//! watchdog. Only the newest outstanding op per `(display, key)` is watched.
//!
//! # Leaks and recovery
//!
//! When a write goes unacked past [`EngineConfig::watchdog_timeout`], or a
//! worker reports a panic, the display is marked unresponsive, its worker
//! handle is **dropped without joining** (the OS thread is detached — never
//! join a thread stuck in a GPU driver), and its in-flight entries are cleared.
//! A later enumeration that sights the display (manager emits
//! [`Responsive`](duja_core::manager::ManagerEvent::Responsive) or
//! [`Reattached`](duja_core::manager::ManagerEvent::Reattached)) spawns a fresh
//! worker with a freshly-opened controller. A display that gets stuck twice is
//! abandoned for the session (no further respawn), bounding leaked threads.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crossbeam_channel::{Receiver, Select, Sender, unbounded};

use duja_core::debounce::{Action, Debouncer};
use duja_core::id::StableDisplayId;
use duja_core::manager::{DisplayManager, ManagerEvent};
use duja_core::model::Feature;

use crate::protocol::{AckOutcome, InflightKey, WorkerAck};
use crate::worker::{WorkerHandle, spawn_worker};
use crate::{ControllerFactory, EngineCommand, EngineConfig, EngineNotification, Enumerator};

/// After a display has been marked stuck this many times, it is abandoned for
/// the session (no further worker is spawned on recovery).
const MAX_STUCK_RESPAWNS: u32 = 2;

/// All mutable state owned by the engine thread.
pub(crate) struct EngineState {
    cfg: EngineConfig,
    enumerator: Enumerator,
    factory: ControllerFactory,
    platform_rx: Receiver<()>,
    platform_open: bool,
    cmd_rx: Receiver<EngineCommand>,
    notif_tx: Sender<EngineNotification>,
    /// Kept so the ack receiver never disconnects (workers clone from this).
    ack_tx: Sender<WorkerAck>,
    ack_rx: Receiver<WorkerAck>,
    manager: DisplayManager,
    workers: BTreeMap<StableDisplayId, WorkerHandle>,
    /// Latest outstanding op per `(display, kind)`: its seq and dispatch time.
    inflight: BTreeMap<(StableDisplayId, InflightKey), (u64, Instant)>,
    /// Probed brightness max per display (for level scaling); defaults to 100.
    brightness_max: BTreeMap<StableDisplayId, u16>,
    /// Displays whose initial hardware level we are still waiting to learn. A
    /// user `SetUserLevel` clears membership, so a late initial-`Get` ack can
    /// never clobber a level the user has already chosen.
    pending_learn: std::collections::BTreeSet<StableDisplayId>,
    /// Displays with an in-flight **poll** `Get` (as opposed to an initial-learn
    /// `Get`). Its ack takes the external-change reflection path; distinguishing
    /// it here means a *stale* initial-`Get` ack (whose `pending_learn` a user
    /// action already cleared) is correctly ignored, not mistaken for a poll.
    poll_gets: std::collections::BTreeSet<StableDisplayId>,
    debouncer: Debouncer,
    /// How many times each display has been marked stuck.
    stuck_count: BTreeMap<StableDisplayId, u32>,
    /// When the next level poll is due, or `None` while polling is disabled (the
    /// flyout is closed). `None` keeps the idle engine at zero wakeups.
    poll_next: Option<Instant>,
    seq: u64,
}

impl EngineState {
    /// Build the engine state, spawn its thread (running the first enumeration
    /// immediately), and return the command sender, notification receiver, and
    /// join handle.
    pub(crate) fn launch(
        cfg: EngineConfig,
        enumerator: Enumerator,
        factory: ControllerFactory,
        platform_rx: Receiver<()>,
    ) -> (
        Sender<EngineCommand>,
        Receiver<EngineNotification>,
        JoinHandle<()>,
    ) {
        let (cmd_tx, cmd_rx) = unbounded();
        let (notif_tx, notif_rx) = unbounded();
        let (ack_tx, ack_rx) = unbounded();

        let state = EngineState {
            cfg,
            enumerator,
            factory,
            platform_rx,
            platform_open: true,
            cmd_rx,
            notif_tx,
            ack_tx,
            ack_rx,
            manager: DisplayManager::new(),
            workers: BTreeMap::new(),
            inflight: BTreeMap::new(),
            brightness_max: BTreeMap::new(),
            pending_learn: std::collections::BTreeSet::new(),
            poll_gets: std::collections::BTreeSet::new(),
            debouncer: Debouncer::new(cfg.displaychange_debounce),
            stuck_count: BTreeMap::new(),
            poll_next: None,
            seq: 0,
        };

        let join = thread::spawn(move || {
            // RATIONALE(AssertUnwindSafe): the engine's state is owned solely by
            // this thread and never observed after a panic here (the handle
            // detects termination via channel closure), so unwind-safety of the
            // captured state cannot corrupt any other observer.
            let _ = catch_unwind(AssertUnwindSafe(move || state.run()));
        });

        (cmd_tx, notif_rx, join)
    }

    /// The actor loop.
    fn run(mut self) {
        self.refresh(); // startup: first enumeration immediately.

        loop {
            // Fire anything already due before parking again.
            if let Action::Fire = self.debouncer.poll(Instant::now()) {
                self.refresh();
            }
            self.poll_watchdog(Instant::now());
            self.fire_poll_if_due(Instant::now());

            let deadline = self.next_deadline();

            let wake = {
                let mut sel = Select::new();
                let i_cmd = sel.recv(&self.cmd_rx);
                let i_ack = sel.recv(&self.ack_rx);
                let i_evt = if self.platform_open {
                    Some(sel.recv(&self.platform_rx))
                } else {
                    None
                };

                let now = Instant::now();
                let picked = match deadline {
                    Some(dl) => sel.select_timeout(dl.saturating_duration_since(now)),
                    None => Ok(sel.select()),
                };

                match picked {
                    Err(_) => Wake::Timeout,
                    Ok(oper) => {
                        let idx = oper.index();
                        if idx == i_cmd {
                            Wake::Cmd(oper.recv(&self.cmd_rx))
                        } else if idx == i_ack {
                            Wake::Ack(oper.recv(&self.ack_rx))
                        } else if Some(idx) == i_evt {
                            Wake::Evt(oper.recv(&self.platform_rx))
                        } else {
                            Wake::Timeout
                        }
                    }
                }
            };

            match wake {
                // Nothing to do: a timer expired, or a stray closed ack arm.
                Wake::Timeout | Wake::Ack(Err(_)) => {}
                Wake::Cmd(Ok(cmd)) => {
                    if self.handle_command(cmd) {
                        break;
                    }
                }
                // All command senders dropped without an explicit Shutdown
                // (Engine handle forgotten): treat as shutdown.
                Wake::Cmd(Err(_)) => break,
                Wake::Ack(Ok(ack)) => self.handle_ack(ack),
                Wake::Evt(Ok(())) => {
                    self.debouncer.on_event(Instant::now());
                }
                Wake::Evt(Err(_)) => self.platform_open = false,
            }
        }

        self.shutdown_workers();
    }

    /// Earliest instant the loop must wake: min of the debounce deadline and
    /// the earliest in-flight watchdog deadline. `None` when both are idle.
    ///
    /// This **peeks** the debouncer ([`Debouncer::deadline`]) rather than
    /// polling it: the loop already polls once at the top of each iteration to
    /// fire what is due, and a second, mutating poll here (at a fractionally
    /// later `Instant::now`) could consume a fire whose deadline fell between
    /// the two reads — silently dropping the pending enumeration.
    fn next_deadline(&self) -> Option<Instant> {
        min_opt(
            min_opt(self.debouncer.deadline(), self.earliest_watchdog_deadline()),
            self.poll_next,
        )
    }

    /// If a level poll is due, poll every eligible display and re-arm the next
    /// poll. A no-op while polling is disabled (`poll_next` is `None`), so an idle
    /// engine never wakes for this.
    fn fire_poll_if_due(&mut self, now: Instant) {
        if let Some(due) = self.poll_next
            && now >= due
        {
            self.poll_levels();
            self.poll_next = now.checked_add(self.cfg.level_poll_interval);
        }
    }

    /// Dispatch a one-shot brightness read to every responsive display, skipping
    /// any that would let a stale or self-inflicted reading through:
    /// - not responsive (nothing to read);
    /// - awaiting its initial-learn `Get` (that read owns the slot and records
    ///   the level itself);
    /// - a brightness `Set` in flight (don't read our own write mid-flight — the
    ///   echo would look like an external change);
    /// - a prior poll `Get` still pending (avoid superseding it).
    fn poll_levels(&mut self) {
        let ids: Vec<StableDisplayId> = self.workers.keys().cloned().collect();
        for id in ids {
            if self.manager.is_responsive(&id) != Some(true)
                || self.pending_learn.contains(&id)
                || self
                    .inflight
                    .contains_key(&(id.clone(), InflightKey::Set(Feature::Brightness)))
                || self
                    .inflight
                    .contains_key(&(id.clone(), InflightKey::Get(Feature::Brightness)))
            {
                continue;
            }
            self.dispatch_poll_get(&id);
        }
    }

    /// Dispatch a brightness read for a poll (unlike [`dispatch_initial_get`] it
    /// does **not** set `pending_learn`, so its ack takes the external-change
    /// reflection path rather than the initial-learn path).
    fn dispatch_poll_get(&mut self, id: &StableDisplayId) {
        let feature = Feature::Brightness;
        let seq = self.next_seq();
        if let Some(handle) = self.workers.get(id)
            && handle
                .cmd_tx
                .send(crate::protocol::WorkerCommand::Get { feature, seq })
                .is_ok()
        {
            self.inflight.insert(
                (id.clone(), InflightKey::Get(feature)),
                (seq, Instant::now()),
            );
            self.poll_gets.insert(id.clone());
        }
    }

    /// The earliest `dispatched_at + watchdog_timeout` across all in-flight ops.
    fn earliest_watchdog_deadline(&self) -> Option<Instant> {
        self.inflight
            .values()
            .filter_map(|(_seq, dispatched)| dispatched.checked_add(self.cfg.watchdog_timeout))
            .min()
    }

    /// Handle one command. Returns `true` to stop the engine.
    fn handle_command(&mut self, cmd: EngineCommand) -> bool {
        match cmd {
            EngineCommand::SetUserLevel { id, pct } => {
                let pct = pct.min(100);
                // The user's intent wins over any in-flight level probe: cancel a
                // pending initial-learn AND any pending poll `Get`, so a stale
                // pre-write reading can never be mistaken for an external change.
                self.pending_learn.remove(&id);
                self.poll_gets.remove(&id);
                self.manager.record_user_level(&id, pct);
                self.dispatch_set(&id, Feature::Brightness, pct);
                self.notify_displays_changed();
            }
            EngineCommand::SetInput { id, value } => {
                self.dispatch_input(&id, value);
            }
            EngineCommand::RefreshNow => self.refresh(),
            EngineCommand::SetLevelPolling { on } => {
                // Enabling (or re-enabling) arms an immediate poll; the loop fires
                // it at the top of the next iteration. Disabling clears the
                // deadline so the idle engine returns to zero wakeups.
                self.poll_next = on.then_some(Instant::now());
            }
            EngineCommand::Snapshot { reply } => {
                let _ = reply.send(self.manager.snapshots());
            }
            EngineCommand::Shutdown => return true,
        }
        false
    }

    /// Run one enumeration pass and reconcile workers with the manager's
    /// decisions.
    fn refresh(&mut self) {
        let enumeration = (self.enumerator)();
        let now = Instant::now();
        let events = self.manager.apply_enumeration(enumeration.displays, now);
        if events.is_empty() {
            return;
        }
        // A single pass can emit BOTH `Reattached` and `Responsive` for one id
        // (documented "existence first, then health" ordering). The sighting
        // event already spawns a fresh worker and issues the correct one-shot
        // (a restore write or an initial Get); the `Responsive` arm must then
        // only un-grey it — never retire that just-spawned worker, respawn a
        // second one, and re-learn the panel's power-on level (which would drop
        // the restore's in-flight write and clobber the user's saved level).
        let mut respawned: std::collections::BTreeSet<StableDisplayId> =
            std::collections::BTreeSet::new();
        for event in events {
            match event {
                ManagerEvent::Added { id } => {
                    self.spawn_for(&id);
                    self.dispatch_initial_get(&id);
                    respawned.insert(id);
                }
                ManagerEvent::Removed { id } => self.retire_worker(&id),
                ManagerEvent::Reattached { id, restore_level } => {
                    // A physical replug always gets a fresh controller.
                    self.retire_worker(&id);
                    self.spawn_for(&id);
                    match restore_level {
                        Some(pct) => self.dispatch_set(&id, Feature::Brightness, pct),
                        None => self.dispatch_initial_get(&id),
                    }
                    respawned.insert(id);
                }
                ManagerEvent::Responsive { id } => {
                    // Recovery from unresponsive: respawn unless a sighting event
                    // in THIS pass already did (dedupe), or the display has been
                    // abandoned after too many stuck cycles.
                    if !respawned.contains(&id)
                        && self.stuck_count.get(&id).copied().unwrap_or(0) < MAX_STUCK_RESPAWNS
                    {
                        self.retire_worker(&id);
                        self.spawn_for(&id);
                        self.dispatch_initial_get(&id);
                    }
                    self.notify(EngineNotification::DisplayResponsive(id));
                }
                // The manager never emits Unresponsive from an enumeration.
                ManagerEvent::Unresponsive { .. } => {}
            }
        }
        self.notify_displays_changed();
    }

    /// Ask the factory for a deferred opener and register a worker for `id`.
    ///
    /// The opener runs on the worker thread (see [`spawn_worker`]); a failed
    /// open is reported back as [`AckOutcome::OpenFailed`], which retires the
    /// dead handle. The worker is inserted unconditionally so any command
    /// dispatched between here and the open is either delivered or harmlessly
    /// dropped when the worker exits.
    fn spawn_for(&mut self, id: &StableDisplayId) {
        let opener = (self.factory)(id);
        let handle = spawn_worker(
            id.clone(),
            opener,
            self.cfg.write_min_gap,
            self.ack_tx.clone(),
        );
        self.workers.insert(id.clone(), handle);
    }

    /// Stop and detach any worker for `id` and clear its in-flight ops.
    ///
    /// The handle is dropped (not joined): an idle worker sees the channel
    /// disconnect and exits, and a leaked one is left running — either way the
    /// engine never blocks on the hot-plug path.
    fn retire_worker(&mut self, id: &StableDisplayId) {
        drop(self.workers.remove(id));
        self.clear_inflight_for(id);
        self.pending_learn.remove(id);
        self.poll_gets.remove(id);
    }

    /// Scale `pct` onto the display's brightness range and dispatch a write, if
    /// the display has a worker and is responsive.
    fn dispatch_set(&mut self, id: &StableDisplayId, feature: Feature, pct: u8) {
        if self.manager.is_responsive(id) != Some(true) {
            return;
        }
        let max = self.brightness_max.get(id).copied().unwrap_or(100);
        let raw = pct_to_raw(pct, max);
        let seq = self.next_seq();
        if let Some(handle) = self.workers.get(id)
            && handle
                .cmd_tx
                .send(crate::protocol::WorkerCommand::Set { feature, raw, seq })
                .is_ok()
        {
            self.inflight.insert(
                (id.clone(), InflightKey::Set(feature)),
                (seq, Instant::now()),
            );
        }
    }

    /// Dispatch an input-source switch, if the display has a responsive worker
    /// and `value` is in its probed allowed set.
    ///
    /// A code outside the probed list — or a display whose capabilities we have
    /// not learned — is rejected here (dropped) rather than reaching the wire, so
    /// the engine never asks a monitor to select an input it did not advertise.
    /// The raw code is sent verbatim (no percent scaling); the controller writes
    /// it without a verify-readback (ADR-0002).
    fn dispatch_input(&mut self, id: &StableDisplayId, value: u8) {
        let allowed = self
            .manager
            .capabilities_of(id)
            .is_some_and(|caps| caps.allows_input(value));
        if !allowed {
            return;
        }
        if self.manager.is_responsive(id) != Some(true) {
            return;
        }
        let seq = self.next_seq();
        let feature = Feature::InputSource;
        if let Some(handle) = self.workers.get(id)
            && handle
                .cmd_tx
                .send(crate::protocol::WorkerCommand::Set {
                    feature,
                    raw: u16::from(value),
                    seq,
                })
                .is_ok()
        {
            self.inflight.insert(
                (id.clone(), InflightKey::Set(feature)),
                (seq, Instant::now()),
            );
        }
    }

    /// Dispatch the one-shot brightness read used on add/recovery to learn the
    /// current hardware level, marking the display as awaiting that level.
    fn dispatch_initial_get(&mut self, id: &StableDisplayId) {
        let feature = Feature::Brightness;
        let seq = self.next_seq();
        if let Some(handle) = self.workers.get(id)
            && handle
                .cmd_tx
                .send(crate::protocol::WorkerCommand::Get { feature, seq })
                .is_ok()
        {
            self.inflight.insert(
                (id.clone(), InflightKey::Get(feature)),
                (seq, Instant::now()),
            );
            self.pending_learn.insert(id.clone());
        }
    }

    /// Process one worker ack.
    fn handle_ack(&mut self, ack: WorkerAck) {
        let id = ack.id;
        match ack.outcome {
            AckOutcome::Set { feature, seq } => {
                self.clear_inflight_match(&id, InflightKey::Set(feature), seq);
            }
            AckOutcome::Get {
                feature,
                seq,
                result,
            } => {
                // Gate ALL Get side-effects on the seq matching the tracked
                // in-flight Get (mirroring the write path). A stale ack from a
                // retired/superseded worker must not record calibration, and —
                // crucially — must not consume the `pending_learn` token, which
                // would drop the fresh worker's real reading.
                let is_fresh = self.clear_inflight_match(&id, InflightKey::Get(feature), seq);
                if is_fresh
                    && let Ok(range) = result
                    && feature == Feature::Brightness
                {
                    self.brightness_max.insert(id.clone(), range.max);
                    let pct = raw_to_pct(range.current, range.max);
                    let was_poll = self.poll_gets.remove(&id);
                    if self.pending_learn.remove(&id) {
                        // Initial learn: apply the reading only if the user has not
                        // set a level in the meantime (which would have cleared the
                        // pending flag).
                        self.manager.record_user_level(&id, pct);
                        self.notify_displays_changed();
                    } else if was_poll
                        && self
                            .manager
                            .user_level_of(&id)
                            .is_none_or(|known| pct.abs_diff(known) > 1)
                    {
                        // Poll read: the hardware drifted from what we last recorded,
                        // so something outside Duja changed it (physical buttons,
                        // another app). Our own writes match to within rounding and
                        // are suppressed here. Record it (so a replug restores the
                        // external value) and reflect it to the app. A *stale*
                        // initial-`Get` ack (not a poll) falls through and is ignored.
                        self.manager.record_user_level(&id, pct);
                        self.notify(EngineNotification::LevelRead {
                            id: id.clone(),
                            hw_pct: pct,
                        });
                    }
                }
            }
            AckOutcome::Panicked { key, seq } => {
                self.clear_inflight_match(&id, key, seq);
                self.mark_stuck(&id);
            }
            AckOutcome::OpenFailed => {
                // The deferred open failed on the worker thread. Drop the dead
                // handle and clear any state we optimistically recorded for it —
                // but only if the handle currently registered is the one that
                // exited, so a stale OpenFailed cannot retire a fresh worker
                // that already replaced it (respawn / reattach).
                if self
                    .workers
                    .get(&id)
                    .is_some_and(|handle| handle.join.is_finished())
                {
                    self.retire_worker(&id);
                }
            }
        }
    }

    /// Fire the watchdog for any display whose latest in-flight op has aged out.
    fn poll_watchdog(&mut self, now: Instant) {
        let mut stuck: Vec<StableDisplayId> = Vec::new();
        for ((id, _key), (_seq, dispatched)) in &self.inflight {
            if now.duration_since(*dispatched) >= self.cfg.watchdog_timeout {
                // BTreeMap iterates by key, so equal ids are adjacent.
                if stuck.last() != Some(id) {
                    stuck.push(id.clone());
                }
            }
        }
        for id in stuck {
            self.mark_stuck(&id);
        }
    }

    /// Mark `id` unresponsive (if newly so): detach its worker, clear its
    /// in-flight ops, and notify. Idempotent via the manager.
    fn mark_stuck(&mut self, id: &StableDisplayId) {
        if self.manager.mark_unresponsive(id).is_some() {
            let count = self.stuck_count.entry(id.clone()).or_insert(0);
            *count = count.saturating_add(1);
            // Drop the handle WITHOUT joining: a stuck thread must never be
            // joined; an already-exited (panicked) one just releases here.
            drop(self.workers.remove(id));
            self.clear_inflight_for(id);
            self.poll_gets.remove(id);
            self.notify(EngineNotification::DisplayUnresponsive(id.clone()));
            self.notify_displays_changed();
        }
    }

    /// Remove the in-flight entry for `(id, key)` iff its seq matches (a newer
    /// dispatch supersedes an older ack), returning whether it matched.
    ///
    /// `true` means the ack corresponds to the current in-flight op, so its
    /// side-effects are safe to apply; `false` means it is stale/superseded and
    /// callers must ignore it.
    fn clear_inflight_match(&mut self, id: &StableDisplayId, key: InflightKey, seq: u64) -> bool {
        let entry = (id.clone(), key);
        if let Some((tracked, _)) = self.inflight.get(&entry)
            && *tracked == seq
        {
            self.inflight.remove(&entry);
            true
        } else {
            false
        }
    }

    /// Drop every in-flight entry for `id`.
    fn clear_inflight_for(&mut self, id: &StableDisplayId) {
        self.inflight.retain(|(other, _), _| other != id);
    }

    /// Allocate the next monotonic sequence number.
    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    fn notify(&self, notification: EngineNotification) {
        let _ = self.notif_tx.send(notification);
    }

    fn notify_displays_changed(&self) {
        self.notify(EngineNotification::DisplaysChanged(
            self.manager.snapshots(),
        ));
    }

    /// On shutdown, ask every remaining (responsive) worker to stop and join
    /// it. Leaked workers were already removed from the map, so this never
    /// blocks on a stuck thread.
    fn shutdown_workers(&mut self) {
        for (_id, handle) in std::mem::take(&mut self.workers) {
            let _ = handle.cmd_tx.send(crate::protocol::WorkerCommand::Shutdown);
            let _ = handle.join.join();
        }
    }
}

/// What the select produced this iteration.
enum Wake {
    Cmd(Result<EngineCommand, crossbeam_channel::RecvError>),
    Ack(Result<WorkerAck, crossbeam_channel::RecvError>),
    Evt(Result<(), crossbeam_channel::RecvError>),
    Timeout,
}

/// The earlier of two optional instants.
fn min_opt(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

/// Scale a user percent (0–100) onto a raw feature range: `raw = pct*max/100`.
///
/// `pct` is clamped to 100 defensively (callers already clamp). Integer math
/// with no overflow: the clamped product fits in `u32` and the result is always
/// `<= max`.
fn pct_to_raw(pct: u8, max: u16) -> u16 {
    let scaled = u32::from(pct.min(100))
        .saturating_mul(u32::from(max))
        .checked_div(100)
        .unwrap_or(0);
    u16::try_from(scaled).unwrap_or(max)
}

/// Inverse of [`pct_to_raw`]: reflect a raw hardware value back to a percent.
fn raw_to_pct(current: u16, max: u16) -> u8 {
    let pct = u32::from(current)
        .saturating_mul(100)
        .checked_div(u32::from(max))
        .unwrap_or(0);
    u8::try_from(pct.min(100)).unwrap_or(100)
}

#[cfg(test)]
mod tests {
    use super::{min_opt, pct_to_raw, raw_to_pct};
    use std::time::{Duration, Instant};

    #[test]
    fn pct_to_raw_scales_and_never_overflows() {
        assert_eq!(pct_to_raw(0, 100), 0);
        assert_eq!(pct_to_raw(50, 100), 50);
        assert_eq!(pct_to_raw(100, 100), 100);
        assert_eq!(pct_to_raw(100, u16::MAX), u16::MAX);
        assert_eq!(pct_to_raw(50, u16::MAX), u16::MAX / 2);
        // Clamped input beyond 100 stays within range.
        assert!(pct_to_raw(200, 100) <= 100);
    }

    #[test]
    fn raw_to_pct_inverts_and_guards_zero_max() {
        assert_eq!(raw_to_pct(0, 100), 0);
        assert_eq!(raw_to_pct(50, 100), 50);
        assert_eq!(raw_to_pct(100, 100), 100);
        assert_eq!(raw_to_pct(u16::MAX, u16::MAX), 100);
        // A zero max must not divide-by-zero.
        assert_eq!(raw_to_pct(10, 0), 0);
    }

    #[test]
    fn min_opt_picks_the_earlier() {
        let a = Instant::now();
        let b = a.checked_add(Duration::from_secs(1)).unwrap();
        assert_eq!(min_opt(Some(a), Some(b)), Some(a));
        assert_eq!(min_opt(Some(b), Some(a)), Some(a));
        assert_eq!(min_opt(Some(a), None), Some(a));
        assert_eq!(min_opt(None, Some(b)), Some(b));
        assert_eq!(min_opt(None, None), None);
    }
}
