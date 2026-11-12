//! The app's side of the sign-in window's WPE `WebKit` engine, and the types both sides share. Nothing here links
//! `WebKit` or asks pkg-config for it, so the app depends on this crate alone and starts on a system without an engine;
//! only the sibling `ngteams-web` worker (crate `teams-wpe`) needs one.
//!
//! - [`worker::pin_worker`] opens the worker as a descriptor at startup, so an update replacing the install can't swap
//!   it under a running app. The handshake compares the protocol and [`worker::build_identity`].
//! - [`Host::start_worker`] starts it on an [`EngineSource`]: the system's engine adds nothing to its command, a
//!   downloaded runtime sets the library, GIO, `GStreamer` and injected-bundle paths on the worker's command alone.
//! - `AppArmor`: the worker inherits the profile attached to `ngteams` and needs no entry of its own.
#![cfg(target_os = "linux")]

/// `from_raw` and `raw` on the engine's id types: a consumer's stand-in for the engine in its own tests makes ids
/// with them, and logs read them; the engine itself never takes an id it did not hand out.
macro_rules! raw_ids {
    ($($id:ident),+) => {
        $(
            impl $id {
                /// The id numbered `raw` (crate docs: for a consumer's stand-in engine).
                #[must_use]
                pub const fn from_raw(raw: u64) -> Self {
                    Self(raw)
                }

                /// The id's number.
                #[must_use]
                pub const fn raw(self) -> u64 {
                    self.0
                }
            }
        )+
    };
}
pub(crate) use raw_ids;

#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod event;
#[doc(hidden)]
pub mod frames;
#[doc(hidden)]
pub mod host;
mod host_commands;
#[doc(hidden)]
pub mod input;
mod source;
#[doc(hidden)]
pub mod spec;
pub mod worker;

pub use source::{EngineSource, RUNTIME_LOCK_ENV};

pub use config::{
    BufferFormat, ConfigError, DmaBufTarget, FrameMode, HostConfig, MODIFIER_INVALID, MODIFIER_LINEAR,
    WebProcessExtensions, fourcc,
};
pub use event::{
    Area, ContextMenu, EditingCommand, Event, LoadError, LoadErrorKind, LoadEvent, MenuToken, NavigationType,
    OptionItem, OptionKind, OptionMenu, PageState, PolicyKind, PolicyRequest, PolicyToken, ReplyToken, SchemeError,
    SchemeRequest, SchemeResponse, SchemeToken, ScriptMessage, Termination,
};
pub use frames::{BufferId, DmaBuf, Frame, FrameContent, Plane, Rect, ShmRegion};
pub use host::{Evaluation, Host, Load, StartError};
pub use input::{Button, Input, Key, Modifiers, Pointer, PointerKind, Scroll};
pub use spec::{
    Geometry, InjectionTime, MessageHandler, Rgba, SessionId, SessionSpec, Settings, UserScript, ViewId, ViewSpec,
};
