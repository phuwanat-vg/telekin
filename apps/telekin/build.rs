//! Build script for the viewer.
//!
//! On Windows this embeds the icon and a version block into the executable.
//! Without it the .exe shows a blank icon in Explorer and the taskbar, and
//! Properties says nothing about what it is — the running app already sets a
//! window icon, but that only exists once the app is running.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/icons/telekin.ico");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../packaging/icons/telekin.ico");
        res.set("ProductName", "Telekin");
        res.set("FileDescription", "Telekin viewer");
        res.set("CompanyName", "IRiSH LAB");
        res.set("LegalCopyright", "phuwanat@IRiSH LAB");
        if let Err(e) = res.compile() {
            // A missing resource compiler must not stop the build: the app is
            // complete without the embedded icon, just less pretty.
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
