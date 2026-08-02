use bevy::app::{App, Plugin, Update};
use bevy::ecs::entity::Entity;
use bevy::ecs::message::MessageReader;
use bevy::ecs::query::{With, Without};
use bevy::ecs::schedule::IntoScheduleConfigs as _;
use bevy::ecs::system::{Commands, Local, Populated, Res, Single};
use bevy::math::IRect;
use bevy::time::Time;
use std::time::{Duration, Instant};
use tracing::{Level, instrument};

use crate::commands::{Command, Direction, Operation};
use crate::config::Config;
use crate::config::swipe::SwipeGestureDirection;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::params::{ActiveDisplay, Windows};
use crate::ecs::{
    ActiveWorkspaceMarker, MissionControlActive, Position, Scrolling, SendMessageTrigger,
};
use crate::errors::Result;
use crate::events::Event;
use crate::manager::{Window, WindowManager};
use crate::platform::Modifiers;

pub struct ScrollEventsPlugin;

impl Plugin for ScrollEventsPlugin {
    fn build(&self, app: &mut App) {
        let mission_control_inactive = |mission_control: Option<Res<MissionControlActive>>| {
            mission_control.is_none_or(|active| !active.0)
        };

        app.add_systems(
            Update,
            (
                vertical_swipe_gesture.run_if(mission_control_inactive),
                (
                    swipe_gesture.run_if(mission_control_inactive),
                    apply_inertia,
                    apply_snap_force,
                    scrolling_integrator,
                    apply_scrolling_constraints,
                    swiping_timeout,
                )
                    .chain(),
            ),
        );
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn swipe_gesture(
    mut messages: MessageReader<Event>,
    active_display: ActiveDisplay,
    mut active_workspace: Single<
        (Entity, &Position, Option<&mut Scrolling>),
        With<ActiveWorkspaceMarker>,
    >,
    time: Res<Time>,
    config: Res<Config>,
    mut commands: Commands,
) {
    // Canvas displays: scroll gestures belong to the Canvas engine (see
    // canvas_scroll_gesture). Leave the strip machinery untouched.
    if config.is_canvas_display(active_display.display().uuid()) {
        return;
    }
    let swipe_sensitivity = config.swipe_sensitivity();
    let direction_modifier = match config.swipe_gesture_direction() {
        SwipeGestureDirection::Natural => -1.0,
        SwipeGestureDirection::Reversed => 1.0,
    };
    let smooth_enabled = config.smooth_scroll_enabled();
    let wheel_rate = -0.05f64.ln() / config.smooth_scroll_duration().as_secs_f64();

    let mut total_delta = 0.0;
    let mut gesture_delta = 0.0;
    let mut touchpad_down = false;
    let mut has_scroll_event = false;
    let mut has_direct_scroll = false;
    let mut has_gesture_event = false;
    // Wheel target for the deferred insert path (no Scrolling component yet).
    let mut pending_wheel_target: Option<f64> = None;

    // Normalization: Touchpad deltas are typically small fractions.
    // Scroll wheel deltas can be larger. We scale it down slightly
    // to match the "feel" of a finger swipe.
    const SCROLL_SCALE_UPPER: f64 = 0.15;
    const SCROLL_SCALE_LOWER: f64 = 0.005;
    const SCROLL_FULL_RANGE: f64 = 2.0;
    let scroll_scale = SCROLL_SCALE_LOWER
        + ((SCROLL_SCALE_UPPER - SCROLL_SCALE_LOWER) / SCROLL_FULL_RANGE) * swipe_sensitivity;

    let (entity, position, scrolling) = &mut *active_workspace;

    for event in messages.read() {
        match event {
            Event::TouchpadDown => {
                touchpad_down = true;
                total_delta = 0.0;
            }
            Event::Scroll { delta, continuous: true } => {
                total_delta += *delta * scroll_scale;
                has_scroll_event = true;
                has_direct_scroll = true;
            }
            Event::Scroll { delta, continuous: false } if smooth_enabled => {
                has_scroll_event = true;
                let normalized = delta.signum() * delta.abs().max(config.smooth_scroll_step());
                let distance = normalized
                    * config.smooth_scroll_speed()
                    * swipe_sensitivity
                    * direction_modifier;
                if let Some(scrolling) = scrolling.as_mut() {
                    feed_wheel_tick(scrolling, distance, wheel_rate);
                } else {
                    let base = f64::from(position.0.x);
                    pending_wheel_target = Some(match pending_wheel_target {
                        Some(target)
                            if distance == 0.0
                                || (target - base).signum() == distance.signum() =>
                        {
                            target + distance
                        }
                        _ => base + distance,
                    });
                }
            }
            Event::Scroll { delta, continuous: false } => {
                // Smoothing disabled: current immediate path, zero synthetic velocity.
                total_delta += *delta * scroll_scale;
                has_scroll_event = true;
                has_direct_scroll = true;
            }
            Event::Swipe { delta, fingers }
                if config
                    .swipe_gesture_fingers()
                    .is_some_and(|fingers_configured| fingers_configured == *fingers) =>
            {
                total_delta += delta;
                gesture_delta += delta;
                has_scroll_event = true;
                has_gesture_event = true;
            }
            _ => (),
        }
    }

    if !touchpad_down && !has_scroll_event {
        return;
    }

    if touchpad_down && let Some(scrolling) = scrolling.as_mut() {
        scrolling.velocity = 0.0;
        scrolling.is_user_swiping = true;
        scrolling.last_event = Instant::now();
        // Discrete-wheel motion must not survive a touchpad gesture.
        scrolling.wheel_target = None;
        scrolling.wheel_velocity = 0.0;
        scrolling.wheel_idle_seconds = 0.0;
    }

    if has_direct_scroll || has_gesture_event {
        let viewport_width = f64::from(active_display.bounds().width());

        let dt = time.delta_secs_f64();
        let new_velocity = if has_gesture_event && dt > 0.0 {
            gesture_delta * swipe_sensitivity / dt
        } else {
            0.0
        };

        if let Some(scrolling) = scrolling.as_mut() {
            // Continuous events take the direct path; drop any wheel-only state.
            scrolling.wheel_target = None;
            scrolling.wheel_velocity = 0.0;
            scrolling.wheel_idle_seconds = 0.0;
            // Native modifier-scroll events already include macOS momentum.
            // Add synthetic inertia only for raw multi-finger gestures.
            scrolling.velocity = if has_gesture_event {
                // Smoothen gesture velocity changes using EMA.
                0.3 * new_velocity + 0.7 * scrolling.velocity
            } else {
                0.0
            };
            scrolling.is_user_swiping = true;
            scrolling.last_event = Instant::now();
            scrolling.position +=
                total_delta * viewport_width * direction_modifier * swipe_sensitivity;
        } else if let Ok(mut entity_commands) = commands.get_entity(*entity) {
            entity_commands.try_insert(Scrolling {
                velocity: new_velocity,
                position: f64::from(position.0.x)
                    + total_delta * viewport_width * direction_modifier * swipe_sensitivity,
                is_user_swiping: true,
                last_event: Instant::now(),
                wheel_target: pending_wheel_target,
                ..Scrolling::default()
            });
        }
    } else if let Some(target) = pending_wheel_target {
        if let Ok(mut entity_commands) = commands.get_entity(*entity) {
            entity_commands.try_insert(Scrolling {
                velocity: 0.0,
                position: f64::from(position.0.x),
                is_user_swiping: false,
                last_event: Instant::now(),
                wheel_target: Some(target),
                ..Scrolling::default()
            });
        }
    }
}

/// Feeds one discrete wheel tick into the wheel motion state (Mos observable
/// contract): same-direction ticks extend the current target, a reversal
/// discards the old residual tail, and a tick arriving during a fling
/// continues from the projected resting position.
fn feed_wheel_tick(scroll: &mut Scrolling, distance: f64, rate: f64) {
    scroll.velocity = 0.0;
    scroll.is_user_swiping = false;
    scroll.last_event = Instant::now();
    scroll.wheel_idle_seconds = 0.0;
    let base = scroll.position;
    let same_direction = |target: f64| {
        distance == 0.0 || (target - base).signum() == distance.signum()
    };
    match scroll.wheel_target {
        Some(target) if same_direction(target) => scroll.wheel_target = Some(target + distance),
        Some(_) => {
            // Reversal: discard the old residual tail and start fresh.
            scroll.wheel_target = Some(base + distance);
            scroll.wheel_velocity = 0.0;
        }
        None if scroll.wheel_velocity != 0.0 => {
            // Tick during a fling: continue from the projected rest, or reset
            // on reversal.
            let projected = base + scroll.wheel_velocity / rate;
            scroll.wheel_target = Some(if same_direction(projected) {
                projected + distance
            } else {
                base + distance
            });
            scroll.wheel_velocity = 0.0;
        }
        None => scroll.wheel_target = Some(base + distance),
    }
}

/// Advances discrete-wheel motion by one frame. While `wheel_target` is
/// present the position interpolates toward it with rate `rate` (95% of the
/// remaining distance per `duration_ms`); once `wheel_idle_seconds` exceeds
/// the continuation threshold the target is dropped and the retained velocity
/// decays as a closed-form exponential fling. When the projected remaining
/// distance is at or below `dead_zone` the motion lands exactly on the
/// projected endpoint and stops. `rate` and the targets are signed display
/// pixels, so no direction modifier or viewport scaling is applied here.
fn advance_wheel(scroll: &mut Scrolling, dt: f64, rate: f64, dead_zone: f64) {
    const FLING_THRESHOLD_SECONDS: f64 = 0.18;
    if dt <= 0.0 {
        return;
    }
    if let Some(target) = scroll.wheel_target {
        if scroll.wheel_idle_seconds < FLING_THRESHOLD_SECONDS {
            scroll.wheel_idle_seconds += dt;
            let remaining = target - scroll.position;
            scroll.position = target - remaining * (-rate * dt).exp();
            scroll.wheel_velocity = rate * (target - scroll.position);
            if scroll.wheel_idle_seconds >= FLING_THRESHOLD_SECONDS {
                // Tracking-to-fling transition: keep the derivative, drop the
                // target. The fling is the continuous extension of the
                // interpolation, so no transition jump occurs.
                scroll.wheel_target = None;
            }
            return;
        }
        scroll.wheel_target = None;
    }
    if scroll.wheel_velocity == 0.0 {
        return;
    }
    let decay = (-rate * dt).exp();
    let projected_rest = scroll.wheel_velocity / rate;
    if projected_rest.abs() <= dead_zone {
        scroll.position += projected_rest;
        scroll.wheel_velocity = 0.0;
    } else {
        scroll.position += scroll.wheel_velocity * (1.0 - decay) / rate;
        scroll.wheel_velocity *= decay;
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
pub(super) fn swiping_timeout(
    strips: Populated<(Entity, &mut Scrolling), With<LayoutStrip>>,
    active_display: ActiveDisplay,
    time: Res<Time>,
    window_manager: Res<WindowManager>,
    mut commands: Commands,
) {
    const FINGER_LIFT_THRESHOLD: Duration = Duration::from_millis(50);
    const MIN_VELOCITY_PX: f64 = 5.0;
    let dt = time.delta_secs_f64();
    let viewport_width = f64::from(active_display.bounds().width());

    for (entity, mut scroll) in strips {
        if scroll.last_event.elapsed() > FINGER_LIFT_THRESHOLD {
            scroll.is_user_swiping = false;

            if scroll.velocity.abs() * dt * viewport_width < MIN_VELOCITY_PX
                && scroll.wheel_target.is_none()
                && scroll.wheel_velocity == 0.0
                && let Ok(mut entity_commands) = commands.get_entity(entity)
            {
                entity_commands.try_remove::<Scrolling>();
            }
            if let Some(point) = window_manager.cursor_position() {
                commands.trigger(SendMessageTrigger(Event::MouseMoved {
                    point,
                    modifiers: Modifiers::empty(),
                }));
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn apply_inertia(
    mut strips: Populated<(Entity, &mut Scrolling), With<LayoutStrip>>,
    time: Res<Time>,
    config: Res<Config>,
) {
    let dt = time.delta_secs_f64();
    // Discrete-wheel motion advances here, after input handling and before the
    // snap/integrator/constraint systems. This deliberately reuses the chain's
    // existing schedule slot: registering a separate system between
    // swipe_gesture and apply_inertia perturbs Bevy's scheduler order of the
    // layout animation/reshape systems and changes gesture timing.
    let wheel_rate = -0.05f64.ln() / config.smooth_scroll_duration().as_secs_f64();
    let dead_zone = config.smooth_scroll_dead_zone();
    for (_, mut scroll) in &mut strips {
        if scroll.wheel_target.is_some() || scroll.wheel_velocity != 0.0 {
            advance_wheel(&mut scroll, dt, wheel_rate, dead_zone);
        }
        if scroll.is_user_swiping {
            continue;
        }

        if scroll.velocity.abs() > 0.001 {
            let decay_rate = config.swipe_deceleration();
            scroll.velocity *= (-decay_rate * dt).exp();
        } else {
            scroll.velocity = 0.0;
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn apply_snap_force(
    mut strip: Single<(&LayoutStrip, &Position, &mut Scrolling)>,
    active_display: ActiveDisplay,
    windows: Windows,
    config: Res<Config>,
    time: Res<Time>,
) {
    const CENTER_MAGNETIC_FORCE: f64 = 10.0;
    const SNAP_DISPLAY_RATIO: f64 = 0.45;

    if !config.auto_center() {
        return;
    }

    let viewport = active_display.actual_bounds(&config);
    let viewport_center = viewport.center().x;
    let snap_threshold = SNAP_DISPLAY_RATIO * f64::from(viewport.width());

    let (strip, position, ref mut scroll) = *strip;
    if scroll.is_user_swiping || scroll.velocity.abs() > 0.5 {
        return;
    }
    // Auto-center may resume only after wheel motion has settled.
    if scroll.wheel_target.is_some() || scroll.wheel_velocity != 0.0 {
        return;
    }

    let target_offset = strip
        .all_columns()
        .into_iter()
        .filter_map(|entity| {
            windows
                .layout_position(entity)
                .map(|p| p.0.x)
                .zip(Some(entity))
        })
        .map(|(position, entity)| {
            let col_width = windows.moving_frame(entity).map_or(0, |f| f.width());
            viewport_center - (position + col_width / 2)
        })
        .min_by_key(|target| (position.x - target).abs())
        .unwrap_or(position.x);

    let dist_to_snap = f64::from(position.x - target_offset);
    if dist_to_snap.abs() < snap_threshold {
        let dt = time.delta_secs_f64();
        scroll.position -= dist_to_snap * dt * CENTER_MAGNETIC_FORCE;
    }
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn scrolling_integrator(
    mut strip: Single<&mut Scrolling, With<LayoutStrip>>,
    time: Res<Time>,
    active_display: ActiveDisplay,
    config: Res<Config>,
) {
    let dt = time.delta_secs_f64();
    let viewport = active_display.actual_bounds(&config);
    let viewport_width = f64::from(viewport.width());

    // Direction modifier: Natural moves strip left (negative offset) for positive delta (finger left)
    let direction_modifier = match config.swipe_gesture_direction() {
        SwipeGestureDirection::Natural => -1.0,
        SwipeGestureDirection::Reversed => 1.0,
    };

    let scroll = &mut *strip;
    if scroll.velocity.abs() > 0.0001 {
        scroll.position += scroll.velocity * dt * viewport_width * direction_modifier;
    }
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
#[instrument(level = Level::TRACE, skip_all)]
fn apply_scrolling_constraints(
    mut strip: Single<
        (&LayoutStrip, &mut Position, &mut Scrolling),
        (With<ActiveWorkspaceMarker>, Without<Window>),
    >,
    active_display: ActiveDisplay,
    windows: Windows,
    config: Res<Config>,
) {
    let viewport = active_display.actual_bounds(&config);
    let (strip, ref mut position, ref mut scroll) = *strip;

    let get_window_frame = |entity| windows.moving_frame(entity);
    let raw_position = scroll.position as i32;
    let clamped_offset = clamp_viewport_offset(
        raw_position,
        strip,
        &windows,
        &get_window_frame,
        &viewport,
        &config,
    );
    if let Some(clamped_offset) = clamped_offset {
        position.x = clamped_offset;
        // While wheel motion is active, keep the continuous motion state
        // unquantized so interpolation and the fling travel the full target
        // distance; the layout offset remains i32. Re-sync once motion settles.
        if scroll.wheel_target.is_none() && scroll.wheel_velocity == 0.0 {
            scroll.position = f64::from(clamped_offset);
        }
        let clamp_offset = |offset| {
            clamp_viewport_offset(
                offset,
                strip,
                &windows,
                &get_window_frame,
                &viewport,
                &config,
            )
        };
        apply_wheel_constraints(scroll, raw_position, Some(clamped_offset), clamp_offset);
    } else {
        scroll.velocity = 0.0;
        scroll.wheel_target = None;
        scroll.wheel_velocity = 0.0;
    }
}

/// Clamps wheel motion state against the strip boundaries: the wheel target
/// is clamped through the same offset clamps as the position, outward wheel
/// velocity is zeroed when the requested frame crosses a boundary, and a
/// missing valid extent clears all wheel state (no unreachable target is
/// retained).
fn apply_wheel_constraints(
    scroll: &mut Scrolling,
    raw_position: i32,
    clamped_position: Option<i32>,
    clamp_offset: impl Fn(i32) -> Option<i32>,
) {
    match clamped_position {
        Some(clamped) => {
            let outward = f64::from(raw_position - clamped).signum();
            if outward != 0.0 && outward == scroll.wheel_velocity.signum() {
                scroll.wheel_velocity = 0.0;
            }
            if let Some(target) = scroll.wheel_target {
                match clamp_offset(target as i32) {
                    Some(clamped_target) => {
                        scroll.wheel_target = Some(f64::from(clamped_target));
                    }
                    None => {
                        scroll.wheel_target = None;
                        scroll.wheel_velocity = 0.0;
                    }
                }
            }
        }
        None => {
            scroll.wheel_target = None;
            scroll.wheel_velocity = 0.0;
        }
    }
}

#[instrument(level = Level::TRACE, skip_all)]
fn clamp_viewport_offset<W>(
    current_offset: i32,
    layout_strip: &LayoutStrip,
    windows: &Windows,
    get_window_frame: &W,
    viewport: &IRect,
    config: &Config,
) -> Option<i32>
where
    W: Fn(Entity) -> Option<IRect>,
{
    let total_strip_width = layout_strip
        .last()
        .ok()
        .and_then(|column| column.top())
        .and_then(|entity| {
            windows
                .layout_position(entity)
                .zip(get_window_frame(entity))
        })
        .map(|(position, frame)| position.x + frame.width())?;

    let continuous_swipe = config.continuous_swipe();
    let strip_position = |column: Result<Column>| {
        column
            .ok()
            .and_then(|column| column.top())
            .and_then(|entity| windows.layout_position(entity))
            .map(|position| position.0.x)
    };

    let left_snap = strip_position(layout_strip.last());
    let right_snap = strip_position(layout_strip.get(1));

    Some(
        if continuous_swipe && let Some((left_snap, right_snap)) = left_snap.zip(right_snap) {
            // Allow to scroll away until the last or first window snaps.
            current_offset.clamp(viewport.min.x - left_snap, viewport.max.x - right_snap)
        } else if viewport.width() < total_strip_width {
            // Snap the strip directly to the edges.
            current_offset.clamp(viewport.max.x - total_strip_width, viewport.min.x)
        } else {
            // Snap the strip directly to the edges.
            current_offset.clamp(viewport.min.x, viewport.max.x - total_strip_width)
        },
    )
}

#[derive(Default)]
struct VerticalGestureState {
    accumulated: f64,
    last_event: Option<Instant>,
    fired: bool,
}

#[allow(clippy::needless_pass_by_value)]
#[instrument(level = Level::TRACE, skip_all)]
fn vertical_swipe_gesture(
    mut messages: MessageReader<Event>,
    active_display: ActiveDisplay,
    config: Res<Config>,
    mut commands: Commands,
    mut state: Local<VerticalGestureState>,
) {
    const GESTURE_TIMEOUT: Duration = Duration::from_millis(150);

    if active_display.fullscreen().is_some() {
        return;
    }

    // Reset state when the gesture times out (fingers lifted).
    if let Some(last) = state.last_event
        && last.elapsed() > GESTURE_TIMEOUT
    {
        state.accumulated = 0.0;
        state.fired = false;
    }

    for event in messages.read() {
        match event {
            Event::VerticalScrollTick { delta } => {
                switch_virtual_workspace(*delta, &config, &mut commands);
            }
            Event::VerticalSwipe { delta, fingers }
                if config
                    .swipe_gesture_fingers()
                    .is_some_and(|fingers_configured| fingers_configured == *fingers) =>
            {
                state.last_event = Some(Instant::now());

                if !state.fired {
                    state.accumulated += delta;
                }
            }
            _ => {}
        }
    }

    // Threshold needs to be high enough that incidental vertical movement
    // during horizontal swipes doesn't trigger a workspace switch.
    let threshold = 0.15 / config.swipe_sensitivity();
    if state.accumulated.abs() >= threshold {
        switch_virtual_workspace(state.accumulated, &config, &mut commands);
        state.accumulated = 0.0;
        state.fired = true;
    }
}

fn switch_virtual_workspace(delta: f64, config: &Config, commands: &mut Commands) {
    let physical_finger_direction = if delta > 0.0 {
        Direction::South
    } else {
        Direction::North
    };
    let direction = match config.swipe_gesture_direction() {
        SwipeGestureDirection::Natural => physical_finger_direction.reverse(),
        SwipeGestureDirection::Reversed => physical_finger_direction,
    };
    commands.trigger(SendMessageTrigger(Event::Command {
        command: Command::Window(Operation::Virtual(direction)),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 5.348_715_445_543_852; // -ln(0.05) / 0.56 (default duration_ms = 560)
    const DEAD_ZONE: f64 = 1.0;

    fn wheel_scroll(position: f64, target: Option<f64>, velocity: f64, idle: f64) -> Scrolling {
        Scrolling {
            velocity: 0.0,
            position,
            is_user_swiping: false,
            last_event: Instant::now(),
            wheel_target: target,
            wheel_velocity: velocity,
            wheel_idle_seconds: idle,
        }
    }

    #[test]
    fn wheel_tick_moves_partway_on_first_frame() {
        let mut scroll = wheel_scroll(0.0, Some(-100.0), 0.0, 0.0);
        advance_wheel(&mut scroll, 1.0 / 60.0, RATE, DEAD_ZONE);
        assert!(scroll.position < 0.0);
        assert!(scroll.position > -100.0);
        assert_ne!(scroll.position, -100.0);
        assert_ne!(scroll.wheel_velocity, 0.0);
    }

    #[test]
    fn wheel_same_direction_ticks_extend_target_and_travel() {
        let mut burst = wheel_scroll(0.0, None, 0.0, 0.0);
        feed_wheel_tick(&mut burst, -33.6, RATE);
        feed_wheel_tick(&mut burst, -33.6, RATE);
        feed_wheel_tick(&mut burst, -33.6, RATE);
        assert!(
            (burst.wheel_target.expect("target") + 100.8).abs() < 1e-9,
            "three same-direction ticks must accumulate to -100.8"
        );

        let mut single = wheel_scroll(0.0, None, 0.0, 0.0);
        feed_wheel_tick(&mut single, -33.6, RATE);
        assert_eq!(single.wheel_target, Some(-33.6));

        for _ in 0..5 {
            advance_wheel(&mut burst, 1.0 / 60.0, RATE, DEAD_ZONE);
            advance_wheel(&mut single, 1.0 / 60.0, RATE, DEAD_ZONE);
        }
        assert!(
            burst.position < single.position,
            "burst must travel farther than a single tick"
        );
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn wheel_reversed_tick_replaces_residual() {
        let mut scroll = wheel_scroll(0.0, None, 0.0, 0.0);
        feed_wheel_tick(&mut scroll, -33.6, RATE);
        // Two 50ms frames keep tracking below the 0.18s fling threshold.
        advance_wheel(&mut scroll, 0.05, RATE, DEAD_ZONE);
        advance_wheel(&mut scroll, 0.05, RATE, DEAD_ZONE);
        assert!(scroll.position < 0.0);

        feed_wheel_tick(&mut scroll, 33.6, RATE);
        assert_eq!(scroll.wheel_target, Some(scroll.position + 33.6));
        assert_eq!(scroll.wheel_velocity, 0.0);

        let before = scroll.position;
        advance_wheel(&mut scroll, 0.05, RATE, DEAD_ZONE);
        assert!(
            scroll.position > before,
            "next frame must move only in the new direction"
        );
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn wheel_reversed_tick_during_fling_resets() {
        let mut scroll = wheel_scroll(0.0, None, 0.0, 0.0);
        feed_wheel_tick(&mut scroll, -33.6, RATE);
        // Three 100ms frames: idle 0.3s > 0.18s, so tracking already became a fling.
        for _ in 0..3 {
            advance_wheel(&mut scroll, 0.1, RATE, DEAD_ZONE);
        }
        assert!(scroll.wheel_target.is_none());
        assert_ne!(scroll.wheel_velocity, 0.0);

        feed_wheel_tick(&mut scroll, 33.6, RATE);
        assert_eq!(scroll.wheel_target, Some(scroll.position + 33.6));
        assert_eq!(scroll.wheel_velocity, 0.0);

        let before = scroll.position;
        advance_wheel(&mut scroll, 0.1, RATE, DEAD_ZONE);
        assert!(scroll.position > before);
    }

    #[test]
    fn wheel_tracking_to_fling_is_continuous() {
        let mut scroll = wheel_scroll(0.0, None, 0.0, 0.0);
        feed_wheel_tick(&mut scroll, -100.0, RATE);
        advance_wheel(&mut scroll, 0.1, RATE, DEAD_ZONE);
        advance_wheel(&mut scroll, 0.1, RATE, DEAD_ZONE);
        // Crossing frame: target cleared, velocity retained.
        assert!(scroll.wheel_target.is_none());
        let position = scroll.position;
        let velocity = scroll.wheel_velocity;
        assert!(
            (position + velocity / RATE + 100.0).abs() < 1e-6,
            "projected endpoint must equal the target"
        );

        for _ in 0..200 {
            if scroll.wheel_velocity == 0.0 {
                break;
            }
            advance_wheel(&mut scroll, 0.1, RATE, DEAD_ZONE);
        }
        assert_eq!(scroll.wheel_velocity, 0.0);
        assert!(
            (scroll.position + 100.0).abs() < 1e-6,
            "fling must land on the original target"
        );
    }

    #[test]
    fn wheel_frame_rate_independent() {
        let mut at_60 = wheel_scroll(0.0, None, 0.0, 0.0);
        feed_wheel_tick(&mut at_60, -100.0, RATE);
        for _ in 0..60 {
            advance_wheel(&mut at_60, 1.0 / 60.0, RATE, DEAD_ZONE);
        }

        let mut at_120 = wheel_scroll(0.0, None, 0.0, 0.0);
        feed_wheel_tick(&mut at_120, -100.0, RATE);
        for _ in 0..120 {
            advance_wheel(&mut at_120, 1.0 / 120.0, RATE, DEAD_ZONE);
        }

        assert!(
            (at_60.position - at_120.position).abs() < 1e-4,
            "positions drift across refresh rates: {} vs {}",
            at_60.position,
            at_120.position
        );
        assert!(
            (at_60.wheel_velocity - at_120.wheel_velocity).abs() < 1e-3,
            "velocities drift across refresh rates"
        );
    }

    #[test]
    fn wheel_zero_dt_is_finite_and_stationary() {
        let mut scroll = wheel_scroll(-10.0, Some(-100.0), -20.0, 0.1);
        advance_wheel(&mut scroll, 0.0, RATE, DEAD_ZONE);
        assert_eq!(scroll.position, -10.0);
        assert_eq!(scroll.wheel_target, Some(-100.0));
        assert_eq!(scroll.wheel_velocity, -20.0);
        assert!(scroll.position.is_finite());
        assert!(scroll.wheel_velocity.is_finite());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn wheel_fling_lands_exactly_at_dead_zone() {
        // Projected rest 1.01 px > dead_zone 1.0: one decaying frame, then the
        // motion lands exactly on the projected endpoint.
        let mut scroll = wheel_scroll(0.0, None, RATE * 1.01, 0.0);
        advance_wheel(&mut scroll, 0.1, RATE, DEAD_ZONE);
        let decay = (-RATE * 0.1).exp();
        assert_ne!(scroll.wheel_velocity, 0.0);
        let expected = 1.01 * (1.0 - decay);
        assert!((scroll.position - expected).abs() < 1e-12);

        let projected = scroll.wheel_velocity / RATE;
        let before = scroll.position;
        advance_wheel(&mut scroll, 0.1, RATE, DEAD_ZONE);
        assert_eq!(scroll.wheel_velocity, 0.0);
        assert!(
            (scroll.position - (before + projected)).abs() < 1e-12,
            "landing must advance exactly to the projected endpoint"
        );
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn wheel_clamped_edge_clears_outward_motion() {
        // Raw position -10 clamped to 0: left boundary, outward (negative) motion killed.
        let mut outward = wheel_scroll(-10.0, Some(-50.0), -5.0, 0.0);
        apply_wheel_constraints(&mut outward, -10, Some(0), |_| Some(-20));
        assert_eq!(outward.wheel_velocity, 0.0);
        assert_eq!(outward.wheel_target, Some(-20.0));

        // Inward (positive) velocity at the same boundary is preserved.
        let mut inward = wheel_scroll(-10.0, Some(-50.0), 5.0, 0.0);
        apply_wheel_constraints(&mut inward, -10, Some(0), |_| Some(-20));
        assert_eq!(inward.wheel_velocity, 5.0);
        assert_eq!(inward.wheel_target, Some(-20.0));

        // No valid strip extent: all wheel state is cleared, no unreachable target.
        let mut invalid = wheel_scroll(-10.0, Some(-50.0), -5.0, 0.0);
        apply_wheel_constraints(&mut invalid, -10, None, |_| None);
        assert_eq!(invalid.wheel_velocity, 0.0);
        assert_eq!(invalid.wheel_target, None);
    }
}
