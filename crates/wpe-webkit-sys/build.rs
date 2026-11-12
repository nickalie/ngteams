// Generated once by gir (`split_build_rs`, see Gir.toml) and kept by hand since: gir regenerates only
// build_version.rs, which this script does not use (the crate binds no version-gated API).
//
// Probes the system's wpe-webkit-2.0 through pkg-config and, when there is none, says which package brings it.

fn main() {
    // Linux only: the crate is empty on other targets (macOS signs in through WKWebView, Windows through WebView2).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    if std::env::var("DOCS_RS").is_ok() {
        // prevent linking libraries to avoid documentation failure
        return;
    }

    if let Err(s) = system_deps::Config::new().probe() {
        println!(
            "cargo::error={}: this crate needs WPE WebKit 2.54 or newer: install the system's wpewebkit (sudo pacman -S \
             wpewebkit; docs/development.md, WPE WebKit)",
            s.to_string().replace('\n', " ")
        );
    }
}
