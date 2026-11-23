use std::io::Write as _;

use flate2::Compression;
use flate2::write::GzEncoder;
use rasterlottie::Animation;
use serde_json::{Value, json};

use super::{MAX_COPIES, MAX_DEPTH, MAX_ID, MAX_LAYERS, MAX_PARENTS, check};
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
    repeater_from(copies, 0.0)
}

fn repeater_from(copies: &Value, offset: f64) -> Value {
    json!({ "ty": "rp", "nm": "repeater", "c": copies, "o": { "a": 0, "k": offset }, "m": 1 })
}

fn null_layer(index: usize, parent: Option<usize>) -> Value {
    let mut layer = json!({ "ty": 3, "nm": "null", "ind": index, "ip": 0, "op": 30, "st": 0 });
    if let Some(parent) = parent {
        layer["parent"] = json!(parent);
    }
    layer
}

fn ellipse() -> Value {
    json!({ "ty": "el", "nm": "dot", "p": { "a": 0, "k": [0, 0] }, "s": { "a": 0, "k": [8, 8] } })
}

#[test]
fn the_synthetic_sticker_is_in_bounds() {
    let json = include_str!("../../../../../tests/fixtures/telegram/synthetic-sticker.json");
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
    Value::Array(
        (0..depth)
            .map(|level| {
                let inner = if level + 1 < depth {
                    json!([precomp(&format!("{}", level + 1))])
                } else {
                    json!([])
                };
                asset(&level.to_string(), &inner)
            })
            .collect(),
    )
}

#[test]
fn precompositions_nest_at_most_max_depth_deep() {
    let root = json!([precomp("0")]);
    assert_eq!(checked(&animation(&root, &chain(MAX_DEPTH))), Ok(()));
    assert_eq!(
        checked(&animation(&root, &chain(MAX_DEPTH + 1))),
        Err(Skip::Unsupported)
    );
}

