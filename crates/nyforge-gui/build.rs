// Windows: embed the icon and a manifest (DPI awareness, normal user level; the app asks for
// administrator rights itself, through the normal UAC prompt, when the person chooses to).
fn main() {
    println!("cargo:rerun-if-changed=nyforge-gui.rc");
    println!("cargo:rerun-if-changed=../../packaging/nyforge-gui.manifest");
    println!("cargo:rerun-if-changed=../../assets/logo/nyforge.ico");
    #[cfg(windows)]
    {
        embed_resource::compile("nyforge-gui.rc", embed_resource::NONE).manifest_required().unwrap();
    }
}
