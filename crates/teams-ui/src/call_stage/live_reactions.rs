//! The live reactions of a call, as Teams shows them in every layout (the gallery, the speaker view, a share with its
//! strip), the account's own too:
//!
//! - **On the sender's tile** while it is on the stage: the reaction's 3D emoji from Teams' CDN, its size from the
//!   tile's (up to 140 px; the CDN's 56, 112 or 140 px file), centred on the tile with its top at the tile's middle. Its 102 frames play once over
//!   Teams' 4.25 s while it fades in and out, and the tile's picture dims meanwhile. Until the 3D emoji is read, or
//!   when the CDN has none, the picker's small animation loops in its place. A newer reaction of the same person takes
//!   its place.
//! - **Floating** while the sender's tile is not on the stage (on another page, Focus on content): a small animated
//!   emoji rises a little from the stage's bottom middle, fading in and out within [`FLOAT_FOR`]; several at once rise
//!   side by side in lanes.
//!
//! Neither carries a name, as in Teams. Both are drawn on the stage's own clock (the executor's: a test moves it),
//! one frame after another only while a reaction shows: no reaction on screen asks for no frame. With "Play
//! animations" off, each stands still until it goes.
use std::time::{Duration, Instant};

use super::*;

/// How long a reaction floats when its sender's tile is not on the stage.
pub(super) const FLOAT_FOR: Duration = Duration::from_millis(2_000);
/// A floating emoji's side, px.
const FLOAT_EMOJI: f32 = 36.0;
/// How far a floating reaction rises, px.
const FLOAT_RISE: f32 = 56.0;
/// Where a floating reaction starts, above the stage's bottom, px.
const FLOAT_BOTTOM: f32 = 80.0;
/// The space between the lanes of floating reactions, px.
const FLOAT_LANE: f32 = 200.0;
/// How many lanes there are before the next reaction takes the first lane again.
const LANES: u64 = 4;
/// How far a floating reaction sways to each side as it rises, px.
const SWAY: f32 = 4.0;
/// How opaque a tile's picture is while a reaction shows on it.
const DIMMED: f32 = 0.4;
/// When a reaction on a tile is seen, as shares of its life: hidden, then fading in, shown, and fading out.
const TILE_FADE: [(f32, f32); 5] = [(0.0, 0.0), (0.157, 0.0), (0.314, 1.0), (0.814, 1.0), (1.0, 0.0)];

/// The emoji's side on a tile `height` px tall, as Teams sizes it: 140 px on a large tile, 112 on a gallery's, 56 on a
/// strip's (less on a smaller one).
#[must_use]
pub(super) fn tile_emoji(height: f32) -> f32 {
    if height >= 480.0 {
        140.0
    } else if height >= 240.0 {
        112.0
    } else {
        (height * 0.43).clamp(16.0, 56.0)
    }
}

/// How opaque a reaction on a tile is at `t` of its life (0 to 1), by [`TILE_FADE`], eased.
#[must_use]
pub(super) fn tile_opacity(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    TILE_FADE.windows(2).find(|pair| t <= pair[1].0).map_or(0.0, |pair| {
        let ((from, a), (to, b)) = (pair[0], pair[1]);
        let x = if to > from { (t - from) / (to - from) } else { 1.0 };
        let eased = x * x * (3.0 - 2.0 * x);
        a + (b - a) * eased
    })
}

/// Where a floating reaction is at `t` of its life (0 as it comes, 1 as it goes): how far up its rise (0 to 1), how
/// far to the side (-1 to 1) and how opaque it is.
#[must_use]
pub(super) fn float_at(t: f32) -> (f32, f32, f32) {
    let t = t.clamp(0.0, 1.0);
    // fast at first, slower as it goes up
    let up = 1.0 - (1.0 - t) * (1.0 - t);
    let side = (t * std::f32::consts::TAU).sin();
    let opacity = if t < 0.15 {
        t / 0.15
    } else if t > 0.7 {
        (1.0 - t) / 0.3
    } else {
        1.0
    };
    (up, side, opacity.clamp(0.0, 1.0))
}

