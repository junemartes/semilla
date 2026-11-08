//! The web page and its scripts. The binary contains all of them, so the UI works offline.
//!
//! Vendored libraries (from the npm registry, with their license files):
//!
//! | Library | Version | License |
//! |---|---|---|
//! | mermaid | 11.16.1 | MIT |
//! | cytoscape | 3.34.1 | MIT |
//! | elkjs | 0.9.3 | EPL-2.0 |
//! | cytoscape-elk | 2.3.0 | MIT |

/// One embedded file.
pub struct Asset {
    /// The URL path, for example `/app.js`.
    pub path: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

const JS: &str = "text/javascript; charset=utf-8";
const TEXT: &str = "text/plain; charset=utf-8";

macro_rules! asset {
    ($path:literal, $type:expr, $file:literal) => {
        Asset { path: $path, content_type: $type, bytes: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/", $file)) }
    };
}

/// All the embedded files.
pub static ASSETS: &[Asset] = &[
    asset!("/", "text/html; charset=utf-8", "index.html"),
    asset!("/index.html", "text/html; charset=utf-8", "index.html"),
    asset!("/app.js", JS, "app.js"),
    asset!("/app.css", "text/css; charset=utf-8", "app.css"),
    asset!("/favicon.svg", "image/svg+xml", "favicon.svg"),
    asset!("/vendor/mermaid/mermaid.min.js", JS, "vendor/mermaid/mermaid.min.js"),
    asset!("/vendor/mermaid/LICENSE", TEXT, "vendor/mermaid/LICENSE"),
    asset!("/vendor/cytoscape/cytoscape.min.js", JS, "vendor/cytoscape/cytoscape.min.js"),
    asset!("/vendor/cytoscape/LICENSE", TEXT, "vendor/cytoscape/LICENSE"),
    asset!("/vendor/elkjs/elk.bundled.js", JS, "vendor/elkjs/elk.bundled.js"),
    asset!("/vendor/elkjs/LICENSE.md", TEXT, "vendor/elkjs/LICENSE.md"),
    asset!("/vendor/cytoscape-elk/cytoscape-elk.js", JS, "vendor/cytoscape-elk/cytoscape-elk.js"),
    asset!("/vendor/cytoscape-elk/LICENSE", TEXT, "vendor/cytoscape-elk/LICENSE"),
    asset!("/vendor/VERSIONS.txt", TEXT, "vendor/VERSIONS.txt"),
];

/// Finds an embedded file by its URL path.
pub fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.path == path)
}
