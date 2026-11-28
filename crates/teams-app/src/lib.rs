//! The app shell without GPUI: what the desktop (`teams-ui`, which re-exports it) and the phone app share above the
//! engine. Nothing here draws, and nothing here sends a request.
//!
//! - [`settings`]: the settings file's data, its parse and render, and its private I/O.
//! - [`ai`]: the AI choices the settings keep (the providers, their kinds, the model of each work).
//! - [`languages`]: the languages captions, chats and drafts are translated into.
//! - [`reactions`]: the reaction keys Teams offers, as the message hover bar's settings keep them.

pub mod ai;
pub mod languages;
pub mod reactions;
pub mod settings;