/// How far into a life of `life` a reaction sent at `at` is at `now`, from 0 to 1 (1 and on: gone).
fn share_of(at: Instant, now: Instant, life: Duration) -> f32 {
    now.saturating_duration_since(at).as_secs_f32() / life.as_secs_f32()
}

/// The reactions of the frame drawn: the newest of each sender that still shows, and which of them a tile drew.
#[derive(Default)]
pub(super) struct Frame {
    now: Option<Instant>,
    newest: Vec<calls::LiveReaction>,
    on_tiles: Vec<u64>,
    /// Some reaction showed: the window draws its next frame.
    pub(super) shown: bool,
}

/// The live reactions of the frame drawn, and the stage drawn again as the next one is due to go.
#[derive(Default)]
pub(super) struct Reactions {
    pub(super) frame: Frame,
    timer: Option<Task<()>>,
}

impl CallStage {
    /// Takes the reactions that show in this frame, before its tiles are drawn.
    pub(super) fn take_reactions(&mut self, active: &ActiveCall, cx: &App) {
        let now = cx.background_executor().now();
        let mut newest: Vec<calls::LiveReaction> = Vec::new();
        for reaction in active.reactions.iter().rev() {
            let alive = now.saturating_duration_since(reaction.at) < calls::REACTION_SHOWN;
            if alive && !newest.iter().any(|known| known.from == reaction.from) {
                newest.push(reaction.clone());
            }
        }
        self.reactions.frame = Frame {
            now: Some(now),
            newest,
            on_tiles: Vec::new(),
            shown: false,
        };
    }

