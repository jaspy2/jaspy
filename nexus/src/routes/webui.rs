// Serves the React admin UI. The Vite build output (webui/dist) is embedded
// into the binary by rust-embed at compile time (in debug builds it reads from
// disk, so frontend iteration does not require recompiling Rust). Unknown
// non-API paths fall back to index.html for SPA client-side routing.
use rocket::get;
use rocket::http::ContentType;
use rust_embed::RustEmbed;
use std::path::PathBuf;

#[derive(RustEmbed)]
#[folder = "webui/dist/"]
struct WebUiAssets;

#[get("/<path..>", rank = 20)]
pub fn spa(path: PathBuf) -> Option<(ContentType, Vec<u8>)> {
    let path_str = path.to_string_lossy().to_string();

    // API namespaces and the weathermap statics must 404 instead of falling
    // back to the SPA shell.
    for prefix in ["api", "dev", "weathermap"].iter() {
        if path_str == *prefix || path_str.starts_with(&format!("{}/", prefix)) {
            return None;
        }
    }

    let candidate = if path_str.is_empty() { "index.html".to_string() } else { path_str };
    let (asset, served_name) = match WebUiAssets::get(&candidate) {
        Some(asset) => (asset, candidate),
        None => {
            // A miss under the Vite build-output namespace is a missing file
            // (hashed bundle) and must be a real 404. Anything else is a SPA
            // client route (which may contain dots, e.g. /devices/<fqdn>) and
            // gets the shell.
            if candidate.starts_with("assets/") {
                return None;
            }
            (WebUiAssets::get("index.html")?, "index.html".to_string())
        }
    };

    let content_type = std::path::Path::new(&served_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(ContentType::from_extension)
        .unwrap_or(ContentType::HTML);

    Some((content_type, asset.data.into_owned()))
}
