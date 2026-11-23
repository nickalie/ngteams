//! What a Lottie animation may make `rasterlottie` do, checked before it is prepared.
//!
//! `rasterlottie` follows a precomposition's asset and a layer's parents with no depth limit, draws as many repeater
//! copies, repeater steps and polystar points as an animation asks for, draws a matte source again for each layer
//! that uses it, makes canvas-sized pixmaps for each mask, looks up a precomposition's asset through all the assets
//! each time it draws it, and goes through each layer list once per layer for each path expression and each parent: a
//! crafted `.tgs` overflows the stack, panics, hangs or takes all memory. So the animation is refused
//! ([`Skip::Unsupported`]) when its precompositions nest deeper than [`MAX_DEPTH`] (a cycle nests without end, so this
//! refuses it too), a layer list holds more than [`MAX_LAYERS`] layers or the animation more than [`MAX_LAYERS`]
//! assets, a parent chain is longer than [`MAX_PARENTS`], a shape path has an expression, a repeater's copies plus its
//! offset or a polystar's points can be more than [`MAX_COPIES`], or a frame does more than [`MAX_WORK`] work: layers,
//! shapes, path vertices and points, [`MASK_WORK`] for each mask and one for each [`SCANS_PER_WORK`] assets looked
//! through, with each precomposition, matte and copy counted. Values are bounded as the renderer reads them, in
//! `f32`: a number that is not finite there has no bound.

use std::collections::HashMap;
use std::iter;

use rasterlottie::{AnimatedValue, Animation, Layer, LayerType, ShapeItem, ShapePathValue};
use serde_json::{Map, Value};

use super::Skip;

/// The deepest precompositions nest.
pub const MAX_DEPTH: usize = 16;
/// The most layers one layer list (the animation's or an asset's) may hold.
pub const MAX_LAYERS: usize = 1000;
/// The longest chain of parents a layer may have, itself included.
pub const MAX_PARENTS: usize = 16;
/// The most copies one repeater may make with its offset added, and the most points of one polystar.
pub const MAX_COPIES: f64 = 1000.0;
/// The most work one frame may do; a layer, shape, path vertex or point is one.
pub const MAX_WORK: f64 = 250_000.0;
/// The work of one mask: the renderer makes and fills canvas-sized pixmaps for it.
pub const MASK_WORK: f64 = 1000.0;
/// How many assets the renderer looks through for one work.
pub const SCANS_PER_WORK: f64 = 100.0;

/// Whether `animation` stays in the bounds of the module docs.
///
/// # Errors
/// [`Skip::Unsupported`] when it does not.
pub fn check(animation: &Animation) -> Result<(), Skip> {
    if animation.assets.len() > MAX_LAYERS {
        return Err(Skip::Unsupported);
    }
    for layers in iter::once(&animation.layers).chain(animation.assets.iter().map(|asset| &asset.layers)) {
        list(layers)?;
    }
    let mut first = HashMap::new();
    for (position, asset) in animation.assets.iter().enumerate() {
        first.entry(asset.id.as_str()).or_insert(position);
    }
    Walk {
        animation,
        first,
        open: 0,
        done: HashMap::new(),
    }
    .layers(&animation.layers)
    .map(drop)
}

/// What `rasterlottie` does once for each layer list of the animation, used or not.
fn list(layers: &[Layer]) -> Result<(), Skip> {
    if layers.len() > MAX_LAYERS || layers.iter().any(|layer| expressive(&layer.shapes)) {
        return Err(Skip::Unsupported);
    }
    let positions = positions(layers);
    for position in 0..layers.len() {
        let mut current = Some(position);
        for _ in 0..MAX_PARENTS {
            current = current
                .and_then(|at| layers[at].parent)
                .and_then(|parent| positions.get(&parent).copied());
        }
        if current.is_some() {
            return Err(Skip::Unsupported);
        }
    }
    Ok(())
}

fn expressive(items: &[ShapeItem]) -> bool {
    items
        .iter()
        .any(|item| item.path.as_ref().is_some_and(|path| path.expression.is_some()) || expressive(&item.items))
}

/// The position of each layer index; as in `rasterlottie`, the last layer of an index takes it.
fn positions(layers: &[Layer]) -> HashMap<i64, usize> {
    layers
        .iter()
        .enumerate()
        .filter_map(|(position, layer)| layer.index.map(|index| (index, position)))
        .collect()
}

/// The layer a track matte of the layer at `position` is cut by, found as `rasterlottie` finds it.
fn matte_source(layer: &Layer, position: usize, positions: &HashMap<i64, usize>) -> Option<usize> {
    layer.track_matte_mode()?;
    layer
        .matte_parent
        .or_else(|| layer.index.map(|index| index - 1))
        .and_then(|index| positions.get(&index).copied())
        .or_else(|| position.checked_sub(1))
}

/// The work of a layer list and how deep its precompositions nest.
#[derive(Clone, Copy, Default)]
struct Reach {
    work: f64,
    depth: usize,
}

struct Walk<'a> {
    animation: &'a Animation,
    first: HashMap<&'a str, usize>,
    open: usize,
    done: HashMap<&'a str, Reach>,
}

