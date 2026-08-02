//! Canvas mode: opt-in infinite-canvas management for configured displays.
//!
//! Windows on `[canvas].displays` displays are routed through the managed
//! floating layer (`Unmanaged::Floating`) and carry the [`CanvasManaged`]
//! marker. [`CanvasWorlds`] holds one world per display UUID — a camera/zoom
//! state plus a `WinID -> CanvasSurface` map, keyed by the canonical Core
//! Graphics UUID string of the display. The math lives in
//! `crate::canvas_engine`; this module is the ECS adapter: surface seeding,
//! the deterministic frame-apply pass, momentum ticks, cleanup on marker
//! removal, and config-reload migration in both directions.
//!
//! Frame authority: the apply pass writes native frames directly through the
//! existing `WindowApi` (`reposition` then `resize`, position before size, the
//! same order Paneru's own staging uses). Canvas windows never receive
//! `Position`/`Bounds` component writes, so the strip layout systems and the
//! `commit_window_position`/`commit_window_size` systems cannot race them.

use std::collections::{BTreeMap, HashMap};

use bevy::ecs::change_detection::DetectChanges;
use bevy::ecs::entity::Entity;
use bevy::ecs::lifecycle::RemovedComponents;
use bevy::ecs::query::{Added, With};
use bevy::ecs::resource::Resource;
use bevy::ecs::system::{Commands, Query, Res, ResMut};
use bevy::math::{IRect, IVec2};
use bevy::time::Time;
use tracing::{debug, info, warn};

use crate::canvas_engine::camera::{
    all_windows_bbox, canvas_to_screen, clamp_zoom, home_camera, zoom_anchor_camera, zoom_to_fit,
};
use crate::canvas_engine::cluster::Side;
use crate::canvas_engine::geom::{Pt, Rect};
use crate::canvas_engine::momentum::MomentumState;
use crate::canvas_engine::snap::{SnapRect, snap_candidates};
use crate::config::Config;
use crate::ecs::{CanvasManaged, Unmanaged};
use crate::manager::Display;
use crate::platform::WinID;

/// Margin (screen px) inset applied to the viewport by fit-all.
const FIT_MARGIN: f64 = 24.0;

/// Engage tolerance (world px) for the one-shot `canvas snap` command.
const SNAP_ENGAGE: f64 = 6.0;

/// One infinite world per Canvas display (key: canonical display UUID).
#[derive(Resource, Default)]
pub struct CanvasWorlds {
    pub worlds: HashMap<String, CanvasWorld>,
}

impl CanvasWorlds {
    pub fn world_for_display_mut(&mut self, uuid: &str) -> Option<&mut CanvasWorld> {
        if self.worlds.contains_key(uuid) {
            return self.worlds.get_mut(uuid);
        }
        self.worlds
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(uuid))
            .map(|(_, world)| world)
    }

    #[allow(dead_code)] // mirrors world_for_display_mut for read-only consumers
    pub fn world_for_display(&self, uuid: &str) -> Option<&CanvasWorld> {
        if self.worlds.contains_key(uuid) {
            return self.worlds.get(uuid);
        }
        self.worlds
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(uuid))
            .map(|(_, world)| world)
    }
}

/// Camera + surface state for one Canvas display.
pub struct CanvasWorld {
    /// Screen position (display-local px) of the world origin.
    pub camera: Pt,
    /// World-to-screen scale; clamped to `[MIN_ZOOM_FLOOR, MAX_ZOOM]`.
    pub zoom: f64,
    /// Frame-rate-independent fling state (gesture-driven pan, command-fed).
    pub momentum: MomentumState,
    /// Stable ordering by `WinID` for deterministic apply/state output.
    pub surfaces: BTreeMap<WinID, CanvasSurface>,
}

impl Default for CanvasWorld {
    fn default() -> Self {
        Self {
            camera: Pt::ZERO,
            zoom: 1.0,
            momentum: MomentumState::default(),
            surfaces: BTreeMap::new(),
        }
    }
}

/// One Canvas-managed window: its world rect plus native-frame bookkeeping.
pub struct CanvasSurface {
    /// World-space rect (display-local coords at zoom 1 / camera origin).
    pub world: Rect,
    /// Last frame this daemon asked the OS for (None before first apply).
    pub last_requested: Option<IRect>,
    /// Last frame observed after an apply (from `update_frame`).
    pub last_observed: IRect,
    /// True when the last observed native size differs from the requested one
    /// (fixed-size/constrained app). The world rect keeps the logical size.
    pub constrained: bool,
    /// Suspended (minimized/hidden): the apply pass skips the window but the
    /// world rect is retained so it resumes at the same world position.
    pub suspended: bool,
}