    /// `face` of the tile of `mri` (`selector`), with the reaction its person sent showing on it (an emoji of
    /// `emoji` px; the picture under it `dim`med, as Teams dims an avatar, not a video).
    pub(super) fn reacted(
        &mut self,
        mri: Option<&str>,
        face: AnyElement,
        (selector, emoji, dim): (&str, f32, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(now) = self.reactions.frame.now else {
            return face;
        };
        let Some(reaction) = mri.and_then(|mri| {
            self.reactions
                .frame
                .newest
                .iter()
                .find(|reaction| reaction.from.as_str() == mri)
                .cloned()
        }) else {
            return face;
        };
        self.reactions.frame.on_tiles.push(reaction.seq);
        self.reactions.frame.shown = true;
        let moving = crate::motion::on(cx);
        let opacity = if moving {
            tile_opacity(share_of(reaction.at, now, calls::REACTION_SHOWN))
        } else {
            1.0
        };
        let key = key_of(&reaction);
        let side = emoji_3d_side(emoji);
        let emoji_3d = moving
            .then(|| crate::reactions::call_emoji_3d(&reaction.reaction, side, &self.core, cx))
            .flatten();
        let animated = (moving && emoji_3d.is_none())
            .then(|| crate::reactions::animation(key, &self.core, cx))
            .flatten();
        let selector = format!("{selector}-reaction");
        let picture = match emoji_3d {
            Some(frames) => {
                let frame =
                    crate::reactions::frame_at(now.saturating_duration_since(reaction.at), frames.frame_count());
                played_once(frames, frame, emoji, &selector)
            }
            None => playing(key, emoji, animated, &selector),
        };
        let emoji = div()
            .id(SharedString::from(format!("call-reaction-on-tile-{}", reaction.seq)))
            .absolute()
            .left_0()
            .right_0()
            .top(relative(0.5))
            .flex()
            .justify_center()
            .opacity(opacity)
            .child(picture);
        div()
            .relative()
            .size_full()
            .child(div().size_full().when(dim, |face| face.opacity(DIMMED)).child(face))
            .child(emoji)
            .into_any_element()
    }

    /// The reactions whose senders' tiles are not on the stage, floating up from its bottom middle (`area`: the
    /// stage's size, px); none while none floats.
    pub(super) fn reactions_floating(&mut self, area: (f32, f32), cx: &mut Context<Self>) -> Option<AnyElement> {
        let now = self.reactions.frame.now?;
        let floating: Vec<calls::LiveReaction> = self
            .reactions
            .frame
            .newest
            .iter()
            .filter(|reaction| !self.reactions.frame.on_tiles.contains(&reaction.seq))
            .filter(|reaction| now.saturating_duration_since(reaction.at) < FLOAT_FOR)
            .cloned()
            .collect();
        if floating.is_empty() {
            return None;
        }
        self.reactions.frame.shown = true;
        let moving = crate::motion::on(cx);
        let middle = area.0 / 2.0;
        let floats: Vec<AnyElement> = floating
            .iter()
            // the oldest first, as they came
            .rev()
            .enumerate()
            .map(|(index, reaction)| {
                let key = key_of(reaction);
                let animated = moving
                    .then(|| crate::reactions::animation(key, &self.core, cx))
                    .flatten();
                let (up, side, opacity) = if moving {
                    float_at(share_of(reaction.at, now, FLOAT_FOR))
                } else {
                    (0.0, 0.0, 1.0)
                };
                let lane = as_px(reaction.seq % LANES) - (as_px(LANES) - 1.0) / 2.0;
                let left = (middle + lane * FLOAT_LANE + side * SWAY - FLOAT_EMOJI / 2.0)
                    .clamp(0.0, (area.0 - FLOAT_EMOJI).max(0.0));
                let selector = format!("call-reaction-{index}");
                div()
                    .id(SharedString::from(format!("call-reaction-floating-{}", reaction.seq)))
                    .absolute()
                    .left(px(left))
                    .bottom(px(FLOAT_BOTTOM + up * FLOAT_RISE))
                    .opacity(opacity)
                    .child(playing(key, FLOAT_EMOJI, animated, &selector))
                    .into_any_element()
            })
            .collect();
        Some(
            div()
                .debug_selector(|| "call-reactions".to_owned())
                .absolute()
                .left_0()
                .bottom_0()
                .size_full()
                .children(floats)
                .into_any_element(),
        )
    }

    /// While a live reaction shows, the stage is drawn again as the next one is due to go (with "Play animations"
    /// off, the reactions stand still and only this redraws them).
    pub(super) fn float_reactions(&mut self, active: &ActiveCall, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let Some(due) = active
            .reactions
            .iter()
            .flat_map(|reaction| [reaction.at + FLOAT_FOR, reaction.at + calls::REACTION_SHOWN])
            .map(|end| end.saturating_duration_since(now))
            .filter(|due| !due.is_zero())
            .min()
        else {
            self.reactions.timer = None;
            return;
        };
        self.reactions.timer = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(due).await;
            // a stage that is gone has nothing left to draw
            let _ = this.update(cx, |this, cx| {
                if let Some(active) = this.live(cx) {
                    this.float_reactions(&active, cx);
                }
                cx.notify();
            });
        }));
    }

    /// Whether a reaction showed on the stage drawn last: the window draws its next frame while one does.
    #[must_use]
    pub fn reactions_shown_now(&self) -> bool {
        self.reactions.frame.shown
    }
}

/// The side of the 3D emoji read for an emoji drawn `size` px: the smallest Teams' CDN has that is not smaller, else its
/// largest.
#[must_use]
pub(super) fn emoji_3d_side(size: f32) -> u16 {
    let sides = teams_proto::EMOJI_3D_SIDES;
    sides
        .iter()
        .copied()
        .find(|side| f32::from(*side) >= size)
        .unwrap_or(sides[sides.len() - 1])
}

