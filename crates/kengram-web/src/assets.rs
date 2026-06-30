//! Embedded static assets (`/static/{*path}`).
//!
//! Assets in `static/` are baked into the binary via `rust-embed` so the deploy
//! is a single `kengram` binary — no separate asset directory to ship.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "static/"]
struct Assets;

/// Serve an embedded asset by path, with a content-type guessed from the
/// extension and a modest cache window. Vendored libs (e.g. cytoscape) are
/// immutable, but a uniform short max-age keeps hand-edited `app.js`/`app.css`
/// from going stale during development.
pub(crate) async fn static_handler(Path(path): Path<String>) -> Response {
    match Assets::get(&path) {
        Some(content) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    (header::CACHE_CONTROL, "public, max-age=3600"),
                ],
                content.data.into_owned(),
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
}
