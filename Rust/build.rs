use std::env;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/app.ico");
    resource
        .compile()
        .expect("failed to embed Windows executable icon");
}
