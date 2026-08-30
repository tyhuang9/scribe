use std::time::{Duration, Instant};

use super::{OverlaySnapshot, controller::OverlayPhase};

pub(super) const FRAME_INTERVAL: Duration = Duration::from_millis(16);
pub(super) const METER_INTERVAL: Duration = Duration::from_millis(33);
const PREPARING_FADE: Duration = Duration::from_millis(120);
const SEMANTIC_CROSSFADE: Duration = Duration::from_millis(140);
const ERROR_CROSSFADE: Duration = Duration::from_millis(120);
const SUCCESS_DWELL: Duration = Duration::from_millis(650);
const SUCCESS_FADE_OUT: Duration = Duration::from_millis(140);

/// Immutable source/target snapshots let the native worker retarget to new
/// app semantics without queuing obsolete visual states.
#[derive(Clone, Debug)]
pub(super) struct RenderPlan {
    pub target: OverlaySnapshot,
    pub previous: Option<OverlaySnapshot>,
    pub target_opacity: u8,
    pub previous_opacity: u8,
    pub visible: bool,
    pub animated: bool,
    pub reserve_cancel_region: bool,
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
    last_meter_update: Option<Instant>,
}

impl OverlayTransitionEngine {
    pub(super) fn tick(&mut self, now: Instant, reduced_motion: bool) -> Option<RenderPlan> {
        self.advance(self.displayed.clone(), now, reduced_motion)
    }