/// Creates Canvas worlds for configured displays at startup (exclusion wins),
/// so commands and routing find the world before any window is processed.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn canvas_init_worlds(
    config: Res<Config>,
    displays: Query<&Display>,
    mut worlds: ResMut<CanvasWorlds>,
) {
    for display in &displays {
        let uuid = display.uuid().to_string();
        if config.is_canvas_display(&uuid) && !config.excludes_display_uuid(&uuid) {
            info!("canvas: display {uuid} is Canvas-managed");
            worlds.worlds.entry(uuid).or_default();
        }
    }
}

/// Seeds a surface for every window that just gained the `CanvasManaged`
/// marker (spawn routing, workspace migration, or session restore). The world
/// rect is the actual AX frame translated into display-local coordinates, so
/// the initial state is identity: windows stay exactly where they are.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn canvas_seed_surfaces(
    added: Query<(Entity, &crate::manager::Window), Added<CanvasManaged>>,
    displays: Query<&Display>,
    config: Res<Config>,
    mut worlds: ResMut<CanvasWorlds>,
) {
    for (_, window) in &added {
        let frame = window.frame();
        let Some(display) = displays
            .iter()
            .find(|display| display.contains_frame(frame))
        else {
            debug!("canvas: window {} seeded before display classification", window.id());
            continue;
        };
        let uuid = display.uuid().to_string();
        if !config.is_canvas_display(&uuid) || config.excludes_display_uuid(&uuid) {
            // Stale marker (config changed mid-flight); migration cleans it up.
            debug!("canvas: window {} marked canvas on non-canvas display", window.id());
            continue;
        }
        let bounds = display.bounds();
        let local = IRect::from_corners(frame.min - bounds.min, frame.max - bounds.min);
        let world = worlds.worlds.entry(uuid.clone()).or_default();
        let inserted = world.surfaces.insert(
            window.id(),
            CanvasSurface {
                world: Rect::new(
                    local.min.x as f64,
                    local.min.y as f64,
                    local.width() as f64,
                    local.height() as f64,
                ),
                last_requested: None,
                last_observed: frame,
                constrained: false,
                suspended: false,
            },
        );
        debug!(
            "canvas: seeded window {} on display {} (replaced: {})",
            window.id(),
            uuid,
            inserted.is_some()
        );
    }
}

/// Deterministic frame-apply pass: canvas world -> screen (display-local) ->
/// global via the owning display's bounds, written directly with
/// `reposition` then `resize`. Skips suspended windows and never touches
/// windows whose native frame no longer lives on the canvas display.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn canvas_apply_frames(
    mut worlds: ResMut<CanvasWorlds>,
    mut windows: Query<
        (Entity, &mut crate::manager::Window, Option<&Unmanaged>),
        With<CanvasManaged>,
    >,
    displays: Query<&Display>,
) {
    for (display_uuid, world) in worlds.worlds.iter_mut() {
        let Some(display) = displays
            .iter()
            .find(|display| display.uuid().eq_ignore_ascii_case(display_uuid))
        else {
            warn!("canvas: display {display_uuid} gone, skipping apply");
            continue;
        };
        let bounds = display.bounds();
        for (_, mut window, unmanaged) in &mut windows {
            if matches!(
                unmanaged,
                Some(Unmanaged::Minimized | Unmanaged::Hidden)
            ) {
                if let Some(surface) = world.surfaces.get_mut(&window.id()) {
                    surface.suspended = true;
                }
                continue;
            }
            let Some(surface) = world.surfaces.get_mut(&window.id()) else {
                continue;
            };
            surface.suspended = false;

            let top_left = canvas_to_screen(
                Pt::new(surface.world.x, surface.world.y),
                world.camera,
                world.zoom,
            );
            let bottom_right = canvas_to_screen(
                Pt::new(
                    surface.world.x + surface.world.w,
                    surface.world.y + surface.world.h,
                ),
                world.camera,
                world.zoom,
            );
            let target = IRect::from_corners(
                IVec2::new(
                    bounds.min.x + top_left.x.round() as i32,
                    bounds.min.y + top_left.y.round() as i32,
                ),
                IVec2::new(
                    bounds.min.x + bottom_right.x.round() as i32,
                    bounds.min.y + bottom_right.y.round() as i32,
                ),
            );
            if surface.last_requested == Some(target) && surface.last_observed == target {
                continue;
            }

            // Position before size: the same order Paneru's staged resize
            // path uses (see `resize_staging_origin`).
            window.reposition(target.min);
            window.resize(target.size());
            surface.last_requested = Some(target);
            match window.update_frame() {
                Ok(actual) => {
                    surface.last_observed = actual;
                    surface.constrained = actual.size() != target.size();
                    if surface.constrained {
                        debug!(
                            "canvas: window {} constrained (requested {:?}, actual {:?})",
                            window.id(),
                            target.size(),
                            actual.size()
                        );
                    }
                }
                Err(err) => {
                    warn!("canvas: update_frame failed for {}: {err}", window.id());
                    surface.last_observed = target;
                }
            }
        }
    }
}

