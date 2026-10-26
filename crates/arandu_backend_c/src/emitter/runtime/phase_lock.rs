//! Standalone C mirror of address-keyed phase-fair lock admission.
use super::super::CEmitter;

impl CEmitter<'_> {
    pub(super) fn emit_phase_lock_runtime(&mut self) {
        self.output.push_str(include_str!("phase_lock.c"));
    }
}
