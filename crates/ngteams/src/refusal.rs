//! Build and mode refusals, before account files, credentials or windows are opened.
//!
//! The initial [`refusal`] gate rejects test-support and unmarked binaries before even the WPE worker starts.
//! [`refusal_for`] restricts a lab-marked build to explicitly selected lab mode once the CLI is parsed.

/// The install marker, set only by `scripts/install.sh` and the release workflow when they compile this crate.
pub const INSTALLED: Option<&str> = option_env!("TEAMS_PERSONAL_INSTALL");

/// The lab marker, set only by `scripts/run-lab.sh` when it compiles this crate.
pub const LAB_BUILD: Option<&str> = option_env!("TEAMS_LAB_BUILD");

const TEST_SUPPORT: &str = "built with test-support (cargo test, --all-targets or --all-features unify it in): run a normal build made by scripts/run-lab.sh or scripts/install.sh";
const UNMARKED: &str = "no install or lab build marker: run a build made by scripts/run-lab.sh or scripts/install.sh";
const LAB_ONLY: &str = "a lab build runs only with --lab <alias> (including ngteams secrets --lab <alias>)";

fn marked(marker: Option<&str>) -> bool {
    marker.is_some_and(|value| !value.is_empty())
}

/// Pure build/mode policy; usable in tests without compiling a runnable binary.
#[must_use]
pub fn build_refusal(
    test_support: bool,
    installed: Option<&str>,
    lab_build: Option<&str>,
    lab: bool,
) -> Option<&'static str> {
    if test_support {
        return Some(TEST_SUPPORT);
    }
    if marked(lab_build) {
        return (!lab).then_some(LAB_ONLY);
    }
    (!marked(installed)).then_some(UNMARKED)
}

/// The marker a build carries, as `--build-marker` prints it: `lab` (it wins, as in [`build_refusal`]), `install` or
/// `none`. Packaging reads it to give a lab build its own identity.
#[must_use]
pub fn marker_name(installed: Option<&str>, lab_build: Option<&str>) -> &'static str {
    if marked(lab_build) {
        "lab"
    } else if marked(installed) {
        "install"
    } else {
        "none"
    }
}

/// Whether this binary was built with any crate's test-support (`cargo test`, `--all-targets`, `--all-features`).
#[must_use]
pub fn test_support() -> bool {
    teams_proto::test_support_enabled() || teams_signin::test_support_enabled() || teams_update::test_support_enabled()
}

/// The initial gate, independent of app/broker mode so the inherited WPE worker can start.
#[must_use]
pub fn refusal() -> Option<&'static str> {
    build_refusal(test_support(), INSTALLED, LAB_BUILD, marked(LAB_BUILD))
}

/// The strict gate after CLI parsing: lab-marked binaries may only run an explicitly selected lab mode.
#[must_use]
pub fn refusal_for(lab: bool) -> Option<&'static str> {
    build_refusal(test_support(), INSTALLED, LAB_BUILD, lab)
}
