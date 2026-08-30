use std::time::{Duration, Instant};

use super::super::controller::OverlayPhase;
use super::OverlaySnapshot;

pub(super) const FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub(super) const METER_INTERVAL: Duration = Duration::from_millis(33);
const PREPARING_FADE: Duration = Duration::from_millis(120);
const SEMANTIC_CROSSFADE: Duration = Duration::from_millis(140);
const ERROR_CROSSFADE: Duration = Duration::from_millis(120);
const SUCCESS_DWELL: Duration = Duration::from_millis(650);
const SUCCESS_FADE_OUT: Duration = Duration::from_millis(140);

#[derive(Clone, Debug)]
pub(super) struct RenderPlan {
    pub target: OverlaySnapshot,
    pub previous: Option<OverlaySnapshot>,
    pub target_opacity: u8,
    pub previous_opacity: u8,
    pub animated: bool,
    pub exit_fade: bool,
    pub reserve_cancel_region: bool,
}

/// The worker must not confuse a paused renderer with an explicit dismissal.
/// In particular, a dismissed Success must never fall back to a raw snapshot.
#[derive(Clone, Debug)]
pub(super) enum TransitionStep {
    Render(Box<RenderPlan>),
    Idle,
    Hidden,
}

#[derive(Clone, Debug)]
struct ActiveTransition {
    previous: Option<OverlaySnapshot>,
    target: OverlaySnapshot,
    started: Instant,
    duration: Duration,
    kind: TransitionKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransitionKind {
    Enter,
    Semantic,
    Exit,
}

#[derive(Default)]
pub(super) struct OverlayTransitionEngine {
    displayed: Option<OverlaySnapshot>,
    active: Option<ActiveTransition>,
    success_since: Option<Instant>,
    success_dismissed: bool,
    smoothed_rms: f32,
    smoothed_peak: f32,
    meter_target_rms: f32,
    meter_target_peak: f32,
    last_meter_update: Option<Instant>,
}

impl OverlayTransitionEngine {
    pub(super) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(super) fn advance(
        &mut self,
        incoming: OverlaySnapshot,
        now: Instant,
        reduced_motion: bool,
    ) -> TransitionStep {
        if !incoming.requested_visible {
            self.clear();
            return TransitionStep::Hidden;
        }
        if self.success_dismissed && incoming.state.phase == OverlayPhase::Success {
            if reduced_motion {
                self.clear();
                return TransitionStep::Hidden;
            }
            return self.plan_at(now, true);
        }

        let mut incoming = incoming;
        self.smooth_meter(&mut incoming, now);
        if incoming.state.phase != OverlayPhase::Success {
            self.success_since = None;
            self.success_dismissed = false;
        }
        let changed = self.displayed.as_ref().is_none_or(|old| {
            old.state.mode != incoming.state.mode
                || old.state.phase != incoming.state.phase
                || old.state.transcript.revision != incoming.state.transcript.revision
                || old.state.notice != incoming.state.notice
                || old.state.error != incoming.state.error
        });
        if reduced_motion {
            self.active = None;
        } else if changed {
            let previous = self
                .active
                .as_ref()
                .map(|active| active.target.clone())
                .or_else(|| self.displayed.clone());
            let entering = previous.as_ref().is_none_or(|old| !old.requested_visible);
            let duration = if entering && incoming.state.phase == OverlayPhase::Preparing {
                PREPARING_FADE
            } else if incoming.state.phase == OverlayPhase::Error {
                ERROR_CROSSFADE
            } else {
                SEMANTIC_CROSSFADE
            };
            self.active = Some(ActiveTransition {
                previous,
                target: incoming.clone(),
                started: now,
                duration,
                kind: if entering {
                    TransitionKind::Enter
                } else {
                    TransitionKind::Semantic
                },
            });
        }
        if changed && incoming.state.phase == OverlayPhase::Success {
            self.success_since = Some(now);
        }
        self.displayed = Some(incoming);
        self.plan_at(now, true)
    }

