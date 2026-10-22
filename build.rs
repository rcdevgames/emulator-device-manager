fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "ManDra");
        res.set("FileDescription", "Management Android Emulator");
        res.set("CompanyName", "rcdevgames.net");
        res.set("LegalCopyright", "Copyright 2026 RCDev Games");
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        res.set("FileVersion", env!("CARGO_PKG_VERSION"));
        res.set("OriginalFilename", "devmgr.exe");
        res.set("InternalName", "devmgr");
        res.set("URL", "https://rcdevgames.net/");
        if let Err(e) = res.compile() {
            // Icon/resource embedding is cosmetic; never fail the build over it.
            println!("cargo:warning=winresource failed: {e}");
        }
    }
}