/// Advances fling momentum for every world; frame-rate independent.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn canvas_momentum_tick(mut worlds: ResMut<CanvasWorlds>, time: Res<Time>) {
    let dt = time.delta();
    if dt.is_zero() {
        return;
    }
    for world in worlds.worlds.values_mut() {
        if world.momentum.is_moving()
            && let Some(delta) = world.momentum.tick(dt)
        {
            world.camera = world.camera.add(delta);
        }
    }
}

/// Drops surfaces when their window loses the `CanvasManaged` marker
/// (destroy, move-away, config migration).
#[allow(clippy::needless_pass_by_value)]
pub(super) fn canvas_cleanup_removed(
    mut removed: RemovedComponents<CanvasManaged>,
    mut worlds: ResMut<CanvasWorlds>,
    windows: Query<&crate::manager::Window>,
) {
    for entity in removed.read() {
        let Ok(window) = windows.get(entity) else {
            continue;
        };
        let id = window.id();
        for world in worlds.worlds.values_mut() {
            if world.surfaces.remove(&id).is_some() {
                debug!("canvas: removed surface for window {id}");
                break;
            }
        }
    }
}

/// Reacts to config reloads. Order of operations:
/// 1. Exclusion always wins: any world whose display is now excluded (or no
///    longer canvas-listed) is released; its windows lose `CanvasManaged` and
///    `Unmanaged`, and the existing `On<Remove, Unmanaged>` observer re-inserts
///    them into the normal strips.
/// 2. Displays newly listed as Canvas get an empty world; their existing strip
///    windows are converted (removed from strips, marked floating + canvas)
///    and seeded from their actual frames on the next pass.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn canvas_migrate_on_reload(
    config: Res<Config>,
    mut worlds: ResMut<CanvasWorlds>,
    mut commands: Commands,
    windows: Query<(Entity, &crate::manager::Window)>,
    canvas_windows: Query<Entity, With<CanvasManaged>>,
    mut workspaces: Query<&mut crate::ecs::layout::LayoutStrip>,
    displays: Query<&Display>,
) {
    if !config.is_changed() {
        return;
    }
    let canvas_for = |uuid: &str| {
        config.is_canvas_display(uuid) && !config.excludes_display_uuid(uuid)
    };

    // 1. Release worlds that are no longer Canvas (or now excluded).
    let stale = worlds
        .worlds
        .keys()
        .filter(|uuid| !canvas_for(uuid))
        .cloned()
        .collect::<Vec<_>>();
    for uuid in stale {
        let Some(world) = worlds.worlds.remove(&uuid) else {
            continue;
        };
        let count = world.surfaces.len();
        for win_id in world.surfaces.keys() {
            let Some((entity, _)) = windows.iter().find(|(_, w)| w.id() == *win_id) else {
                continue;
            };
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_remove::<CanvasManaged>();
                entity_commands.try_remove::<Unmanaged>();
            }
        }
        info!("canvas: released display {uuid} ({count} surfaces)");
    }

    // 2. New Canvas displays: convert existing strip windows.
    for display in &displays {
        let uuid = display.uuid().to_string();
        if !canvas_for(&uuid) || worlds.worlds.contains_key(&uuid) {
            continue;
        }
        for (entity, window) in &windows {
            let frame = window.frame();
            if !display.contains_frame(frame) || canvas_windows.contains(entity) {
                continue;
            }
            for mut strip in &mut workspaces {
                if strip.contains(entity) {
                    strip.remove(entity);
                }
            }
            if let Ok(mut entity_commands) = commands.get_entity(entity) {
                entity_commands.try_insert(Unmanaged::Floating);
                entity_commands.try_insert(CanvasManaged);
            }
        }
        info!("canvas: display {uuid} now Canvas-managed");
        worlds.worlds.insert(uuid, CanvasWorld::default());
    }
}

