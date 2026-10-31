fn main() {
    slint_build::compile("ui/history.slint").expect("compile Slint UI");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/sniptory.ico")
            .set_manifest_file("app.manifest")
            .set("ProductName", "SnipTory")
            .set("FileDescription", "SnipTory — Capture. Recall. Paste.")
            .compile()
            .expect("compile Windows application resources");
    }
}
