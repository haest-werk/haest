//! haest-gui: a native window onto the haest sessions running on this
//! machine, over their control sockets. Experimental. The `haest-gui` binary
//! is the app; this library is its parts, public so the snapshot example
//! (and a test) can drive them without a display.
//!
//! - [`model`]: frames reduced to what the views show (no GPUI, all tested).
//! - [`link`]: the tokio side, finding haests and talking to them as `haest ctl` does.
//! - [`replay`]: a recorded stream played back as if a haest sent it.
//! - [`store`]: the GPUI entity every view reads.
//! - [`ui`]: the views; [`assets`]: the icons they draw.

pub mod assets;
pub mod link;
pub mod model;
pub mod replay;
pub mod store;
pub mod ui;
