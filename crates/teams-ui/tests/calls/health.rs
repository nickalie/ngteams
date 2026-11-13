//! The Call health pane: More → Settings → Call health opens it beside the stage in a 1:1 call and in a group call
//! (whose Settings also mute notifications); it shows the call's network, audio, video and screen sharing as the
//! engine reads them, each section's page from its View more, reads them again while it is open, and stops once it
//! closes or another pane takes its place. Reading them is no call action.

use std::sync::Arc;

use gpui::VisualTestContext;
use teams_core::calling::{MediaHealth, NetworkHealth, StreamHealth};
use teams_core::{CallHealth, CallStatus};
use teams_ui::call_stage::{CallStage, HEALTH_REFRESH};

use super::{
    AB, actions, call_windows, calling, click, draw, drawn, in_group_call, no_name_in, registered, shell_window, stage,
    status, trace,
};

/// A 1:1 call's audio both ways on a sound network, no video, nothing shared (made up).
fn audio_call() -> CallHealth {
    let audio = |packets| StreamHealth {
        bitrate_kbps: Some(32.0),
        packets,
        loss_percent: Some(0.5),
        jitter_ms: Some(4.0),
        round_trip_ms: None,
        frame_rate: None,
        resolution: None,
        codec: Some("opus".to_owned()),
        hardware: None,
    };
    CallHealth {
        network: NetworkHealth {
            round_trip_ms: Some(42.0),
            received_loss_percent: Some(0.5),
            received_jitter_ms: Some(4.0),
            sent_loss_percent: Some(0.0),
            available_send_kbps: Some(2_500.0),
            available_receive_kbps: None,
        },
        audio: MediaHealth {
            sent: Some(audio(1_200)),
            received: Some(audio(1_180)),
        },
        ..CallHealth::default()
    }
}

/// How often the pane asked for the call's health so far.
fn reads(rig: &crate::Rig) -> usize {
    rig.core.asked().iter().filter(|asked| **asked == "call_health").count()
}

/// Opens More → Settings (a submenu, reached by the keyboard as a user's arrows reach it).
fn open_settings(window: &mut VisualTestContext) {
    for downs in 1..=12 {
        click(window, "call-more");
        let keys = format!("{} right", vec!["down"; downs].join(" "));
        window.simulate_keystrokes(&keys);
        if drawn(window, "call-more-call-health") {
            return;
        }
        window.simulate_keystrokes("escape escape");
        draw(window);
    }
    panic!("Call health isn't in More's Settings");
}

/// More → Settings → Call health.
fn open_call_health(window: &mut VisualTestContext) {
    open_settings(window);
    click(window, "call-more-call-health");
}

/// The pane shows its overview of a read: when it was read, a card per section; Audio's View more opens its page,
/// whose Back comes back.
fn shows_audio_alone(window: &mut VisualTestContext) {
    assert!(drawn(window, "call-health-pane"));
    assert!(drawn(window, "call-health-updated"), "when it was read");
    for section in [
        "call-health-network",
        "call-health-audio",
        "call-health-video",
        "call-health-sharing",
    ] {
        assert!(drawn(window, section), "{section}");
    }
    click(window, "call-health-more-audio");
    assert!(drawn(window, "call-health-page-audio"));
    assert!(!drawn(window, "call-health-network"), "the page alone");
    click(window, "call-health-back");
    assert!(!drawn(window, "call-health-page-audio"));
    assert!(drawn(window, "call-health-network"));
}

#[test]
fn call_health_shows_the_call_s_measures_and_reads_them_again_while_it_is_open() {
    let rig = calling();
    *rig.core.call_health.lock().unwrap() = Some(audio_call());
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);
    let before = actions(&rig).len();
    assert_eq!(reads(&rig), 0, "nothing is read before the pane opens");

    open_call_health(window);
    shows_audio_alone(window);
    assert_eq!(reads(&rig), 1);

    window.executor().advance_clock(HEALTH_REFRESH);
    draw(window);
    assert_eq!(reads(&rig), 2, "read again while open");
    assert_eq!(
        actions(&rig).len(),
        before,
        "reading the call's health is no call action"
    );

    // another pane takes its place: it reads no more
    click(window, "call-roster");
    assert!(!drawn(window, "call-health-pane"));
    window.executor().advance_clock(HEALTH_REFRESH * 2);
    draw(window);
    assert_eq!(reads(&rig), 2);

    open_call_health(window);
    assert!(drawn(window, "call-health-pane"));
    assert!(!drawn(window, "call-roster-pane"));
    click(window, "call-health-close");
    assert!(!drawn(window, "call-health-pane"));
    let read = reads(&rig);
    window.executor().advance_clock(HEALTH_REFRESH * 2);
    draw(window);
    assert_eq!(reads(&rig), read, "closed, it reads no more");
    no_name_in(&written);
}

#[test]
fn a_call_without_media_shows_every_measure_unknown_and_a_group_call_s_settings_offer_it() {
    let rig = calling();
    let (trace, _) = trace();
    let (windows, window) = in_group_call(&rig, &trace);
    open_settings(window);
    assert!(
        drawn(window, "call-more-mute-notifications"),
        "beside Mute notifications"
    );
    click(window, "call-more-call-health");
    assert!(drawn(window, "call-health-pane"));
    assert!(drawn(window, "call-health-network"), "its sections, every measure --");
    assert!(!drawn(window, "call-health-updated"), "no health read (yet)");
    click(window, "call-health-more-network");
    assert!(drawn(window, "call-health-page-network"));
    click(window, "call-health-close");
    assert!(!drawn(window, "call-health-pane"));

    // a health shown (the tour's) stays while the reads bring none
    let handle = rig.cx.read(|cx| windows.read(cx).window()).unwrap();
    rig.cx.update(|cx| {
        let stage = handle.read(cx).unwrap().view().clone().downcast::<CallStage>().unwrap();
        stage.update(cx, |stage, cx| stage.show_health_reading(audio_call(), cx));
    });
    assert!(drawn(window, "call-health-updated"));
    assert!(!drawn(window, "call-health-page-network"), "it opens on its overview");
    window.executor().advance_clock(HEALTH_REFRESH);
    assert!(drawn(window, "call-health-updated"));
    rig.cx.update(|cx| {
        let stage = handle.read(cx).unwrap().view().clone().downcast::<CallStage>().unwrap();
        stage.update(cx, |stage, cx| stage.show_health_section(3, cx));
    });
    assert!(drawn(window, "call-health-page-sharing"));
}
