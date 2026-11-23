//! What a Lottie animation may make `rasterlottie` do, checked before it is prepared.
//!
//! `rasterlottie` follows a precomposition's asset with no cycle check and no depth limit, and it draws as many
//! repeater copies as an animation asks for: a crafted `.tgs` overflows the stack, panics or takes all memory. So the
//! animation is refused ([`Skip::Unsupported`]) when its precompositions nest in a cycle or deeper than [`MAX_DEPTH`],
//! when a repeater's copies can be more than [`MAX_COPIES`], or when a frame draws more than [`MAX_WORK`] layers and
//! shapes with each precomposition and repeated copy counted.

use std::collections::HashMap;

use rasterlottie::{AnimatedValue, Animation, Layer, LayerType, ShapeItem};
use serde_json::{Map, Value};

use super::Skip;

/// The deepest precompositions nest.
pub const MAX_DEPTH: usize = 16;
/// The most copies one repeater may make.
pub const MAX_COPIES: f64 = 1000.0;
/// The most layers and shapes one frame may draw.
pub const MAX_WORK: f64 = 250_000.0;

/// Whether `animation` stays in the bounds of the module docs.
///
/// # Errors
/// [`Skip::Unsupported`] when it does not.
pub fn check(animation: &Animation) -> Result<(), Skip> {
    Walk {
        animation,
        open: Vec::new(),
        done: HashMap::new(),
    }
    .layers(&animation.layers)
    .map(drop)
}

struct Walk<'a> {
    animation: &'a Animation,
    open: Vec<&'a str>,
    done: HashMap<&'a str, f64>,
}

impl<'a> Walk<'a> {
    fn layers(&mut self, layers: &'a [Layer]) -> Result<f64, Skip> {
        let mut work = 0.0;
        for layer in layers {
            work += 1.0 + shapes(&layer.shapes)?;
            if layer.layer_type == LayerType::PRECOMP
                && let Some(id) = layer.ref_id.as_deref()
            {
                work += self.asset(id)?;
            }
            within(work)?;
        }
        Ok(work)
    }

    fn asset(&mut self, id: &'a str) -> Result<f64, Skip> {
        if let Some(work) = self.done.get(id) {
            return Ok(*work);
        }
        if self.open.contains(&id) || self.open.len() >= MAX_DEPTH {
            return Err(Skip::Unsupported);
        }
        // The renderer takes the first asset of an id too; a missing one is the renderer's to refuse.
        let Some(asset) = self.animation.assets.iter().find(|asset| asset.id == id) else {
            return Ok(0.0);
        };
        self.open.push(id);
        let work = self.layers(&asset.layers)?;
        self.open.pop();
        self.done.insert(id, work);
        Ok(work)
    }
}

fn within(work: f64) -> Result<f64, Skip> {
    if work > MAX_WORK {
        Err(Skip::Unsupported)
    } else {
        Ok(work)
    }
}

/// The shapes `items` draw: a repeater repeats what comes before it in its list, so each one counts for all of them.
fn shapes(items: &[ShapeItem]) -> Result<f64, Skip> {
    let mut work = 0.0;
    let mut repeat = 1.0;
    for item in items {
        work += 1.0 + shapes(&item.items)?;
        if item.item_type == "rp" {
            repeat *= copies(item.repeater_copies())?;
        }
        within(work * repeat)?;
    }
    within(work * repeat)
}

/// The most copies a repeater whose count is `value` makes, at least 1.
fn copies(value: Option<&AnimatedValue>) -> Result<f64, Skip> {
    let most = value.and_then(|value| value.keyframes.as_ref()).map_or(0.0, reach);
    if most > MAX_COPIES {
        return Err(Skip::Unsupported);
    }
    Ok(most.max(1.0))
}

/// The largest size a value `k` takes. Between two keyframes it moves on a Bézier easing whose handles may overshoot,
/// by at most the largest handle times the distance between them.
fn reach(k: &Value) -> f64 {
    let Value::Array(items) = k else {
        return magnitude(k);
    };
    let keyframes: Vec<&Map<String, Value>> = items.iter().filter_map(Value::as_object).collect();
    if keyframes.is_empty() {
        return magnitude(k);
    }
    let ends = largest(
        keyframes
            .iter()
            .flat_map(|keyframe| ["s", "e"].map(|key| keyframe.get(key))),
        0.0,
    );
    let easing = largest(
        keyframes
            .iter()
            .flat_map(|keyframe| ["o", "i"].map(|key| keyframe.get(key).and_then(|handle| handle.get("y")))),
        1.0,
    );
    ends * 2.0f64.mul_add(easing, 1.0)
}

fn largest<'v>(values: impl Iterator<Item = Option<&'v Value>>, least: f64) -> f64 {
    values.flatten().map(magnitude).fold(least, f64::max)
}

/// The largest absolute number in `value`.
fn magnitude(value: &Value) -> f64 {
    match value {
        Value::Number(number) => number.as_f64().map_or(f64::INFINITY, f64::abs),
        Value::Array(items) => items.iter().map(magnitude).fold(0.0, f64::max),
        _ => 0.0,
    }
}

#[cfg(test)]
#[path = "lottie_bounds_tests.rs"]
mod tests;
