//! The frame -> view-model reduction: no GPUI, no sockets, all tested.

pub mod agents;
pub mod session;
pub mod stream;
pub mod timeline;

#[cfg(test)]
mod tests;

pub use agents::{AgentNode, AgentState, AgentTree, MAIN};
pub use session::{Ask, AskId, Conn, SessionModel, agent_of, approval_queue, short};
pub use stream::{AgentPhase, Item, ItemKind, NoticeLevel, Stream, ToolCard, ToolStatus};
pub use timeline::{CacheSplit, ModelCall, Timeline, ToolBar, Turn};
