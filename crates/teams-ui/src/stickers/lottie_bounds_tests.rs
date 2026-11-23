use std::io::Write as _;

use flate2::Compression;
use flate2::write::GzEncoder;
use rasterlottie::Animation;
use serde_json::{Value, json};

use super::{MAX_COPIES, MAX_DEPTH, check};
use crate::stickers::import::Skip;
use crate::stickers::lottie::{frames, renderer};

fn animation(layers: &Value, assets: &Value) -> Value {
    json!({
        "_synthetic": "made up for the bounds tests; no capture, no real sticker",
        "v": "5.5.2", "fr": 30, "ip": 0, "op": 30, "w": 64, "h": 64,
        "layers": layers, "assets": assets,
    })
}

fn checked(json: &Value) -> Result<(), Skip> {
    check(&Animation::from_json_str(&json.to_string()).unwrap())
}

fn precomp(id: &str) -> Value {
    json!({ "ty": 0, "nm": "precomp", "refId": id, "ip": 0, "op": 30, "st": 0, "w": 64, "h": 64 })
}

fn asset(id: &str, layers: &Value) -> Value {
    json!({ "id": id, "layers": layers })
}

fn shape_layer(shapes: &Value) -> Value {
    json!({ "ty": 4, "nm": "shapes", "ip": 0, "op": 30, "st": 0, "shapes": shapes })
}

fn repeater(copies: &Value) -> Value {
    json!({ "ty": "rp", "nm": "repeater", "c": copies, "o": { "a": 0, "k": 0 }, "m": 1 })
}

fn ellipse() -> Value {
    json!({ "ty": "el", "nm": "dot", "p": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [8, 8] } })
}

#[test]
fn the_synthetic_sticker_is_in_bounds() {
    let json = include_str!("../../../../tests/fixtures/telegram/synthetic-sticker.json");
    assert_eq!(check(&Animation::from_json_str(json).unwrap()), Ok(()));
}

#[test]
fn a_precomposition_that_holds_itself_is_refused() {
    let own = animation(&json!([precomp("a")]), &json!([asset("a", &json!([precomp("a")]))]));
    assert_eq!(checked(&own), Err(Skip::Unsupported));
    let pair = animation(
        &json!([precomp("a")]),
        &json!([asset("a", &json!([precomp("b")])), asset("b", &json!([precomp("a")]))]),
    );
    assert_eq!(checked(&pair), Err(Skip::Unsupported));
}

fn chain(depth: usize) -> Value {
    let assets: Vec<Value> = (0..depth)
        .map(|level| {
            let inner = if level + 1 < depth {
                json!([precomp(&format!("{}", level + 1))])
            } else {
                json!([])
            };
            asset(&level.to_string(), &inner)
        })
        .collect();
    animation(&json!([precomp("0")]), &Value::Array(assets))
}

#[test]
fn precompositions_nest_at_most_max_depth_deep() {
    assert_eq!(checked(&chain(MAX_DEPTH)), Ok(()));
    assert_eq!(checked(&chain(MAX_DEPTH + 1)), Err(Skip::Unsupported));
}

#[test]
fn a_precomposition_used_many_times_counts_each_time() {
    let fan = |id: &str, next: &str| asset(id, &Value::Array(vec![precomp(next); 100]));
    let json = animation(
        &json!([precomp("a")]),
        &json!([
            fan("a", "b"),
            fan("b", "c"),
            fan("c", "d"),
            asset("d", &json!([shape_layer(&json!([ellipse()]))]))
        ]),
    );
    assert_eq!(checked(&json), Err(Skip::Unsupported));
    let few = animation(
        &json!([precomp("a"), precomp("a"), precomp("missing")]),
        &json!([asset("a", &json!([shape_layer(&json!([ellipse()]))]))]),
    );
    assert_eq!(checked(&few), Ok(()));
}

#[test]
fn a_repeater_makes_at_most_max_copies() {
    let layer = |copies: Value| {
        animation(
            &json!([shape_layer(&json!([ellipse(), repeater(&copies)]))]),
            &json!([]),
        )
    };
    assert_eq!(checked(&layer(json!({ "a": 0, "k": 5 }))), Ok(()));
    assert_eq!(checked(&layer(json!({ "a": 0, "k": [MAX_COPIES] }))), Ok(()));
    assert_eq!(checked(&layer(json!({ "a": 0, "k": 1e30 }))), Err(Skip::Unsupported));
    assert_eq!(checked(&layer(json!({ "a": 0, "k": -1e30 }))), Err(Skip::Unsupported));
    let keyframed = |y: f64| {
        json!({ "a": 1, "k": [
            { "t": 0, "s": [2], "o": { "x": [0.5], "y": [y] }, "i": { "x": [0.5], "y": [1] } },
            { "t": 30, "s": [10] },
        ] })
    };
    assert_eq!(checked(&layer(keyframed(0.0))), Ok(()));
    assert_eq!(checked(&layer(keyframed(1e6))), Err(Skip::Unsupported));
    assert_eq!(
        checked(&layer(json!({ "a": 1, "k": [{ "t": 0, "s": [5e6] }] }))),
        Err(Skip::Unsupported)
    );
}

#[test]
fn nested_repeaters_count_their_copies_together() {
    let group = |items: Value| json!({ "ty": "gr", "nm": "group", "it": items });
    let copies = json!({ "a": 0, "k": 100 });
    let inner = group(json!([ellipse(), repeater(&copies)]));
    let middle = group(json!([inner, repeater(&copies)]));
    let outer = json!([middle, repeater(&copies)]);
    assert_eq!(
        checked(&animation(&json!([shape_layer(&outer)]), &json!([]))),
        Err(Skip::Unsupported)
    );
    let two = json!([group(json!([ellipse(), repeater(&copies)])), repeater(&copies)]);
    assert_eq!(checked(&animation(&json!([shape_layer(&two)]), &json!([]))), Ok(()));
}

#[test]
fn a_tgs_out_of_bounds_is_skipped_before_it_is_drawn() {
    let own = animation(&json!([precomp("a")]), &json!([asset("a", &json!([precomp("a")]))]));
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(own.to_string().as_bytes()).unwrap();
    assert_eq!(
        frames(&encoder.finish().unwrap(), renderer()).err(),
        Some(Skip::Unsupported)
    );
}
