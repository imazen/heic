//! Public-API surface snapshots for the PARENT workspace (docs/public-api/).
//! Shared implementation + format docs: the `zenutils-apidoc` crate.
#[test]
fn public_api_surface_docs_are_current() {
    zenutils_apidoc::ApiDoc::new()
        .workspace_dir("..")
        // The parent `heic` crate's default feature set is intentionally
        // empty and hits the backend-selection `compile_error!` in
        // src/lib.rs; `backend-rust` is the smallest platform-independent
        // buildable surface (the snapshot header records the baseline).
        .base_features("heic", "backend-rust")
        .run();
}
