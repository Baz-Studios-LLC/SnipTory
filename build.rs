fn main() {
    slint_build::compile("ui/history.slint").expect("compile Slint UI");
    winresource::WindowsResource::new()
        .set_icon("assets/sniptory.ico")
        .set_manifest_file("app.manifest")
        .set("ProductName", "SnipTory")
        .set("FileDescription", "SnipTory — Capture. Recall. Paste.")
        .compile()
        .expect("compile Windows application resources");
}
