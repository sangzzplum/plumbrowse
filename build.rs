use std::path::Path;

fn main() {
    let png = Path::new("assets/plumnet.png");
    println!("cargo:rerun-if-changed=assets/plumnet.png");

    if !png.exists() {
        println!("cargo:warning=assets/plumnet.png not found — app icon will be skipped");
        return;
    }

    #[cfg(target_os = "windows")]
    embed_windows_icon(png);
}

#[cfg(target_os = "windows")]
fn embed_windows_icon(png: &Path) {
    use std::fs::File;
    use std::path::PathBuf;
    use ico::{IconDir, IconDirEntry, ResourceType};
    use image::imageops::FilterType;
    use image::GenericImageView;

    let img = image::open(png).expect("failed to load assets/plumnet.png");
    let (src_w, src_h) = img.dimensions();
    if src_w != src_h {
        println!(
            "cargo:warning=assets/plumnet.png should be square (got {src_w}x{src_h})"
        );
    }

    let mut icon_dir = IconDir::new(ResourceType::Icon);
    for size in [16u32, 32, 48, 64, 128, 256] {
        let rgba = img
            .resize_exact(size, size, FilterType::Lanczos3)
            .to_rgba8();
        let entry = IconDirEntry::encode_as_png(&rgba).expect("encode icon png");
        icon_dir.add_entry(entry);
    }

    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let ico_path = out_dir.join("plumnet.ico");
    let file = File::create(&ico_path).expect("create ico");
    icon_dir.write(file).expect("write ico");

    winresource::WindowsResource::new()
        .set_icon(ico_path.to_string_lossy())
        .set("ProductName", "PlumBrowser")
        .set("FileDescription", "PlumBrowser")
        .compile()
        .expect("embed Windows icon");
}
