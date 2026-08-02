//! Canvas-mode integration tests: routing, seeding, frame transforms,
//! command parsing/errors, migration in both directions, exclusion
//! precedence, and unchanged strip routing elsewhere.

use bevy::math::{IRect, IVec2};
use bevy::prelude::*;

use crate::canvas_engine::camera::canvas_to_screen;
use crate::canvas_engine::geom::Pt;
use crate::commands::{CanvasOperation, Command, Operation};
use crate::config::Config;
use crate::ecs::canvas::CanvasWorlds;
use crate::ecs::layout::LayoutStrip;
use crate::ecs::{CanvasManaged, Unmanaged};
use crate::events::Event;
use crate::manager::{Display, Window};
use crate::platform::WinID;

use super::harness::{TestHarness, find_window_entity};
use super::*;

fn canvas_config(displays: &[u32]) -> Config {
    let uuids = displays
        .iter()
        .map(|id| Display::mock_uuid(*id))
        .collect::<Vec<_>>()
        .join("\", \"");
    let input = format!(
        "[options]\n\n[canvas]\ndisplays = [\"{uuids}\"]\n\n[bindings]\n"
    );
    Config::try_from(input.as_str()).expect("config should parse")
}

fn default_config() -> Config {
    Config::try_from("[options]\n\n[bindings]\n").expect("config should parse")
}

fn canvas_command(op: CanvasOperation) -> Event {
    Event::Command {
        command: Command::Window(Operation::Canvas(op)),
    }
}

/// A benign command that drives the app loop without side effects.
fn tick() -> Event {
    Event::Command {
        command: Command::PrintState,
    }
}

fn canvas_world<'a>(world: &'a World, id: u32) -> Option<&'a crate::ecs::canvas::CanvasWorld> {
    let uuid = Display::mock_uuid(id);
    world.resource::<CanvasWorlds>().world_for_display(&uuid)
}

fn window_frame(world: &mut World, win_id: WinID) -> IRect {
    let entity = find_window_entity(win_id, world);
    world
        .entity(entity)
        .get::<Window>()
        .expect("window component")
        .frame()
}

fn in_strip(world: &mut World, win_id: WinID) -> bool {
    let entity = find_window_entity(win_id, world);
    let mut strips = world.query::<&LayoutStrip>();
    strips.iter(world).any(|strip| strip.contains(entity))
}

#[test]
fn test_canvas_routing_seeds_surface() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_windows(2);
    harness.run(vec![tick()]);

    let world = harness.world();
    let display_bounds = world
        .query::<&Display>()
        .iter(world)
        .next()
        .expect("test display")
        .bounds();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    assert_eq!(world_state.surfaces.len(), 2);
    // World rects are display-local: world + bounds.min == the seeded frame.
    let surface = &world_state.surfaces[&0];
    assert_eq!(surface.world.x, (0 - display_bounds.min.x) as f64);
    assert_eq!(surface.world.y, (0 - display_bounds.min.y) as f64);
    assert_eq!(surface.world.w, TEST_WINDOW_WIDTH as f64);
    assert_eq!(surface.world.h, TEST_WINDOW_HEIGHT as f64);

    let entity = find_window_entity(0, world);
    assert!(world.entity(entity).contains::<CanvasManaged>());
    assert!(matches!(
        world.entity(entity).get::<Unmanaged>(),
        Some(Unmanaged::Floating)
    ));
}

#[test]
fn test_canvas_pan_moves_frames() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_windows(1);
    harness.run(vec![tick(), canvas_command(CanvasOperation::Pan(100.0, -50.0, None))]);

    let world = harness.world();
    let frame = window_frame(world, 0);
    assert_eq!(frame.min.x, 100);
    assert_eq!(frame.min.y, -50);
    assert_eq!(frame.width(), TEST_WINDOW_WIDTH);
    assert_eq!(frame.height(), TEST_WINDOW_HEIGHT);

    // Second pan is relative to the current camera.
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    assert_eq!(world_state.camera.x, 100.0);
    assert_eq!(world_state.camera.y, -50.0);
}

