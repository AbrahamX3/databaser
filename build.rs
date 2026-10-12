fn main() {
    // Windows file metadata (Product name / version) and the executable icon.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "Databaser");
        res.set("FileDescription", "Databaser");
        res.set("CompanyName", "Abraham");
        res.set("LegalCopyright", "Copyright (c) 2026 Abraham");
        res.set("OriginalFilename", "databaser.exe");
        // Icon is optional during early setup; only embed if it has been generated.
        let icon = std::path::Path::new("assets/icon.ico");
        if icon.exists() {
            res.set_icon("assets/icon.ico");
            println!("cargo:rerun-if-changed=assets/icon.ico");
        }
        if let Err(e) = res.compile() {
            eprintln!("winresource failed: {e}");
        }
    }

    println!("cargo:rerun-if-changed=build.rs");
}
