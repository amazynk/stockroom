//! Compiles the windows (the app window and the compact install window) with the bundled
//! translations.

fn main() {
    // The accessibility tests query elements, which needs Slint's debug info in the bindings.
    println!("cargo:rerun-if-env-changed=SLINT_EMIT_DEBUG_INFO");
    // Directories are scanned recursively, so any markup or catalog edit rebuilds the bindings.
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=translations");
    let debug_info = std::env::var("SLINT_EMIT_DEBUG_INFO").is_ok_and(|v| v == "1");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default());
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("translations")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None)
        .with_debug_info(debug_info);
    if let Err(e) = slint_build::compile_with_output_path("ui/app.slint", out.join("app_ui.rs"), config) {
        panic!("ui/app.slint: {e}");
    }
}