    pub(super) fn tick(&mut self, now: Instant, reduced_motion: bool) -> TransitionStep {
        if self.success_dismissed && self.active.is_none() {
            return TransitionStep::Hidden;
        }
        if self.displayed.is_none() {
            return TransitionStep::Idle;
        }
        if reduced_motion {
            if self
                .displayed
                .as_ref()
                .is_some_and(|displayed| displayed.state.phase == OverlayPhase::Success)
                && !self.success_dismissed
                && self
                    .success_since
                    .is_some_and(|since| now.duration_since(since) >= SUCCESS_DWELL)
            {
                self.active = None;
                self.displayed = None;
                self.success_dismissed = true;
                return TransitionStep::Hidden;
            }
            return if self.active.take().is_some() {
                self.plan_at(now, true)
            } else {
                TransitionStep::Idle
            };
        }
        if self.active.is_none()
            && self.displayed.as_ref().is_some_and(|displayed| {
                matches!(
                    displayed.state.phase,
                    OverlayPhase::Listening | OverlayPhase::Finalizing
                )
            })
        {
            let mut displayed = self
                .displayed
                .clone()
                .expect("displayed listening snapshot");
            self.smooth_meter(&mut displayed, now);
            self.displayed = Some(displayed);
            return self.plan_at(now, true);
        }
        if let (Some(success_since), Some(displayed)) = (self.success_since, self.displayed.clone())
            && displayed.state.phase == OverlayPhase::Success
            && !self.success_dismissed
            && self.active.is_none()
            && now.duration_since(success_since) >= SEMANTIC_CROSSFADE + SUCCESS_DWELL
        {
            self.active = Some(ActiveTransition {
                previous: Some(displayed.clone()),
                target: displayed,
                started: now,
                duration: SUCCESS_FADE_OUT,
                kind: TransitionKind::Exit,
            });
            self.success_dismissed = true;
        }
        self.plan_at(now, false)
    }

    fn plan_at(&mut self, now: Instant, force_submit: bool) -> TransitionStep {
        let Some(displayed) = self.displayed.clone() else {
            return if self.success_dismissed {
                TransitionStep::Hidden
            } else {
                TransitionStep::Idle
            };
        };
        let mut plan = RenderPlan {
            target: displayed,
            previous: None,
            target_opacity: 255,
            previous_opacity: 0,
            animated: false,
            exit_fade: false,
            reserve_cancel_region: false,
        };
        let Some(active) = &self.active else {
            return if force_submit {
                TransitionStep::Render(Box::new(plan))
            } else {
                TransitionStep::Idle
            };
        };
        let fraction = (now.duration_since(active.started).as_secs_f32()
            / active.duration.as_secs_f32())
        .clamp(0.0, 1.0);
        if fraction >= 1.0 {
            let exit = active.kind == TransitionKind::Exit;
            self.active = None;
            if exit {
                self.displayed = None;
                return TransitionStep::Hidden;
            }
            return TransitionStep::Render(Box::new(plan));
        }
        let alpha = (fraction * 255.0).round() as u8;
        plan.previous = active.previous.clone();
        plan.animated = true;
        plan.reserve_cancel_region = active
            .previous
            .as_ref()
            .is_some_and(|old| old.control_requested)
            && !active.target.control_requested;
        if active.kind == TransitionKind::Exit {
            plan.exit_fade = true;
            plan.target_opacity = 0;
            plan.previous_opacity = 255 - alpha;
        } else {
            plan.target_opacity = alpha;
            plan.previous_opacity = 255 - alpha;
        }
        TransitionStep::Render(Box::new(plan))
    }

    fn clear(&mut self) {
        self.displayed = None;
        self.active = None;
        self.success_since = None;
        self.last_meter_update = None;
        self.smoothed_rms = 0.0;
        self.smoothed_peak = 0.0;
        self.meter_target_rms = 0.0;
        self.meter_target_peak = 0.0;
    }

    fn smooth_meter(&mut self, snapshot: &mut OverlaySnapshot, now: Instant) {
        let dt = self
            .last_meter_update
            .map(|previous| now.duration_since(previous))
            .unwrap_or_default();
        self.last_meter_update = Some(now);
        let recording = snapshot.state.phase == OverlayPhase::Listening;
        if recording {
            self.meter_target_rms = snapshot.state.audio_level.rms;
            self.meter_target_peak = snapshot.state.audio_level.peak;
        }
        // The first sample intentionally snaps, avoiding a visible empty-meter
        // delay immediately after capture begins.
        let smooth = |current: &mut f32, target: f32| {
            let millis = dt.as_secs_f32() * 1000.0;
            let tau = if !recording {
                80.0
            } else if target > *current {
                60.0
            } else {
                140.0
            };
            let amount = if millis == 0.0 {
                1.0
            } else {
                1.0 - (-millis / tau).exp()
            };
            *current += (target - *current) * amount;
        };
        smooth(
            &mut self.smoothed_rms,
            if recording {
                self.meter_target_rms
            } else {
                0.0
            },
        );
        smooth(
            &mut self.smoothed_peak,
            if recording {
                self.meter_target_peak
            } else {
                0.0
            },
        );
        snapshot.state.audio_level.rms = self.smoothed_rms;
        snapshot.state.audio_level.peak = self.smoothed_peak;
    }