impl<'a> Walk<'a> {
    fn layers(&mut self, layers: &'a [Layer]) -> Result<Reach, Skip> {
        let mut each = Vec::with_capacity(layers.len());
        let mut depth = 0;
        for layer in layers {
            let masks: f64 = layer
                .masks
                .iter()
                .map(|mask| MASK_WORK + path(mask.path.as_ref()))
                .sum();
            let mut work = 1.0 + masks + shapes(&layer.shapes)?;
            if layer.layer_type == LayerType::PRECOMP
                && let Some(id) = layer.ref_id.as_deref()
            {
                let inner = self.asset(id)?;
                let scanned = self
                    .first
                    .get(id)
                    .map_or(self.animation.assets.len(), |position| position + 1);
                work += inner.work + count(scanned) / SCANS_PER_WORK;
                depth = depth.max(inner.depth);
            }
            each.push(within(work)?);
        }
        let positions = positions(layers);
        let mut work = 0.0;
        for (position, layer) in layers.iter().enumerate() {
            let matte = matte_source(layer, position, &positions).map_or(0.0, |source| each[source]);
            work = within(work + each[position] + matte)?;
        }
        Ok(Reach { work, depth })
    }

    fn asset(&mut self, id: &'a str) -> Result<Reach, Skip> {
        if let Some(done) = self.done.get(id) {
            return if self.open + done.depth > MAX_DEPTH {
                Err(Skip::Unsupported)
            } else {
                Ok(*done)
            };
        }
        if self.open >= MAX_DEPTH {
            return Err(Skip::Unsupported);
        }
        // The renderer takes the first asset of an id too; a missing one is the renderer's to refuse.
        let Some(asset) = self.first.get(id).map(|&position| &self.animation.assets[position]) else {
            self.done.insert(id, Reach::default());
            return Ok(Reach::default());
        };
        self.open += 1;
        let inner = self.layers(&asset.layers)?;
        self.open -= 1;
        let reach = Reach {
            work: inner.work,
            depth: inner.depth + 1,
        };
        self.done.insert(id, reach);
        Ok(reach)
    }
}

fn within(work: f64) -> Result<f64, Skip> {
    if work.is_finite() && work <= MAX_WORK {
        Ok(work)
    } else {
        Err(Skip::Unsupported)
    }
}

/// The shapes and points `items` draw: a repeater repeats what comes before it in its list, so each one counts for
/// all of them.
fn shapes(items: &[ShapeItem]) -> Result<f64, Skip> {
    let mut work = 0.0;
    let mut repeat = 1.0;
    for item in items {
        work += 1.0 + shapes(&item.items)?;
        match item.item_type.as_str() {
            "rp" => repeat *= copies(item)?,
            "sr" => work += 2.0 * bounded(most(item.polystar_points().as_ref()))?,
            "sh" => work += path(item.path.as_ref()),
            _ => {}
        }
        within(work * repeat)?;
    }
    within(work * repeat)
}

/// The most copies a repeater makes, at least 1. It steps each copy's transform as many times as the copy's place
/// plus its offset, so the offset counts too.
fn copies(repeater: &ShapeItem) -> Result<f64, Skip> {
    let made = bounded(most(repeater.repeater_copies()))?;
    bounded(made + most(repeater.repeater_offset()))?;
    Ok(made.max(1.0))
}

fn bounded(most: f64) -> Result<f64, Skip> {
    if most.is_finite() && most <= MAX_COPIES {
        Ok(most)
    } else {
        Err(Skip::Unsupported)
    }
}

/// The most vertices a path has in any of its keyframes.
fn path(value: Option<&ShapePathValue>) -> f64 {
    count(value.and_then(|value| value.keyframes.as_ref()).map_or(0, vertices))
}

fn count(count: usize) -> f64 {
    u32::try_from(count).map_or(f64::INFINITY, f64::from)
}

fn vertices(value: &Value) -> usize {
    let own = value.get("v").and_then(Value::as_array).map_or(0, Vec::len);
    let inner = match value {
        Value::Array(items) => items.iter().map(vertices).max(),
        Value::Object(fields) => fields.values().map(vertices).max(),
        _ => None,
    };
    own.max(inner.unwrap_or(0))
}

fn most(value: Option<&AnimatedValue>) -> f64 {
    value.and_then(|value| value.keyframes.as_ref()).map_or(0.0, reach)
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
    let reach = ends * 2.0f64.mul_add(easing, 1.0);
    if reach.is_finite() { reach } else { f64::INFINITY }
}

fn largest<'v>(values: impl Iterator<Item = Option<&'v Value>>, least: f64) -> f64 {
    values.flatten().map(magnitude).fold(least, f64::max)
}

/// The largest absolute number in `value`, infinite when the renderer's `f32` cannot hold it.
fn magnitude(value: &Value) -> f64 {
    match value {
        Value::Number(number) => number
            .as_f64()
            .map(f64::abs)
            .filter(|size| *size <= f64::from(f32::MAX))
            .unwrap_or(f64::INFINITY),
        Value::Array(items) => items.iter().map(magnitude).fold(0.0, f64::max),
        _ => 0.0,
    }
}

#[cfg(test)]
#[path = "bounds_tests.rs"]
mod tests;
