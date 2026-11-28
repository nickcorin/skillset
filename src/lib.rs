//! Vendor agent skills from GitHub at pinned commits into a folder you choose.

mod error;

pub mod content_hash;
pub mod install;
pub mod lock;
pub mod skill;

pub use error::Error;