/// Frame `frame` of the 3D emoji `frames`, `size` px square (test selectors `selector` and `<selector>-3d`).
fn played_once(frames: std::sync::Arc<gpui::RenderImage>, frame: usize, size: f32, selector: &str) -> AnyElement {
    let shown = selector.to_owned();
    let three_d = format!("{selector}-3d");
    div()
        .debug_selector(move || shown.clone())
        .child(
            div()
                .debug_selector(move || three_d.clone())
                .size(px(size))
                .flex_none()
                .child(
                    gpui::canvas(
                        |_, _, _| (),
                        move |bounds, (), window, _| {
                            // a frame it cannot draw leaves the tile as it is
                            let _ = window.paint_image(bounds, bounds, gpui::Corners::default(), frames, frame, false);
                        },
                    )
                    .size_full(),
                ),
        )
        .into_any_element()
}

/// The emoji key `reaction` is drawn by.
fn key_of(reaction: &calls::LiveReaction) -> &str {
    toolbar::REACTIONS
        .iter()
        .find(|(name, _, _)| *name == reaction.reaction)
        .map_or(reaction.reaction.as_str(), |(_, key, _)| key)
}

/// `key`'s emoji `size` px square (test selector `selector`), playing its animation when `animated` holds one
/// (`<selector>-playing`).
fn playing(key: &str, size: f32, animated: Option<std::sync::Arc<gpui::RenderImage>>, selector: &str) -> AnyElement {
    let shown = selector.to_owned();
    let playing = animated.is_some().then(|| format!("{selector}-playing"));
    div()
        .debug_selector(move || shown.clone())
        .child(
            div()
                .when_some(playing, |emoji, playing| emoji.debug_selector(move || playing.clone()))
                .child(crate::reactions::emoji_playing(key, px(size), animated)),
        )
        .into_any_element()
}

fn as_px(count: u64) -> f32 {
    f32::from(u16::try_from(count).unwrap_or(u16::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reaction_on_a_tile_shows_after_a_moment_then_fades_out_as_teams_does() {
        assert!(tile_opacity(0.0).abs() < 1e-6);
        assert!(tile_opacity(0.15).abs() < 1e-6);
        assert!(tile_opacity(0.25) > 0.2 && tile_opacity(0.25) < 0.9);
        assert!((tile_opacity(0.5) - 1.0).abs() < 1e-6);
        assert!(tile_opacity(0.9) > 0.0 && tile_opacity(0.9) < 1.0);
        assert!(tile_opacity(1.0).abs() < 1e-6);
    }

    #[test]
    fn the_3d_emoji_read_is_the_smallest_that_is_not_smaller_than_the_emoji() {
        assert_eq!(emoji_3d_side(140.0), 140);
        assert_eq!(emoji_3d_side(112.0), 112);
        assert_eq!(emoji_3d_side(56.0), 56);
        assert_eq!(emoji_3d_side(30.0), 56);
        assert_eq!(emoji_3d_side(120.0), 140);
        assert_eq!(emoji_3d_side(300.0), 140);
    }

    #[test]
    fn the_emoji_follows_the_tile_s_size() {
        assert!((tile_emoji(600.0) - 140.0).abs() < 1e-6);
        assert!((tile_emoji(370.0) - 112.0).abs() < 1e-6);
        assert!((tile_emoji(132.0) - 56.0).abs() < 1e-6);
        assert!(tile_emoji(80.0) < 56.0);
    }

    #[test]
    fn a_floating_reaction_rises_fades_in_then_out_and_sways() {
        let (up, _, opacity) = float_at(0.0);
        assert!(up.abs() < 1e-6 && opacity.abs() < 1e-6);
        let (up_mid, _, opacity_mid) = float_at(0.5);
        assert!((opacity_mid - 1.0).abs() < 1e-6);
        let (up_late, _, opacity_late) = float_at(0.9);
        assert!(up_mid > 0.5 && up_late > up_mid, "{up_mid} {up_late}");
        assert!(opacity_late < 0.5);
        let (up_end, _, opacity_end) = float_at(1.0);
        assert!((up_end - 1.0).abs() < 1e-6 && opacity_end.abs() < 1e-6);
        let (_, left, _) = float_at(0.25);
        let (_, right, _) = float_at(0.75);
        assert!(left > 0.9 && right < -0.9);
    }
}