    pub(super) fn advance(
        &mut self,
        incoming: Option<OverlaySnapshot>,
        now: Instant,
        reduced_motion: bool,
    ) -> Option<RenderPlan> {
        if let Some(mut incoming) = incoming {
            if self.success_dismissed
                && self.active.is_none()
                && incoming.state.phase == OverlayPhase::Success
            {
                return None;
            }
            if incoming.state.phase != OverlayPhase::Success {
                self.success_since = None;
                self.success_dismissed = false;
            }
            self.smooth_meter(&mut incoming, now);
            let changed = self.displayed.as_ref().is_none_or(|old| {
                old.state.mode != incoming.state.mode
                    || old.state.phase != incoming.state.phase
                    || old.state.transcript.revision != incoming.state.transcript.revision
                    || old.state.notice != incoming.state.notice
                    || old.state.error != incoming.state.error
            });
            if changed {
                let previous = self
                    .active
                    .as_ref()
                    .map(|active| active.target.clone())
                    .or_else(|| self.displayed.clone());
                let entering = previous.as_ref().is_none_or(|old| !old.requested_visible)
                    && incoming.requested_visible;
                let duration = if reduced_motion {
                    Duration::ZERO
                } else if entering && incoming.state.phase == OverlayPhase::Preparing {
                    PREPARING_FADE
                } else if incoming.state.phase == OverlayPhase::Error {
                    ERROR_CROSSFADE
                } else {
                    SEMANTIC_CROSSFADE
                };
                self.active = (duration > Duration::ZERO).then(|| ActiveTransition {
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
                if incoming.state.phase == OverlayPhase::Success {
                    self.success_since = Some(now);
                }
            }
            self.displayed = Some(incoming);
        } else if let Some(displayed) = self.displayed.clone() {
            if displayed.state.phase == OverlayPhase::Success && !reduced_motion {
                self.active = Some(ActiveTransition {
                    previous: Some(displayed.clone()),
                    target: displayed,
                    started: now,
                    duration: SUCCESS_FADE_OUT,
                    kind: TransitionKind::Exit,
                });
            } else {
                self.displayed = None;
                self.active = None;
                return None;
            }
        }
        if let (Some(success_since), Some(displayed)) = (self.success_since, self.displayed.clone())
            && displayed.state.phase == OverlayPhase::Success
            && !self.success_dismissed
            && !reduced_motion
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
        let displayed = self.displayed.clone()?;
        let mut plan = RenderPlan {
            target: displayed,
            previous: None,
            target_opacity: 255,
            previous_opacity: 0,
            visible: !self.success_dismissed,
            animated: false,
            reserve_cancel_region: false,
        };
        if let Some(active) = &self.active {
            let fraction = (now.duration_since(active.started).as_secs_f32()
                / active.duration.as_secs_f32())
            .clamp(0.0, 1.0);
            if fraction >= 1.0 {
                let exit = active.kind == TransitionKind::Exit;
                self.active = None;
                if exit {
                    self.displayed = None;
                    return None;
                }
            } else {
                let alpha = (fraction * 255.0).round() as u8;
                plan.previous = active.previous.clone();
                plan.animated = true;
                plan.reserve_cancel_region = active
                    .previous
                    .as_ref()
                    .is_some_and(|old| old.control_requested)
                    && !active.target.control_requested;
                if active.kind == TransitionKind::Exit {
                    plan.target_opacity = 0;
                    plan.previous_opacity = 255 - alpha;
                    plan.visible = true;
                } else {
                    plan.target_opacity = alpha;
                    plan.previous_opacity = 255 - alpha;
                }
            }
        }
        Some(plan)
    }

    fn smooth_meter(&mut self, snapshot: &mut OverlaySnapshot, now: Instant) {
        let dt = self
            .last_meter_update
            .map(|previous| now.duration_since(previous))
            .unwrap_or_default();
        self.last_meter_update = Some(now);
        let recording = snapshot.state.phase == OverlayPhase::Listening;
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
                snapshot.state.audio_level.rms
            } else {
                0.0
            },
        );
        smooth(
            &mut self.smoothed_peak,
            if recording {
                snapshot.state.audio_level.peak
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
    #[test]
    fn retargets_to_newest_semantics() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(Some(snapshot(OverlayPhase::Preparing)), at, false);
        let _ = engine.advance(
            Some(snapshot(OverlayPhase::Listening)),
            at + Duration::from_millis(20),
            false,
        );
        let plan = engine
            .advance(
                Some(snapshot(OverlayPhase::Finalizing)),
                at + Duration::from_millis(30),
                false,
            )
            .unwrap();
        assert_eq!(plan.target.state.phase, OverlayPhase::Finalizing);
        assert_eq!(plan.previous.unwrap().state.phase, OverlayPhase::Listening);
    }
    #[test]
    fn reduced_motion_swaps_without_animation() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(Some(snapshot(OverlayPhase::Preparing)), at, true);
        let plan = engine
            .advance(Some(snapshot(OverlayPhase::Listening)), at, true)
            .unwrap();
        assert!(!plan.animated);
        assert_eq!(plan.target_opacity, 255);
    }
    #[test]
    fn capture_stop_disables_and_reserves_cancel_region() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(Some(snapshot(OverlayPhase::Listening)), at, false);
        let plan = engine
            .advance(
                Some(snapshot(OverlayPhase::Finalizing)),
                at + Duration::from_millis(30),
                false,
            )
            .unwrap();
        assert!(!plan.target.control_requested);
        assert!(plan.reserve_cancel_region);
    }
    #[test]
    fn meter_attacks_then_decays_after_capture_stops() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let mut input = snapshot(OverlayPhase::Listening);
        input.state.audio_level.rms = 1.0;
        let _ = engine.advance(Some(input), at, false);
        let plan = engine
            .advance(
                Some(snapshot(OverlayPhase::Listening)),
                at + Duration::from_millis(60),
                false,
            )
            .unwrap();
        assert!(plan.target.state.audio_level.rms > 0.5);
        let plan = engine
            .advance(
                Some(snapshot(OverlayPhase::Finalizing)),
                at + Duration::from_millis(140),
                false,
            )
            .unwrap();
        assert!(plan.target.state.audio_level.rms < 0.5);
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
    fn error_crossfade_uses_the_shorter_duration() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(Some(snapshot(OverlayPhase::Listening)), at, false);
        let plan = engine
            .advance(
                Some(snapshot(OverlayPhase::Error)),
                at + Duration::from_millis(119),
                false,
            )
            .unwrap();
        assert!(plan.animated);
        let plan = engine
            .advance(
                Some(snapshot(OverlayPhase::Error)),
                at + Duration::from_millis(240),
                false,
            )
            .unwrap();
        assert!(!plan.animated);
    }

    #[test]
    fn success_dwells_then_fades_out() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(Some(snapshot(OverlayPhase::Success)), at, false);
        let plan = engine.tick(at + Duration::from_millis(790), false).unwrap();
        assert!(plan.animated);
        assert!(plan.visible);
        assert!(
            engine
                .tick(at + Duration::from_millis(931), false)
                .is_none()
        );
    }

    #[test]
    fn semantic_transition_never_changes_the_fixed_shell_bounds() {
        let at = Instant::now();
        let mut engine = OverlayTransitionEngine::default();
        let _ = engine.advance(Some(snapshot(OverlayPhase::Preparing)), at, false);
        let plan = engine
            .advance(
                Some(snapshot(OverlayPhase::Finalizing)),
                at + Duration::from_millis(30),
                false,
            )
            .unwrap();
        assert_eq!(
            plan.target.display_bounds,
            plan.previous.unwrap().display_bounds
        );
        let bounds = plan.target.display_bounds.unwrap();
        assert_eq!((bounds.width, bounds.height), (200, 62));
    }
}
