//! Scoped debug logger modeled on Bun's `BUN_DEBUG_<SCOPE>=1` (`APHRODY_DEBUG_<SCOPE>=1`).

use std::fmt;

/// Lightweight scoped debug logger whose disabled check is a single cached bool (`#[inline]`).
#[derive(Debug, Clone, Copy)]
pub struct ScopedLogger {
    scope: &'static str,
    enabled: bool,
}

impl ScopedLogger {
    /// Construct a logger enabled when `APHRODY_DEBUG_<SCOPE>=1` or `APHRODY_DEBUG_ALL=1` is set.
    #[must_use]
    pub fn for_scope(scope: &'static str) -> Self {
        let quiet = std::env::var_os("APHRODY_DEBUG_QUIET_LOGS").is_some();
        if quiet {
            return Self {
                scope,
                enabled: false,
            };
        }
        let all = std::env::var_os("APHRODY_DEBUG_ALL").is_some();
        let specific = std::env::var_os(format!("APHRODY_DEBUG_{scope}")).is_some();
        Self {
            scope,
            enabled: all || specific,
        }
    }

    #[inline]
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        self.enabled
    }

    #[inline]
    pub fn log(self, args: fmt::Arguments<'_>) {
        if self.enabled {
            self.emit_cold(args);
        }
    }

    #[cold]
    #[inline(never)]
    fn emit_cold(self, args: fmt::Arguments<'_>) {
        eprintln!("[aphrody:{}] {args}", self.scope);
    }
}
