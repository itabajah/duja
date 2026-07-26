//! The slider → engine forwarding seam: the one path every user level change
//! takes out of the UI layer, made injectable so its contract is testable.
//!
//! # Why this is a module and not two lines inside `set_user_level`
//!
//! P4 gate Finding 1 was a **shipped defect**. A leading-edge throttle in the
//! UI/tray layer swallowed the *final* sample of a slider drag: the hardware
//! stayed stranded at a mid-drag brightness while the slider, the overlay and
//! the persisted state all showed the value the user released on. The throttle
//! was deleted and the engine's
//! [`write_min_gap`](duja_app::EngineConfig::write_min_gap) last-wins coalescer
//! became the single pacing authority — it bounds the hardware write rate *and*
//! guarantees the last value of a burst lands.
//!
//! That regression was pinned only at the engine, so the UI side was free to
//! re-grow a throttle with every test still green. [`LevelForwarder`] is the
//! forwarding path extracted behind a [`LevelSink`]: production drives
//! [`EngineLevelSink`] (one `SetUserLevel` per write), the tests below drive a
//! recording fake. The **final-value contract** is asserted here, so a re-added
//! UI-side throttle fails a test instead of shipping.
//!
//! Same shape as the other injection seams in this binary — `GammaSink`
//! (`gamma`), `IpcBridge` (`ipc`), `HotkeyRegistrar` (`hotkey`): a narrow trait,
//! a real implementation, and a fake in the tests.

// RATIONALE: the forwarder and its sink are consumed only by the Windows tray
// (`tray::state::AppState`), but they stay cross-platform so the final-value
// regression test runs on every CI OS lane; the dead-code allow applies only
// where no consumer exists. Mirrors `gamma.rs`.
#![cfg_attr(not(windows), allow(dead_code))]

use crossbeam_channel::Sender;

use duja_app::EngineCommand;
use duja_core::id::StableDisplayId;

/// Where one display's resolved hardware level is delivered.
///
/// Abstracts the engine command channel so [`LevelForwarder`]'s contract is
/// testable with a fake. The real implementation is [`EngineLevelSink`].
pub(crate) trait LevelSink {
    /// Deliver `pct` (0–100) as the hardware level for display `id`.
    fn set_user_level(&mut self, id: &StableDisplayId, pct: u8);
}

/// The real sink: one [`EngineCommand::SetUserLevel`] per write.
pub(crate) struct EngineLevelSink {
    tx: Sender<EngineCommand>,
}

impl EngineLevelSink {
    /// Build a sink over the engine's command channel.
    pub(crate) fn new(tx: Sender<EngineCommand>) -> Self {
        EngineLevelSink { tx }
    }
}

impl LevelSink for EngineLevelSink {
    fn set_user_level(&mut self, id: &StableDisplayId, pct: u8) {
        // A closed channel means the engine is already down (teardown in
        // progress); the UI stays alive and the value is simply not written.
        let _ = self.tx.send(EngineCommand::SetUserLevel {
            id: id.clone(),
            pct,
        });
    }
}

/// Owns a [`LevelSink`] and forwards every write handed to it.
///
/// # Contract
///
/// **Every call is forwarded, unconditionally and in order.** There is no
/// throttle, no debounce, no drop and no coalescing on this side of the seam —
/// pacing is the engine's job (`write_min_gap`, last-wins), because only the
/// engine's coalescer keeps the *final* value of a drag. Adding rate limiting
/// here re-creates the P4 defect described in the module docs.
pub(crate) struct LevelForwarder<S: LevelSink> {
    sink: S,
}

impl<S: LevelSink> LevelForwarder<S> {
    /// Forward onto `sink`.
    pub(crate) fn new(sink: S) -> Self {
        LevelForwarder { sink }
    }

    /// Forward one user action's per-member hardware writes (a mirrored group
    /// fans out to several members; a lone display is a single write).
    pub(crate) fn forward(&mut self, writes: &[(StableDisplayId, u8)]) {
        for (id, pct) in writes {
            self.sink.set_user_level(id, *pct);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Every write the forwarder delivered, shared with the test so the fake can
    /// stay owned by the forwarder.
    type Log = Arc<Mutex<Vec<(StableDisplayId, u8)>>>;

    /// A fake sink that records every delivered write, in order.
    struct RecordingSink {
        log: Log,
    }

    impl LevelSink for RecordingSink {
        fn set_user_level(&mut self, id: &StableDisplayId, pct: u8) {
            self.log.lock().unwrap().push((id.clone(), pct));
        }
    }

    fn forwarder() -> (LevelForwarder<RecordingSink>, Log) {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        (LevelForwarder::new(RecordingSink { log: log.clone() }), log)
    }

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
    }

    #[test]
    fn drag_burst_delivers_the_final_value_to_the_sink() {
        // REGRESSION (P4 gate Finding 1, pinned at the tray layer): a slider drag
        // delivers a back-to-back burst of samples in a few milliseconds. The
        // value the user RELEASED on must reach the engine — a leading-edge
        // throttle here forwards 50 and swallows everything up to 70, stranding
        // the hardware mid-drag while the slider/overlay/state all read 70.
        let (mut fwd, log) = forwarder();
        for pct in 50..=70u8 {
            fwd.forward(&[(id("A"), pct)]);
        }

        let seen = log.lock().unwrap();
        assert_eq!(
            seen.last().cloned(),
            Some((id("A"), 70)),
            "the final value of a drag must be the last write the sink sees; \
             a UI-side throttle strands the hardware at an intermediate level"
        );
    }

    #[test]
    fn no_ui_side_coalescing_every_sample_is_forwarded() {
        // The other half of the contract: pacing belongs to the engine's
        // `write_min_gap` (last-wins) coalescer, so this side drops nothing. A
        // dropped sample count is the fingerprint of a re-added throttle.
        let (mut fwd, log) = forwarder();
        for pct in 50..=70u8 {
            fwd.forward(&[(id("A"), pct)]);
        }

        let seen = log.lock().unwrap();
        let values: Vec<u8> = seen.iter().map(|(_, pct)| *pct).collect();
        assert_eq!(
            values,
            (50..=70u8).collect::<Vec<u8>>(),
            "every sample must be forwarded, in order, with no UI-side throttle"
        );
    }

    #[test]
    fn mirrored_fan_out_lands_the_final_value_on_every_member() {
        // A merged clone group (#66) fans one user level out to several members.
        // The final-value guarantee is per member: each must end at 70.
        let (mut fwd, log) = forwarder();
        for pct in 50..=70u8 {
            fwd.forward(&[(id("A"), pct), (id("B"), pct)]);
        }

        let seen = log.lock().unwrap();
        for member in ["A", "B"] {
            let last = seen.iter().rfind(|(who, _)| *who == id(member)).cloned();
            assert_eq!(
                last,
                Some((id(member), 70)),
                "member {member} must end the drag at the released value"
            );
        }
    }
}