#[test]
fn test_canvas_zoom_anchored_scales_frame() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_windows(1);
    // Zoom 0.5 anchored at the window center (display-local (200, 480)):
    // the world point under the anchor stays put, size halves. MAX_ZOOM is
    // 1.0 (zoom-in is clamped), so zoom out first, then back in.
    harness.run(vec![tick(), canvas_command(CanvasOperation::Zoom(0.5, 200.0, 480.0, None))]);

    let world = harness.world();
    let frame = window_frame(world, 0);
    assert_eq!(frame.width(), TEST_WINDOW_WIDTH / 2);
    assert_eq!(frame.height(), TEST_WINDOW_HEIGHT / 2);
    // The window center (global (200, 500)) is invariant under the zoom.
    assert_eq!(frame.min.x + frame.width() / 2, 200);
    assert_eq!(frame.min.y + frame.height() / 2, 500);
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    assert_eq!(world_state.zoom, 0.5);

    // Zoom back in 2x at the same anchor: original size, same center.
    harness
        .world()
        .write_message(canvas_command(CanvasOperation::Zoom(2.0, 200.0, 480.0, None)));
    for _ in 0..5 {
        harness.app.update();
    }
    let world = harness.world();
    let frame = window_frame(world, 0);
    assert_eq!(frame.width(), TEST_WINDOW_WIDTH);
    assert_eq!(frame.height(), TEST_WINDOW_HEIGHT);
    assert_eq!(frame.min.x + frame.width() / 2, 200);
    assert_eq!(frame.min.y + frame.height() / 2, 500);
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    assert_eq!(world_state.zoom, 1.0);
}

#[test]
fn test_canvas_home_and_fit_all() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_window(0, |w| {
            w.frame = IRect::new(0, 0, 400, 400);
        })
        .with_window(1, |w| {
            w.frame = IRect::from_corners(IVec2::new(600, 300), IVec2::new(1000, 700));
        });
    harness.run(vec![tick(), canvas_command(CanvasOperation::Home(None))]);

    let world = harness.world();
    let display_bounds = world
        .query::<&Display>()
        .iter(world)
        .next()
        .expect("test display")
        .bounds();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    // Home: zoom 1.0, world origin at the viewport center. The window's
    // world rect maps through the camera and lands at the same global spot.
    let world_rect = world_state.surfaces[&0].world;
    let camera = world_state.camera;
    let zoom = world_state.zoom;
    assert_eq!(zoom, 1.0);
    let frame = window_frame(world, 0);
    let expected = canvas_to_screen(Pt::new(world_rect.x, world_rect.y), camera, zoom);
    assert_eq!(
        frame.min,
        IVec2::new(
            display_bounds.min.x + expected.x.round() as i32,
            display_bounds.min.y + expected.y.round() as i32,
        )
    );

    // Fit-all: bbox (0,0,1000,700) in the display viewport with 24px margin.
    harness
        .world()
        .write_message(canvas_command(CanvasOperation::FitAll(None)));
    for _ in 0..5 {
        harness.app.update();
    }
    let world = harness.world();
    let display_bounds = world
        .query::<&Display>()
        .iter(world)
        .next()
        .expect("test display")
        .bounds();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    let viewport_w = display_bounds.width() as f64;
    let viewport_h = display_bounds.height() as f64;
    let expected_zoom = ((viewport_w - 48.0) / 1000.0).min((viewport_h - 48.0) / 700.0);
    assert_eq!(world_state.zoom, expected_zoom);
    // Window 1's world rect maps deterministically through the new camera.
    let world_rect = world_state.surfaces[&1].world;
    let zoom = world_state.zoom;
    let camera = world_state.camera;
    let frame = window_frame(world, 1);
    let expected = canvas_to_screen(Pt::new(world_rect.x, world_rect.y), camera, zoom);
    assert_eq!(
        frame.min,
        IVec2::new(
            display_bounds.min.x + expected.x.round() as i32,
            display_bounds.min.y + expected.y.round() as i32,
        )
    );
}

#[test]
fn test_canvas_snap_tightens_near_edges() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_window(0, |w| {
            w.frame = IRect::new(0, 0, 400, 400);
        })
        .with_window(1, |w| {
            w.frame = IRect::from_corners(IVec2::new(405, 0), IVec2::new(805, 400)); // 5px gap: within engage
        })
        .with_window(2, |w| {
            w.frame = IRect::from_corners(IVec2::new(805, 405), IVec2::new(1205, 805)); // corner contact only
        });
    harness.run(vec![tick(), canvas_command(CanvasOperation::Snap(None))]);

    let world = harness.world();
    // Sequential resolution: window 0's right edge (nearest candidate is
    // window 1's left edge, 5px away) shifts to exact adjacency; window 1
    // then sees window 0's right edge at its own left edge (shift 0) and
    // stays put.
    let frame = window_frame(world, 0);
    assert_eq!(frame.min.x, 5);
    assert_eq!(frame.min.y, 0);
    let frame = window_frame(world, 1);
    assert_eq!(frame.min.x, 405);
    assert_eq!(frame.min.y, 0);
    // Corner-only contact is rejected: window 2 does not move.
    let frame = window_frame(world, 2);
    assert_eq!(frame.min.x, 805);
    assert_eq!(frame.min.y, 405);
}