#[test]
fn a_precomposition_seen_before_still_counts_its_depth() {
    let half = MAX_DEPTH.to_string();
    let twice = json!([precomp(&half), precomp(&half)]);
    assert_eq!(checked(&animation(&twice, &chain(2 * MAX_DEPTH))), Ok(()));
    let deeper = json!([precomp(&half), precomp("0")]);
    assert_eq!(
        checked(&animation(&deeper, &chain(2 * MAX_DEPTH))),
        Err(Skip::Unsupported)
    );
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
    assert_eq!(checked(&layer(json!({ "a": 0, "k": 5000 }))), Err(Skip::Unsupported));
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

#[test]
fn a_repeater_steps_at_most_max_copies_with_its_offset() {
    let one = json!({ "a": 0, "k": 1 });
    let layer = |offset: f64| {
        animation(
            &json!([shape_layer(&json!([ellipse(), repeater_from(&one, offset)]))]),
            &json!([]),
        )
    };
    assert_eq!(checked(&layer(MAX_COPIES - 1.0)), Ok(()));
    assert_eq!(checked(&layer(MAX_COPIES)), Err(Skip::Unsupported));
    assert_eq!(checked(&layer(1e30)), Err(Skip::Unsupported));
}

#[test]
fn a_polystar_has_at_most_max_copies_points() {
    let star = |points: f64| {
        let star = json!({
            "ty": "sr", "nm": "star", "sy": 1, "pt": { "a": 0, "k": points },
            "p": { "a": 0, "k": [0, 0] }, "r": { "a": 0, "k": 0 },
            "or": { "a": 0, "k": 10 }, "os": { "a": 0, "k": 0 },
            "ir": { "a": 0, "k": 5 }, "is": { "a": 0, "k": 0 },
        });
        animation(&json!([shape_layer(&json!([star]))]), &json!([]))
    };
    assert_eq!(checked(&star(5.0)), Ok(()));
    assert_eq!(checked(&star(MAX_COPIES)), Ok(()));
    assert_eq!(checked(&star(MAX_COPIES + 1.0)), Err(Skip::Unsupported));
    assert_eq!(checked(&star(1e30)), Err(Skip::Unsupported));
}

#[test]
fn every_layer_list_holds_at_most_max_layers() {
    let nulls = |count: usize| Value::Array((0..count).map(|index| null_layer(index, None)).collect());
    assert_eq!(checked(&animation(&nulls(MAX_LAYERS), &json!([]))), Ok(()));
    assert_eq!(
        checked(&animation(&nulls(MAX_LAYERS + 1), &json!([]))),
        Err(Skip::Unsupported)
    );
    let unused = json!([asset("unused", &nulls(MAX_LAYERS + 1))]);
    assert_eq!(checked(&animation(&json!([]), &unused)), Err(Skip::Unsupported));
}

#[test]
fn a_parent_chain_is_at_most_max_parents_long() {
    let chain = |length: usize| {
        Value::Array(
            (0..length)
                .map(|index| null_layer(index, (index + 1 < length).then_some(index + 1)))
                .collect(),
        )
    };
    assert_eq!(checked(&animation(&chain(MAX_PARENTS), &json!([]))), Ok(()));
    assert_eq!(
        checked(&animation(&chain(MAX_PARENTS + 1), &json!([]))),
        Err(Skip::Unsupported)
    );
    let unused = json!([asset("unused", &chain(MAX_PARENTS + 1))]);
    assert_eq!(checked(&animation(&json!([]), &unused)), Err(Skip::Unsupported));
    let own = json!([null_layer(1, Some(1))]);
    assert_eq!(checked(&animation(&own, &json!([]))), Err(Skip::Unsupported));
}

#[test]
fn a_shape_path_with_an_expression_is_refused() {
    let path = |expression: Option<&str>| {
        let mut path = json!({ "ty": "sh", "nm": "p", "ks": { "a": 0, "k": { "c": true, "v": [[0, 0]], "i": [[0, 0]], "o": [[0, 0]] } } });
        if let Some(expression) = expression {
            path["ks"]["x"] = json!(expression);
        }
        path
    };
    let plain = shape_layer(&json!([path(None)]));
    assert_eq!(checked(&animation(&json!([plain]), &json!([]))), Ok(()));
    let own = path(Some("thisComp.layer('shapes').content('p').path"));
    let nested = shape_layer(&json!([{ "ty": "gr", "nm": "group", "it": [own] }]));
    assert_eq!(
        checked(&animation(&json!([nested]), &json!([]))),
        Err(Skip::Unsupported)
    );
}

#[test]
fn a_matte_source_counts_for_each_layer_it_cuts() {
    let copies = json!({ "a": 0, "k": MAX_COPIES });
    let heavy = json!([asset(
        "heavy",
        &json!([shape_layer(&json!([ellipse(), repeater(&copies)]))])
    )]);
    let cut = |users: usize| {
        let mut source = precomp("heavy");
        source["ind"] = json!(1);
        source["td"] = json!(1);
        let mut layers = vec![source];
        layers.extend((0..users).map(|index| {
            let mut user = null_layer(index + 2, None);
            user["tt"] = json!(1);
            user["tp"] = json!(1);
            user
        }));
        animation(&Value::Array(layers), &heavy)
    };
    assert_eq!(checked(&cut(30)), Ok(()));
    assert_eq!(checked(&cut(200)), Err(Skip::Unsupported));
}

#[test]
fn each_track_matte_counts_its_canvas_surfaces() {
    let mut source = null_layer(1, None);
    source["td"] = json!(1);
    let mut user = null_layer(2, None);
    user["tt"] = json!(1);
    let matted = |times: usize| {
        let assets = json!([
            asset("pair", &json!([source, user])),
            asset("q", &Value::Array(vec![precomp("pair"); times])),
        ]);
        animation(&Value::Array(vec![precomp("q"); times]), &assets)
    };
    assert_eq!(checked(&matted(5)), Ok(()));
    assert_eq!(checked(&matted(10)), Err(Skip::Unsupported));
}

fn fan_out(times: usize, inner: &Value, filler: usize) -> Value {
    let mut assets: Vec<Value> = (0..filler)
        .map(|index| asset(&format!("f{index}"), &json!([])))
        .collect();
    assets.push(asset("q", &Value::Array(vec![precomp("e"); times])));
    assets.push(asset("e", &json!([inner])));
    animation(&Value::Array(vec![precomp("q"); times]), &Value::Array(assets))
}

fn path_of(vertices: usize) -> Value {
    let points = Value::Array(vec![json!([0, 0]); vertices]);
    json!({ "c": true, "v": points, "i": points, "o": points })
}

#[test]
fn each_mask_counts_as_mask_work() {
    let masked = |count: usize| {
        let mut layer = null_layer(1, None);
        layer["masksProperties"] = Value::Array(vec![json!({ "mode": "a", "pt": { "a": 0, "k": path_of(1) } }); count]);
        animation(&json!([layer]), &json!([]))
    };
    assert_eq!(checked(&masked(3)), Ok(()));
    assert_eq!(checked(&masked(300)), Err(Skip::Unsupported));
    let mut once = null_layer(1, None);
    once["masksProperties"] = json!([{ "mode": "a", "pt": { "a": 0, "k": path_of(1) } }]);
    assert_eq!(checked(&fan_out(20, &once, 0)), Err(Skip::Unsupported));
    let mut long = null_layer(1, None);
    long["masksProperties"] = json!([{ "mode": "a", "pt": { "a": 0, "k": path_of(250_000) } }]);
    assert_eq!(checked(&animation(&json!([long]), &json!([]))), Err(Skip::Unsupported));
}

#[test]
fn the_animation_holds_at_most_max_layers_assets_and_each_lookup_counts() {
    let empty = |count: usize| Value::Array((0..count).map(|index| asset(&index.to_string(), &json!([]))).collect());
    assert_eq!(checked(&animation(&json!([]), &empty(MAX_LAYERS))), Ok(()));
    assert_eq!(
        checked(&animation(&json!([]), &empty(MAX_LAYERS + 1))),
        Err(Skip::Unsupported)
    );
    let inner = null_layer(1, None);
    assert_eq!(checked(&fan_out(200, &inner, 0)), Ok(()));
    assert_eq!(checked(&fan_out(200, &inner, MAX_LAYERS - 2)), Err(Skip::Unsupported));
    let missing = animation(&Value::Array(vec![precomp("missing"); MAX_LAYERS]), &empty(MAX_LAYERS));
    assert_eq!(checked(&missing), Ok(()));
}

#[test]
fn an_asset_id_and_a_reference_are_at_most_max_id_long() {
    let id = |length: usize| "i".repeat(length);
    let held = |length: usize| animation(&json!([]), &json!([asset(&id(length), &json!([]))]));
    assert_eq!(checked(&held(MAX_ID)), Ok(()));
    assert_eq!(checked(&held(MAX_ID + 1)), Err(Skip::Unsupported));
    let used = |length: usize| animation(&json!([precomp(&id(length))]), &json!([]));
    assert_eq!(checked(&used(MAX_ID)), Ok(()));
    assert_eq!(checked(&used(MAX_ID + 1)), Err(Skip::Unsupported));
}

fn keyframed(times: usize, each: &dyn Fn(usize) -> Value) -> Value {
    json!({ "a": 1, "k": (0..times).map(each).collect::<Vec<_>>() })
}

#[test]
fn each_keyframe_counts_as_work() {
    let faded = |times: usize| {
        let mut layer = null_layer(1, None);
        layer["ks"] = json!({ "o": keyframed(times, &|at| json!({ "t": at, "s": [100] })) });
        fan_out(100, &layer, 0)
    };
    assert_eq!(checked(&faded(10)), Ok(()));
    assert_eq!(checked(&faded(100)), Err(Skip::Unsupported));
    let coloured = |times: usize| {
        let fill =
            json!({ "ty": "fl", "nm": "fill", "c": keyframed(times, &|at| json!({ "t": at, "s": [1, 0, 0, 1] })) });
        fan_out(100, &shape_layer(&json!([fill])), 0)
    };
    assert_eq!(checked(&coloured(10)), Ok(()));
    assert_eq!(checked(&coloured(100)), Err(Skip::Unsupported));
    let trimmed = |times: usize| {
        let trim = json!({ "ty": "tm", "nm": "trim", "e": keyframed(times, &|at| json!({ "t": at, "s": [50] })) });
        fan_out(100, &shape_layer(&json!([trim])), 0)
    };
    assert_eq!(checked(&trimmed(10)), Ok(()));
    assert_eq!(checked(&trimmed(100)), Err(Skip::Unsupported));
}

#[test]
fn a_dashed_stroke_counts_as_dash_work() {
    let stroke = |dashes: &Value| {
        let stroke = json!({ "ty": "st", "nm": "line", "c": { "a": 0, "k": [0, 0, 0, 1] }, "w": { "a": 0, "k": 1 }, "d": dashes });
        shape_layer(&json!([ellipse(), stroke]))
    };
    let dashed = json!([{ "n": "d", "v": { "a": 0, "k": 1 } }, { "n": "g", "v": { "a": 0, "k": 1 } }]);
    assert_eq!(checked(&fan_out(30, &stroke(&json!([])), 0)), Ok(()));
    assert_eq!(checked(&fan_out(4, &stroke(&dashed), 0)), Ok(()));
    assert_eq!(checked(&fan_out(30, &stroke(&dashed), 0)), Err(Skip::Unsupported));
}

#[test]
fn each_path_vertex_counts_as_work() {
    let shape =
        |vertices: usize| shape_layer(&json!([{ "ty": "sh", "nm": "p", "ks": { "a": 0, "k": path_of(vertices) } }]));
    assert_eq!(checked(&fan_out(100, &shape(10), 0)), Ok(()));
    assert_eq!(checked(&fan_out(100, &shape(100), 0)), Err(Skip::Unsupported));
    let animated = |times: usize| {
        let path = keyframed(times, &|at| json!({ "t": at, "s": [path_of(10)] }));
        shape_layer(&json!([{ "ty": "sh", "nm": "p", "ks": path }]))
    };
    assert_eq!(checked(&fan_out(100, &animated(1), 0)), Ok(()));
    assert_eq!(checked(&fan_out(100, &animated(10), 0)), Err(Skip::Unsupported));
}

#[test]
fn a_value_out_of_f32_or_not_a_number_has_no_bound() {
    let eased = |from: f64, to: f64, y: f64| {
        json!({ "a": 1, "k": [
            { "t": 0, "s": [from], "o": { "x": [0.5], "y": [y] }, "i": { "x": [0.5], "y": [1] } },
            { "t": 30, "s": [to] },
        ] })
    };
    let star = |points: Value| {
        let star = json!({
            "ty": "sr", "nm": "star", "sy": 1, "pt": points,
            "p": { "a": 0, "k": [0, 0] }, "r": { "a": 0, "k": 0 },
            "or": { "a": 0, "k": 10 }, "os": { "a": 0, "k": 0 },
            "ir": { "a": 0, "k": 5 }, "is": { "a": 0, "k": 0 },
        });
        animation(&json!([shape_layer(&json!([star]))]), &json!([]))
    };
    assert_eq!(checked(&star(eased(0.0, 0.0, 1.0))), Ok(()));
    assert_eq!(checked(&star(eased(0.0, 0.0, 1e308))), Err(Skip::Unsupported));
    let offset = |offset: Value| {
        let repeater = json!({ "ty": "rp", "nm": "repeater", "c": { "a": 0, "k": 1 }, "o": offset, "m": 1 });
        animation(&json!([shape_layer(&json!([ellipse(), repeater]))]), &json!([]))
    };
    assert_eq!(checked(&offset(eased(0.0, 1e-37, 1e30))), Ok(()));
    assert_eq!(checked(&offset(eased(0.0, 1e-37, 1e39))), Err(Skip::Unsupported));
}
