//! Infinite-canvas engine for Canvas-mode displays.
//!
//! Self-contained f64 math: camera/world transforms, cursor-anchored zoom,
//! frame-rate-independent momentum, edge snapping with hysteresis, and
//! snap-adjacency clustering. No bevy types here — the ECS adapter layer
//! (CanvasPlugin) converts between this engine and Paneru entities.

pub mod camera;
pub mod cluster;
pub mod geom;
pub mod momentum;
pub mod snap;
