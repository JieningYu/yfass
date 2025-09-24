//! Abstraction and implementation for FASS platform web services.

pub mod func;
pub mod sandbox;
pub mod user;

pub mod os;

#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
#[repr(transparent)]
pub struct NonExhaustiveMarker(()); // intended for struct constructors to be used normally

const fn dnem() -> NonExhaustiveMarker {
    NonExhaustiveMarker(())
}
