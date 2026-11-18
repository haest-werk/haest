//! What a connection may do, decided by the host when it attaches a transport.

use haest_wire::{Access, Origin};

/// Who a connection speaks for, as the host decides it: what it may do and
/// whose input it sends. Nothing the peer sends changes it. More may come
/// (a narrower scope): a host builds one with [`new`](Self::new),
/// [`watch`](Self::watch) or [`command`](Self::command).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Grant {
    /// Watch only, or command too (turns, controls, approval answers).
    pub access: Access,
    /// Whose input its turns are, for the turn gate.
    pub origin: Origin,
}

impl Grant {
    /// `access`, its turns `origin`'s input.
    #[must_use]
    pub fn new(access: Access, origin: Origin) -> Self {
        Self { access, origin }
    }

    /// May only watch: `events`, `snapshot`, `history`.
    #[must_use]
    pub fn watch(origin: Origin) -> Self {
        Self::new(Access::Watch, origin)
    }

    /// May watch and drive: turns, controls and approval answers too.
    #[must_use]
    pub fn command(origin: Origin) -> Self {
        Self::new(Access::Command, origin)
    }
}
