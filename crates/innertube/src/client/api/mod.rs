//! Typed results and pure parsers for the read/navigation endpoints (`resolve_url`,
//! `search`, `next`). The parsers turn renderer JSON into light typed models, do no
//! network I/O, and are tested against synthetic fixtures.

use serde_json::Value;

mod account;
mod captions;
mod channel;
mod chat;
mod comments;
mod json;
mod next;
mod notifications;
mod playlists;
mod search;
#[cfg(test)]
mod tests;

pub use account::*;
pub use captions::*;
pub use channel::*;
pub use chat::*;
pub use comments::*;
pub use json::*;
pub use next::*;
pub use notifications::*;
pub use playlists::*;
pub use search::*;
