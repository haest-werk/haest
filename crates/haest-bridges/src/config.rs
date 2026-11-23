//! The `hooks` setting bound: haest-mods' [`HookSettings`](haest_mods::HookSettings)
//! (read and checked there) made the one `HookSet` a conversation adds, each
//! entry through this crate's `shell` or `http` bridge (bind.rs).

mod bind;
#[cfg(test)]
mod tests;

pub use bind::HookHost;
