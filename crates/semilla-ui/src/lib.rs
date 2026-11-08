//! The local review UI of semilla (design section 11).
//!
//! The crate has four parts:
//! - a JSON API over the change set, the report, and the view data ([`router`]);
//! - a review state that follows the claims, not the lines ([`review`]);
//! - one web page with its scripts, embedded in the binary ([`assets`]);
//! - a standalone report in HTML or Markdown ([`export`]).
//!
//! The UI is not trusted (design section 3). It only shows the data that the verifier and the
//! indexer give it.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

mod api;
pub mod assets;
mod ctx;
pub mod export;
pub mod fixture;
pub mod graph;
pub mod review;
pub mod search;
pub mod summary;

pub use ctx::Ctx;

/// All the data that the UI shows. The CLI writes it as JSON and the UI reads it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UiData {
    pub changeset: semilla_core::ChangeSet,
    pub report: semilla_core::Report,
    /// The change tree of the whole change set.
    pub tree: semilla_core::view::TreeNode,
    /// The flow views. `FlowGraph.function` is the anchor of the frame.
    #[serde(default)]
    pub flows: Vec<semilla_core::view::FlowGraph>,
    /// The workspace graph: the after version, with the statuses of the merged graph.
    #[serde(default)]
    pub graph: semilla_core::view::WorkspaceGraph,
    /// The source text before and after, by anchor text.
    #[serde(default)]
    pub sources: BTreeMap<String, SourcePair>,
}

/// The source text of one node in the before and the after version.
/// `None` means that the node does not exist in that version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePair {
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
}

/// Builds the HTTP router. The review state is read from `review_file` and written to it.
/// Tests use this function directly; [`serve`] uses it to start the server.
pub fn router(data: UiData, review_file: PathBuf) -> axum::Router {
    api::router(Ctx::new(data), review_file)
}

/// Starts the HTTP server on `addr` and runs it until Ctrl-C.
pub async fn serve(data: UiData, review_file: PathBuf, addr: SocketAddr) -> anyhow::Result<()> {
    let app = router(data, review_file);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    eprintln!("semilla ui: open http://{local}/ (Ctrl-C stops the server)");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