#[test]
fn test_canvas_exclusion_wins_over_canvas() {
    let uuid = Display::mock_uuid(TEST_DISPLAY_ID);
    let input = format!(
        "[options]\nexcluded_displays = [\"{uuid}\"]\n\n[canvas]\ndisplays = [\"{uuid}\"]\n\n[bindings]\n"
    );
    let config = Config::try_from(input.as_str()).expect("config should parse");
    let mut harness = TestHarness::new().with_config(config).with_windows(1);
    harness.run(vec![tick()]);

    let world = harness.world();
    let entity = find_window_entity(0, world);
    assert!(!world.entity(entity).contains::<CanvasManaged>());
    assert!(matches!(
        world.entity(entity).get::<Unmanaged>(),
        Some(Unmanaged::ExcludedDisplay)
    ));
    assert!(canvas_world(world, TEST_DISPLAY_ID).is_none());
}

#[test]
fn test_canvas_strip_routing_unchanged_elsewhere() {
    // Canvas configured for a display that does not exist: the test display
    // keeps normal strip routing.
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID + 100]))
        .with_windows(1);
    harness.run(vec![tick()]);

    let world = harness.world();
    assert!(canvas_world(world, TEST_DISPLAY_ID).is_none());
    let entity = find_window_entity(0, world);
    assert!(!world.entity(entity).contains::<CanvasManaged>());
    assert!(world.entity(entity).get::<Unmanaged>().is_none());
    assert!(in_strip(world, 0));
}

#[test]
fn test_canvas_migration_strip_to_canvas() {
    // Window starts in a normal strip; config reload flips the display to
    // Canvas; the window converts and gets seeded.
    let mut harness = TestHarness::new().with_windows(1);
    harness.run(vec![tick()]);
    {
        let world = harness.world();
        let entity = find_window_entity(0, world);
        assert!(!world.entity(entity).contains::<CanvasManaged>());
        assert!(in_strip(world, 0));
    }

    harness
        .world()
        .insert_resource(canvas_config(&[TEST_DISPLAY_ID]));
    harness.run(vec![tick()]);

    let world = harness.world();
    let entity = find_window_entity(0, world);
    assert!(world.entity(entity).contains::<CanvasManaged>());
    assert!(matches!(
        world.entity(entity).get::<Unmanaged>(),
        Some(Unmanaged::Floating)
    ));
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    assert_eq!(world_state.surfaces.len(), 1);
}

#[test]
fn test_canvas_migration_canvas_to_strip() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_windows(1);
    harness.run(vec![tick()]);
    {
        let world = harness.world();
        let entity = find_window_entity(0, world);
        assert!(world.entity(entity).contains::<CanvasManaged>());
    }

    harness.world().insert_resource(default_config());
    harness.run(vec![tick(), tick()]);

    let world = harness.world();
    assert!(canvas_world(world, TEST_DISPLAY_ID).is_none());
    let entity = find_window_entity(0, world);
    assert!(!world.entity(entity).contains::<CanvasManaged>());
    assert!(world.entity(entity).get::<Unmanaged>().is_none());
    assert!(in_strip(world, 0));
}

#[test]
fn test_canvas_command_requires_unambiguous_display() {
    // Two canvas displays: an implicit command must not apply to either.
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID, TEST_DISPLAY_ID + 1]))
        .with_display(
            TEST_DISPLAY_ID + 1,
            IRect::from_corners(IVec2::new(1024, 0), IVec2::new(2048, 768)),
            vec![TEST_WORKSPACE_ID + 1],
        )
        .with_windows(1);
    harness.run(vec![tick(), canvas_command(CanvasOperation::Pan(50.0, 0.0, None))]);

    let world = harness.world();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    assert_eq!(world_state.camera.x, 0.0);

    // An explicit UUID applies to that display's world.
    harness
        .world()
        .write_message(canvas_command(CanvasOperation::Pan(
            50.0,
            0.0,
            Some(Display::mock_uuid(TEST_DISPLAY_ID)),
        )));
    for _ in 0..5 {
        harness.app.update();
    }
    let world = harness.world();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    assert_eq!(world_state.camera.x, 50.0);
}

