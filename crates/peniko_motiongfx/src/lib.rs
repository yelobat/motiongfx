#![no_std]

extern crate alloc;

pub mod interpolation;
pub mod morph;
pub mod trace;

pub mod prelude {
    pub use peniko;
    pub use peniko::kurbo;

    pub use crate::Peniko;
    pub use crate::morph::PathMorph;
    pub use crate::trace::{
        CubicTracer, LineTracer, PathTracer, QuadTracer, Trace,
    };
}

pub use motiongfx;
pub use peniko;

/// Marker for [`Interpolation<Peniko>`] impls on [`peniko`] types.
///
/// [`Interpolation<Peniko>`]: motiongfx::interpolation::Interpolation
pub struct Peniko;
