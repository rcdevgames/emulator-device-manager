fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "devmgr");
        res.set("FileDescription", "Android Device Manager");
        res.set("LegalCopyright", "");
        if let Err(e) = res.compile() {
            // Icon/resource embedding is cosmetic; never fail the build over it.
            println!("cargo:warning=winresource failed: {e}");
        }
    }
}