    pub(super) fn next_wait(&self, now: Instant, progress_active: bool) -> Duration {
        if self.active.is_some() {
            FRAME_INTERVAL
        } else if let Some(success_since) = self.success_since
            && !self.success_dismissed
        {
            (SEMANTIC_CROSSFADE + SUCCESS_DWELL)
                .saturating_sub(now.duration_since(success_since))
                .min(Duration::from_millis(500))
        } else if progress_active {
            METER_INTERVAL
        } else {
            Duration::from_millis(500)
        }
    }

    pub(super) fn health_snapshot(&self) -> Option<OverlaySnapshot> {
        if self.success_dismissed && self.active.is_none() {
            return None;
        }
        let mut snapshot = self.displayed.clone()?;
        if self.active.as_ref().is_some_and(|active| {
            active
                .previous
                .as_ref()
                .is_some_and(|previous| previous.control_requested)
                && !active.target.control_requested
        }) {
            snapshot.control_requested = true;
        }
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{
        controller::{OverlayMode, OverlayViewState},
        platform::OverlayWindowBounds,
    };

    fn snapshot(phase: OverlayPhase) -> OverlaySnapshot {
        OverlaySnapshot {
            state: OverlayViewState {
                mode: OverlayMode::Minimal,
                phase,
                ..Default::default()
            },
            dark_mode: false,
            dpi: 96,
            display_bounds: Some(OverlayWindowBounds {
                x: 0,
                y: 0,
                width: 200,
                height: 62,
            }),
            control_bounds: Some(OverlayWindowBounds {
                x: 208,
                y: 9,
                width: 44,
                height: 44,
            }),
            requested_visible: phase != OverlayPhase::Hidden,
            control_requested: matches!(phase, OverlayPhase::Preparing | OverlayPhase::Listening),
        }
    }

    fn render(step: TransitionStep) -> RenderPlan {
        match step {
            TransitionStep::Render(plan) => *plan,
            other => panic!("expected render, got {other:?}"),
        }
    }

    #[test]
    fn retargets_to_newest_semantics() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Preparing), at, false);
        let _ = engine.advance(
            snapshot(OverlayPhase::Listening),
            at + Duration::from_millis(20),
            false,
        );
        let plan = render(engine.advance(
            snapshot(OverlayPhase::Finalizing),
            at + Duration::from_millis(30),
            false,
        ));
        assert_eq!(plan.target.state.phase, OverlayPhase::Finalizing);
        assert_eq!(plan.previous.unwrap().state.phase, OverlayPhase::Listening);
    }

    #[test]
    fn preparing_enters_from_transparent_and_reaches_full_opacity_at_120ms() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let first = render(engine.advance(snapshot(OverlayPhase::Preparing), at, false));
        assert!(first.previous.is_none());
        assert_eq!(first.target_opacity, 0);
        let midway = render(engine.tick(at + Duration::from_millis(60), false));
        assert!(midway.target_opacity > 0 && midway.target_opacity < 255);
        let complete = render(engine.tick(at + Duration::from_millis(120), false));
        assert!(!complete.animated);
        assert_eq!(complete.target_opacity, 255);
    }

    #[test]
    fn terminal_error_and_success_submit_a_full_completion_frame() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Listening), at, false);
        let _ = engine.advance(
            snapshot(OverlayPhase::Error),
            at + Duration::from_millis(1),
            false,
        );
        let error = render(engine.tick(at + Duration::from_millis(121), false));
        assert_eq!(error.target.state.phase, OverlayPhase::Error);
        assert!(!error.animated);

        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Listening), at, false);
        let _ = engine.advance(
            snapshot(OverlayPhase::Success),
            at + Duration::from_millis(1),
            false,
        );
        let success = render(engine.tick(at + Duration::from_millis(141), false));
        assert_eq!(success.target.state.phase, OverlayPhase::Success);
        assert!(!success.animated);
    }

    #[test]
    fn success_exit_is_marked_as_a_text_free_fade() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Success), at, false);
        let _ = engine.tick(at + Duration::from_millis(140), false);
        let exit = render(engine.tick(at + Duration::from_millis(790), false));
        assert!(exit.exit_fade);
        assert!(exit.animated);
        assert_eq!(exit.target_opacity, 0);
        assert_eq!(exit.previous_opacity, 255);
    }

    #[test]
    fn reduced_motion_success_dwells_then_hides_and_stays_dismissed() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Success), at, true);
        assert!(matches!(
            engine.tick(at + Duration::from_millis(649), true),
            TransitionStep::Idle
        ));
        assert!(matches!(
            engine.tick(at + Duration::from_millis(650), true),
            TransitionStep::Hidden
        ));
        assert!(matches!(
            engine.advance(
                snapshot(OverlayPhase::Success),
                at + Duration::from_millis(651),
                true
            ),
            TransitionStep::Hidden
        ));
    }

    #[test]
    fn dismissed_success_stays_hidden_when_the_app_repeats_success() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Success), at, false);
        let _ = engine.tick(at + Duration::from_millis(140), false);
        let _ = engine.tick(at + Duration::from_millis(790), false);
        assert!(matches!(
            engine.tick(at + Duration::from_millis(931), false),
            TransitionStep::Hidden
        ));
        assert!(matches!(
            engine.advance(
                snapshot(OverlayPhase::Hidden),
                at + Duration::from_millis(935),
                false
            ),
            TransitionStep::Hidden
        ));
        assert!(matches!(
            engine.advance(
                snapshot(OverlayPhase::Success),
                at + Duration::from_millis(940),
                false
            ),
            TransitionStep::Hidden
        ));
        assert_eq!(
            engine.next_wait(at + Duration::from_millis(940), false),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn hide_interrupts_an_active_transition_without_fast_hidden_ticks() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Preparing), at, false);
        assert!(matches!(
            engine.advance(
                snapshot(OverlayPhase::Hidden),
                at + Duration::from_millis(20),
                false
            ),
            TransitionStep::Hidden
        ));
        assert!(matches!(
            engine.tick(at + Duration::from_millis(30), false),
            TransitionStep::Idle
        ));
        assert_eq!(
            engine.next_wait(at + Duration::from_millis(30), false),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn reduced_motion_snaps_an_active_crossfade_to_the_target() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Preparing), at, false);
        let plan = render(engine.advance(
            snapshot(OverlayPhase::Listening),
            at + Duration::from_millis(20),
            true,
        ));
        assert_eq!(plan.target.state.phase, OverlayPhase::Listening);
        assert!(!plan.animated);
        assert_eq!(plan.target_opacity, 255);
    }

    #[test]
    fn capture_stop_disables_and_reserves_cancel_region() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Listening), at, false);
        let plan = render(engine.advance(
            snapshot(OverlayPhase::Finalizing),
            at + Duration::from_millis(30),
            false,
        ));
        assert!(!plan.target.control_requested);
        assert!(plan.reserve_cancel_region);
    }

    #[test]
    fn health_snapshot_treats_the_reserved_cancel_surface_as_expected() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Listening), at, false);
        let _ = engine.advance(
            snapshot(OverlayPhase::Finalizing),
            at + Duration::from_millis(30),
            false,
        );
        let health = engine.health_snapshot().expect("visible transition state");
        assert!(health.control_requested);
        assert!(health.requested_visible);
    }

    #[test]
    fn meter_attacks_then_decays_after_capture_stops() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let mut input = snapshot(OverlayPhase::Listening);
        input.state.audio_level.rms = 1.0;
        let _ = engine.advance(input, at, false);
        let plan = render(engine.advance(
            snapshot(OverlayPhase::Listening),
            at + Duration::from_millis(60),
            false,
        ));
        assert!(plan.target.state.audio_level.rms > 0.5);
        let plan = render(engine.advance(
            snapshot(OverlayPhase::Finalizing),
            at + Duration::from_millis(140),
            false,
        ));
        assert!(plan.target.state.audio_level.rms < 0.5);
    }

    #[test]
    fn meter_advances_without_new_snapshots_then_decays_after_capture_stops() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let mut listening = snapshot(OverlayPhase::Listening);
        listening.state.audio_level.rms = 1.0;
        listening.state.audio_level.peak = 1.0;
        let _ = engine.advance(listening, at, false);
        let _ = engine.tick(at + Duration::from_millis(140), false);
        let attacked = render(engine.tick(at + Duration::from_millis(173), false));
        assert!(attacked.target.state.audio_level.rms > 0.5);

        let mut finalizing = snapshot(OverlayPhase::Finalizing);
        finalizing.state.audio_level.rms = 0.0;
        finalizing.state.audio_level.peak = 0.0;
        let _ = engine.advance(finalizing, at + Duration::from_millis(174), false);
        let decayed = render(engine.tick(at + Duration::from_millis(314), false));
        assert!(decayed.target.state.audio_level.rms < attacked.target.state.audio_level.rms);
    }

    #[test]
    fn static_worker_waits_without_busy_spinning() {
        let engine = OverlayTransitionEngine::default();
        assert_eq!(
            engine.next_wait(Instant::now(), false),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn semantic_transition_never_changes_the_fixed_shell_bounds() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(snapshot(OverlayPhase::Preparing), at, false);
        let plan = render(engine.advance(
            snapshot(OverlayPhase::Finalizing),
            at + Duration::from_millis(30),
            false,
        ));
        assert_eq!(
            plan.target.display_bounds,
            plan.previous.unwrap().display_bounds
        );
        let bounds = plan.target.display_bounds.unwrap();
        assert_eq!((bounds.width, bounds.height), (200, 62));
    }
}