/// Returns the world-space bounding box of all (non-suspended) surfaces.
pub fn canvas_world_bbox(world: &CanvasWorld) -> Option<Rect> {
    all_windows_bbox(
        world
            .surfaces
            .values()
            .filter(|surface| !surface.suspended)
            .map(|surface| surface.world),
    )
}

/// `canvas pan <dx> <dy>`: moves the camera by screen px (content follows).
pub fn canvas_pan(world: &mut CanvasWorld, dx: f64, dy: f64) {
    world.camera = world.camera.add(Pt::new(dx, dy));
    world.momentum.stop();
}

/// `canvas zoom <factor> <x> <y>`: multiplies zoom, anchored at the
/// display-local screen point so the world point under it stays fixed.
pub fn canvas_zoom(world: &mut CanvasWorld, factor: f64, screen_x: f64, screen_y: f64) {
    let new_zoom = clamp_zoom(world.zoom * factor);
    world.camera = zoom_anchor_camera(
        world.camera,
        world.zoom,
        Pt::new(screen_x, screen_y),
        new_zoom,
    );
    world.zoom = new_zoom;
    world.momentum.stop();
}

/// `canvas home`: zoom 1.0 with the world origin at the viewport center.
pub fn canvas_home(world: &mut CanvasWorld, viewport: Rect) {
    world.camera = home_camera(viewport);
    world.zoom = 1.0;
    world.momentum.stop();
}

/// `canvas fit-all`: frames the world bounding box with `FIT_MARGIN` inset.
pub fn canvas_fit_all(world: &mut CanvasWorld, viewport: Rect) {
    let Some(bbox) = canvas_world_bbox(world) else {
        return;
    };
    let (camera, zoom) = zoom_to_fit(bbox, viewport, FIT_MARGIN);
    world.camera = camera;
    world.zoom = zoom;
    world.momentum.stop();
}

/// `canvas snap`: tightens near-miss edges to exact adjacency within the
/// world. Windows are processed in stable `WinID` order; each window's
/// candidates are computed against the current geometry (windows already
/// processed are at their snapped positions), so two windows never shift
/// into each other and the result is deterministic. Diagonal/corner contacts
/// are rejected by `snap_candidates`.
pub fn canvas_snap(world: &mut CanvasWorld) {
    let mut rects = world
        .surfaces
        .iter()
        .filter(|(_, surface)| !surface.suspended)
        .map(|(win_id, surface)| (*win_id, snap_rect(surface.world)))
        .collect::<BTreeMap<_, _>>();
    let win_ids = rects.keys().copied().collect::<Vec<_>>();
    for win_id in win_ids {
        let rect = *rects.get(&win_id).expect("snap rect exists");
        let others = rects
            .iter()
            .filter(|(other, _)| **other != win_id)
            .map(|(_, other)| *other)
            .collect::<Vec<_>>();
        let candidates = snap_candidates(&rect, &others);
        let mut best: Option<(Pt, f64)> = None;
        for (edge_value, edge_candidates, axis) in [
            (rect.x_low, candidates.left.iter(), Side::Left),
            (rect.x_high, candidates.right.iter(), Side::Right),
            (rect.y_low, candidates.top.iter(), Side::Top),
            (rect.y_high, candidates.bottom.iter(), Side::Bottom),
        ] {
            for candidate in edge_candidates {
                let shift = candidate - edge_value;
                let magnitude = shift.abs();
                if magnitude <= SNAP_ENGAGE
                    && best.is_none_or(|(_, best_magnitude)| magnitude < best_magnitude)
                {
                    let delta = match axis {
                        Side::Left | Side::Right => Pt::new(shift, 0.0),
                        Side::Top | Side::Bottom => Pt::new(0.0, shift),
                    };
                    best = Some((delta, magnitude));
                }
            }
        }
        let Some((delta, _)) = best else {
            continue;
        };
        let moved = Rect::new(
            rect.x_low + delta.x,
            rect.y_low + delta.y,
            rect.width(),
            rect.height(),
        );
        rects.insert(win_id, snap_rect(moved));
        if let Some(surface) = world.surfaces.get_mut(&win_id) {
            surface.world = moved;
        }
    }
}

fn snap_rect(rect: Rect) -> SnapRect {
    SnapRect {
        x_low: rect.x,
        x_high: rect.x + rect.w,
        y_low: rect.y,
        y_high: rect.y + rect.h,
    }
}