#[test]
fn test_canvas_parse_command_forms() {
    use crate::config::parse_command;

    let uuid = Display::mock_uuid(TEST_DISPLAY_ID);
    assert!(parse_command(&["canvas"]).is_err());
    assert!(parse_command(&["canvas", "pan", "10"]).is_err());
    assert!(parse_command(&["canvas", "pan", "10", "not-a-number"]).is_err());
    assert!(parse_command(&["canvas", "bogus"]).is_err());

    match parse_command(&["canvas", "pan", "10", "20"]).expect("pan parses") {
        Command::Window(Operation::Canvas(CanvasOperation::Pan(10.0, 20.0, None))) => {}
        other => panic!("unexpected {other:?}"),
    }
    match parse_command(&["canvas", "pan", "10", "20", &uuid])
        .expect("pan with uuid parses")
    {
        Command::Window(Operation::Canvas(CanvasOperation::Pan(10.0, 20.0, Some(u))) ) => {
            assert_eq!(u, uuid);
        }
        other => panic!("unexpected {other:?}"),
    }
    match parse_command(&["canvas", "zoom", "2", "100", "200"]).expect("zoom parses") {
        Command::Window(Operation::Canvas(CanvasOperation::Zoom(2.0, 100.0, 200.0, None))) => {}
        other => panic!("unexpected {other:?}"),
    }
    match parse_command(&["canvas", "home", &uuid]).expect("home parses") {
        Command::Window(Operation::Canvas(CanvasOperation::Home(Some(u)))) => assert_eq!(u, uuid),
        other => panic!("unexpected {other:?}"),
    }
    match parse_command(&["canvas", "fit-all"]).expect("fit-all parses") {
        Command::Window(Operation::Canvas(CanvasOperation::FitAll(None))) => {}
        other => panic!("unexpected {other:?}"),
    }
    match parse_command(&["canvas", "snap"]).expect("snap parses") {
        Command::Window(Operation::Canvas(CanvasOperation::Snap(None))) => {}
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn test_canvas_scroll_gesture_pans_camera() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_windows(1);
    harness.run(vec![
        tick(),
        Event::Scroll {
            delta: 100.0,
            continuous: true,
        },
    ]);
    let world = harness.world();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    // Trackpad scale 0.15: 100 * 0.15 = 15 px of camera pan (no fling yet).
    assert_eq!(world_state.camera.x, 15.0);
    assert_eq!(world_state.camera.y, 0.0);
    assert!(!world_state.momentum.is_moving());
}

#[test]
fn test_canvas_scroll_gesture_fling_on_release() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_windows(1);
    harness.run(vec![
        tick(),
        Event::Scroll {
            delta: 100.0,
            continuous: true,
        },
        Event::TouchpadUp,
        Event::TouchpadUp,
    ]);
    let world = harness.world();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    // The fling carries the camera beyond the 15px direct pan...
    assert!(world_state.camera.x > 15.0);
    // ...and eventually settles (momentum decays to the stop threshold).
    for _ in 0..200 {
        harness.app.update();
    }
    let world = harness.world();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    assert!(!world_state.momentum.is_moving());
}

#[test]
fn test_canvas_scroll_gesture_ignores_non_canvas_displays() {
    // No canvas config: the gesture system must not create worlds or panic;
    // the strip scroll path is exercised by the existing scroll tests.
    let mut harness = TestHarness::new().with_windows(1);
    harness.run(vec![
        tick(),
        Event::Scroll {
            delta: 100.0,
            continuous: true,
        },
        Event::TouchpadUp,
    ]);
    let world = harness.world();
    assert!(canvas_world(world, TEST_DISPLAY_ID).is_none());
}

#[test]
fn test_canvas_scroll_gesture_wheel_tick() {
    let mut harness = TestHarness::new()
        .with_config(canvas_config(&[TEST_DISPLAY_ID]))
        .with_windows(1);
    harness.run(vec![
        tick(),
        Event::Scroll {
            delta: 1.0,
            continuous: false,
        },
    ]);
    let world = harness.world();
    let world_state = canvas_world(world, TEST_DISPLAY_ID).expect("canvas world exists");
    // Physical wheel: one notch pans a fixed 60px.
    assert_eq!(world_state.camera.x, 60.0);
}
