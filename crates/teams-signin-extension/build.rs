//! Finds the system's `wpewebkit` web process extension library (pkg-config `wpe-web-process-extension-2.0`) for
//! the module's link: the module calls three of its functions (`webkit_web_page_get_id`,
//! `webkit_uri_request_get_*`), which the web process that loads it has already loaded.

fn main() {
    // Linux only: the crate is empty on other targets (macOS signs in through WKWebView, Windows through WebView2).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo::rerun-if-env-changed=PKG_CONFIG");
    if let Err(error) = pkg_config::Config::new()
        .atleast_version("2.54")
        .probe("wpe-web-process-extension-2.0")
    {
        println!(
            "cargo::error=teams-signin-extension needs WPE WebKit 2.54 or newer ({}): install the system's wpewebkit \
             (sudo pacman -S wpewebkit; docs/development.md, WPE WebKit)",
            error.to_string().replace('\n', " ")
        );
    }
}
