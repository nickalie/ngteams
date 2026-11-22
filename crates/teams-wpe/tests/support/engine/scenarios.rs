mod input;
mod navigation;
mod refusals;
mod stores;

use serde_json::{Value, json};
use teams_wpe::{
    Event, Geometry, HostConfig, MessageHandler, Rgba, SessionId, SessionSpec, Settings, StartError, ViewId, ViewSpec,
};

use super::{Result, page, pump::Engine};

pub(super) fn spec(session: SessionId) -> ViewSpec {
    ViewSpec {
        session,
        settings: Settings::overlay(true) | Settings::OPEN_WINDOWS_AUTOMATICALLY | Settings::FULLSCREEN,
        scripts: Vec::new(),
        message_handlers: vec![
            MessageHandler {
                name: "bridge".into(),
                world: None,
            },
            MessageHandler {
                name: "private".into(),
                world: Some("isolated".into()),
            },
        ],
        zoom: 1.0,
        background: None,
        geometry: Geometry {
            width: 320,
            height: 240,
            scale: 1.0,
        },
        features_off: Vec::new(),
    }
}

pub(super) fn create(engine: &mut Engine, spec: ViewSpec) -> Result<ViewId> {
    let view = engine.host.create_view(spec);
    let created = engine.wait(|event| matches!(event, Event::ViewCreated { view: id, .. } if *id == view))?;
    assert!(matches!(created, Event::ViewCreated { page_id, .. } if page_id > 0));
    Ok(view)
}

pub(super) fn run(engine: &mut Engine) -> Result {
    eprintln!("native stage: invalid views");
    invalid_views(engine)?;
    let session = engine.host.create_session(SessionSpec::Ephemeral);
    let view = create(engine, spec(session))?;
    navigation::run(engine, view).map_err(|error| format!("navigation: {error}"))?;
    engine.host.close_view(view);
    engine.wait(|event| matches!(event, Event::BuffersRemoved { view: id, .. } if *id == view))?;
    let view = create(engine, spec(session))?;
    engine.load(view, teams_wpe::Load::Uri(page::ONE.into()))?;
    engine.frame(view, 320, 240)?;
    eprintln!("native stage: input");
    input::run(engine, view).map_err(|error| format!("input: {error}"))?;
    eprintln!("native stage: refusals");
    refusals::run(engine, session).map_err(|error| format!("refusals: {error}"))?;
    eprintln!("native stage: stores");
    stores::run(engine).map_err(|error| format!("stores: {error}"))?;
    eprintln!("native stage: lifecycle");
    lifecycle(engine, session, view).map_err(|error| format!("lifecycle: {error}"))?;
    Ok(())
}

fn invalid_views(engine: &mut Engine) -> Result {
    let unknown = engine.host.create_view(spec(SessionId::from_raw(u64::MAX)));
    assert!(
        matches!(engine.wait(|event| matches!(event, Event::ViewFailed { view, .. } if *view == unknown))?, Event::ViewFailed { message, .. } if message.contains("store"))
    );
    let session = engine.host.create_session(SessionSpec::Ephemeral);
    let mut duplicate = spec(session);
    duplicate.message_handlers.push(duplicate.message_handlers[0].clone());
    let failed = engine.host.create_view(duplicate);
    engine.wait(
        |event| matches!(event, Event::ViewFailed { view, message } if *view == failed && message.contains("twice")),
    )?;
    let mut feature = spec(session);
    feature.features_off.push("SyntheticFeatureThatDoesNotExist".into());
    let failed = engine.host.create_view(feature);
    engine.wait(
        |event| matches!(event, Event::ViewFailed { view, message } if *view == failed && message.contains("feature")),
    )?;
    engine.host.drop_session(session);
    engine.host.clear_session(session);
    engine.wait(|event| matches!(event, Event::SessionCleared { session: id, result: Err(_) } if *id == session))?;
    Ok(())
}

fn lifecycle(engine: &mut Engine, session: SessionId, view: ViewId) -> Result {
    engine.host.clear_session(session);
    engine.wait(|event| matches!(event, Event::SessionCleared { session: id, result: Ok(()) } if *id == session))?;
    engine.host.drop_session(session);
    assert_eq!(engine.eval(view, "1+2", None)?, json!(3));
    engine.host.fall_back_to_shm(view);
    if engine.frames.get(&view).is_some_and(|frames| frames.dma) {
        engine.until(view, "document.title", |value| value == "one")?;
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !engine.frames.get(&view).is_some_and(|frames| frames.shm) {
            assert!(
                std::time::Instant::now() < end,
                "dmabuf fall-back produced no SHM frame"
            );
            engine.pump()?;
        }
    }
    engine.host.close_view(view);
    engine.wait(|event| matches!(event, Event::BuffersRemoved { view: id, .. } if *id == view))?;
    assert!(engine.eval(view, "42", None).is_err());
    engine.host.set_background(view, Some(Rgba::TRANSPARENT));
    engine.host.set_visible(view, false);
    engine.host.close_view(view);
    Ok(())
}

/// A second engine, its web processes given an (empty) extension directory; its page is left open for the shutdown
/// to close.
pub(super) fn restart(engine: &mut Engine) -> Result {
    let session = engine.host.create_session(SessionSpec::Ephemeral);
    let view = create(engine, spec(session))?;
    engine.load(view, teams_wpe::Load::Uri(page::TWO.into()))?;
    assert_eq!(engine.eval(view, "document.title", None)?, Value::String("two".into()));
    Ok(())
}

/// An evaluation that never ends, already in `WebKit` (the engine made a page after it), on a page left open for
/// the shutdown to close.
pub(super) fn spinning(engine: &mut Engine) -> Result<async_channel::Receiver<teams_wpe::Evaluation>> {
    let session = engine.host.create_session(SessionSpec::Ephemeral);
    let view = create(engine, spec(session))?;
    engine.load(view, teams_wpe::Load::Uri(page::TWO.into()))?;
    let pending = engine.host.evaluate(view, "for(;;){}".to_owned(), None);
    create(engine, spec(session))?;
    Ok(pending)
}

pub(super) fn with_extensions(mut config: HostConfig, root: &std::path::Path) -> Result<HostConfig> {
    let directory = root.join("extensions");
    std::fs::create_dir_all(&directory)?;
    config.extensions = Some(teams_wpe::WebProcessExtensions {
        directory: directory.clone(),
        data: "synthetic extension data".into(),
        sandbox_paths: vec![directory],
    });
    Ok(config)
}

pub(super) fn refused_restart(config: HostConfig) {
    assert!(matches!(
        teams_wpe::Host::start(config),
        Err(StartError::AlreadyStarted)
    ));
}
