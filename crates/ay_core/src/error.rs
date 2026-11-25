//! Static zero-vtable error type with `#[cold]` constructors to keep hot loops in L1i.

use std::fmt;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    CapacityExceeded {
        component: &'static str,
        limit: usize,
    },
    ArenaCorrupt {
        slot: u32,
    },
    FuelExhausted {
        consumed: u64,
        limit: u64,
    },
    InvalidArgument {
        context: &'static str,
        detail: String,
    },
    Unsupported {
        feature: &'static str,
    },
    Subsystem {
        subsystem: &'static str,
        message: String,
    },
}

impl CoreError {
    #[cold]
    #[inline(never)]
    #[must_use]
    pub const fn capacity_exceeded(component: &'static str, limit: usize) -> Self {
        Self::CapacityExceeded { component, limit }
    }

    #[cold]
    #[inline(never)]
    #[must_use]
    pub const fn arena_corrupt(slot: u32) -> Self {
        Self::ArenaCorrupt { slot }
    }

    #[cold]
    #[inline(never)]
    #[must_use]
    pub const fn fuel_exhausted(consumed: u64, limit: u64) -> Self {
        Self::FuelExhausted { consumed, limit }
    }

    #[cold]
    #[inline(never)]
    #[must_use]
    pub fn invalid_arg(context: &'static str, detail: impl Into<String>) -> Self {
        Self::InvalidArgument {
            context,
            detail: detail.into(),
        }
    }

    #[cold]
    #[inline(never)]
    #[must_use]
    pub const fn unsupported(feature: &'static str) -> Self {
        Self::Unsupported { feature }
    }

    #[cold]
    #[inline(never)]
    #[must_use]
    pub fn subsystem(subsystem: &'static str, message: impl Into<String>) -> Self {
        Self::Subsystem {
            subsystem,
            message: message.into(),
        }
    }
}

impl fmt::Display for CoreError {
    #[cold]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExceeded { component, limit } => {
                write!(f, "{component}: capacity limit exceeded ({limit})")
            }
            Self::ArenaCorrupt { slot } => write!(f, "arena slot {slot} freelist corrupted"),
            Self::FuelExhausted { consumed, limit } => {
                write!(f, "fuel exhausted: consumed {consumed} / limit {limit}")
            }
            Self::InvalidArgument { context, detail } => {
                write!(f, "invalid argument for {context}: {detail}")
            }
            Self::Unsupported { feature } => {
                write!(f, "feature `{feature}` is not enabled in this build")
            }
            Self::Subsystem { subsystem, message } => write!(f, "[{subsystem}] {message}"),
        }
    }
}

impl std::error::Error for CoreError {}
