//! Exact typed ISA profile admitted for future Dreamcast machine-code writes.

use typed_isa_core::TypedIsa;

/// The game currently uses the SH-4 single-precision, single-register-transfer
/// profile. Executable addresses passed to this profile are not disc offsets.
pub(crate) type DreamcastSh4Profile = sh4::Sh4Pr0Sz0;

pub(crate) fn profile_id() -> &'static str {
    <DreamcastSh4Profile as TypedIsa>::PROFILE_ID
}

#[cfg(test)]
#[path = "dreamcast_isa_tests.rs"]
mod tests;
