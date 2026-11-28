//! The app shell without GPUI: what the desktop (`teams-ui`, which re-exports it) and the phone app share above the
//! engine. Nothing here draws.
//!
//! - [`accounts`]: the accounts file and its paths, which the host gives.
//! - [`settings`]: the settings file's data, its parse and render, and its private I/O.
//! - [`ai`]: the AI choices the settings keep (the providers, their kinds, the model of each work).
//! - [`languages`]: the languages captions, chats and drafts are translated into.
//! - [`reactions`]: the reaction keys Teams offers, as the message hover bar's settings keep them.
//! - [`signin`]: the sign-in steps of an account, over the window the host gives ([`signin::SignInWindow`]).
//! - [`signin_bound`]: the hosts, request rules and host gate that bound a Microsoft sign-in page.

pub mod accounts;
pub mod ai;
pub mod languages;
pub mod reactions;
pub mod settings;
pub mod signin;
pub mod signin_bound;
