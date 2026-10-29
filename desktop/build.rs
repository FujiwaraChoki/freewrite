fn main() {
    // Give the Windows executable its icon and version info.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/windows/freewrite.ico");
        res.set("ProductName", "freewrite");
        res.set("FileDescription", "freewrite");
        if let Err(err) = res.compile() {
            println!("cargo:warning=couldn't embed the Windows icon: {err}");
        }
    }
    println!("cargo:rerun-if-env-changed=FREEWRITE_SUPABASE_URL");
    println!("cargo:rerun-if-env-changed=FREEWRITE_SUPABASE_ANON_KEY");
}
