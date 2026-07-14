use std::path::Path;

// rust-embed requires webui/dist to exist at compile time. When the frontend
// has not been built (plain `cargo build`, CI, tests), create a placeholder so
// the binary still compiles and serves a hint instead of the UI.
fn main() {
    let dist = Path::new("webui/dist");
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(dist).expect("create webui/dist");
        std::fs::write(
            dist.join("index.html"),
            "<!DOCTYPE html><html><head><title>jaspy</title></head><body>\
             <h1>jaspy web UI was not built</h1>\
             <p>Run <code>npm ci &amp;&amp; npm run build</code> in <code>nexus/webui</code> and rebuild nexus.</p>\
             </body></html>\n",
        )
        .expect("write webui/dist placeholder");
    }
    println!("cargo:rerun-if-changed=webui/dist");
}
