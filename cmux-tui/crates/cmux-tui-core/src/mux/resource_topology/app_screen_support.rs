//! `app-screens-v1` support that needs the mux's creation fences.

use super::*;

impl Mux {
    /// The creation fences every topology effect holds from its pre-check to
    /// its commit (handoff, then execution). A screen kind commit takes them
    /// too, so it never lands between an effect's app-rule check and the
    /// effect, and the effect never changes memory that its commit check
    /// would then refuse.
    pub(crate) fn app_screen_fences(&self) -> (MutexGuard<'_, ()>, MutexGuard<'_, ()>) {
        let handoff = self.resource_creation_handoff.lock().unwrap();
        (handoff, self.resource_creation_execution.lock().unwrap())
    }
}

#[cfg(test)]
#[path = "app_screen_race_tests.rs"]
mod tests;
