//! Compiles the browser code (web/src/*.ts) to web/dist/app.js, which the server embeds.
//! Needs Node and `npm ci` in the workspace root. Set PANEL_WEB_PREBUILT=1 to skip the
//! compile when web/dist was built separately, e.g. in a Docker stage.

use std::path::Path;
use std::process::Command;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for p in [
        "web/src",
        "web/tsconfig.json",
        "web/index.html",
        "web/styles.css",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(p).display());
    }
    println!("cargo:rerun-if-env-changed=PANEL_WEB_PREBUILT");
    if std::env::var_os("PANEL_WEB_PREBUILT").is_some() {
        return;
    }
    let tsc = root.join("node_modules/.bin/tsc");
    if !tsc.exists() {
        panic!(
            "TypeScript is missing: run `npm ci` in {} before building the panel",
            root.display()
        );
    }
    let status = Command::new(&tsc)
        .args(["-p", "web"])
        .current_dir(&root)
        .status()
        .expect("run tsc");
    assert!(status.success(), "tsc -p web failed; see the errors above");
}
