//! What an account's cloud serves of the services the views offer beside chats, teams, calls and people.
//!
//! Microsoft's cloud serves them all. An own server serves each only when its authz names the service's GTM
//! (`docs/server.md`, "Optional services"): the views hide what it doesn't serve rather than offer something that fails.

use teams_proto::Cloud;

use crate::account::MCPS_GTM;

/// The GTM an own server names while it serves Workflows (the Flow and Power Apps subset, `/workflows/`).
pub const WORKFLOWS_GTM: &str = "workflowsService";
/// The GTM an own server names while it serves the app catalogue and bot invokes.
pub const APPS_GTM: &str = "appsService";
/// The GTM an own server names while it serves a call's live captions.
pub const CAPTIONS_GTM: &str = "captionsService";
/// The GTM an own server names while it serves a call's recording and transcription.
pub const RECORDING_GTM: &str = "recordingService";
/// The GTM an own server names while it serves Copilot (the AI workflows' access).
pub const COPILOT_GTM: &str = "copilotService";

/// The optional services an account's cloud serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per optional service")]
pub struct Capabilities {
    /// Workflows and Power Automate.
    pub workflows: bool,
    /// A meeting's recap (MCPS).
    pub recap: bool,
    /// A call's live captions.
    pub captions: bool,
    /// A call's recording and transcription.
    pub recording: bool,
    /// The app catalogue and bot invokes.
    pub apps: bool,
    /// Copilot: the AI workflows.
    pub copilot: bool,
    /// Teams' own sticker packs in the picker: Microsoft's, which no own server serves.
    pub teams_stickers: bool,
}

impl Capabilities {
    /// Every service: Microsoft's cloud.
    pub const ALL: Self = Self {
        workflows: true,
        recap: true,
        captions: true,
        recording: true,
        apps: true,
        copilot: true,
        teams_stickers: true,
    };

    /// The services `cloud` serves: all on Microsoft's, on an own server those whose GTM `names` holds.
    #[must_use]
    pub fn of(cloud: &Cloud, names: impl Fn(&str) -> bool) -> Self {
        if cloud.is_microsoft() {
            return Self::ALL;
        }
        Self {
            workflows: names(WORKFLOWS_GTM),
            recap: names(MCPS_GTM),
            captions: names(CAPTIONS_GTM),
            recording: names(RECORDING_GTM),
            apps: names(APPS_GTM),
            copilot: names(COPILOT_GTM),
            teams_stickers: false,
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::ALL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microsoft_serves_every_service_and_an_own_server_what_it_names() {
        assert_eq!(Capabilities::of(&Cloud::Microsoft, |_| false), Capabilities::ALL);
        let own = Cloud::own("https://own.example.test").unwrap();
        let none = Capabilities::of(&own, |_| false);
        assert_eq!(
            none,
            Capabilities {
                workflows: false,
                recap: false,
                captions: false,
                recording: false,
                apps: false,
                copilot: false,
                teams_stickers: false,
            }
        );
        let workflows = Capabilities::of(&own, |gtm| gtm == WORKFLOWS_GTM);
        assert!(workflows.workflows && !workflows.apps && !workflows.recap);
        assert!(Capabilities::of(&own, |gtm| gtm == MCPS_GTM).recap);
        assert!(Capabilities::of(&own, |gtm| gtm == APPS_GTM).apps);
        assert!(Capabilities::of(&own, |gtm| gtm == CAPTIONS_GTM).captions);
        assert!(Capabilities::of(&own, |gtm| gtm == RECORDING_GTM).recording);
        assert!(Capabilities::of(&own, |gtm| gtm == COPILOT_GTM).copilot);
        assert!(
            !Capabilities::of(&own, |_| true).teams_stickers,
            "no own server serves Teams' stickers"
        );
        assert_eq!(Capabilities::default(), Capabilities::ALL);
    }
}
