//! Rasterise `assets/logo.svg` into a multi-size `.ico` and embed it as icon
//! resource #1, which is what Explorer shows for the exe and what GPUI loads
//! for the window / taskbar icon.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/logo.svg");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let svg = std::fs::read("assets/logo.svg").expect("read assets/logo.svg");
    let tree = resvg::usvg::Tree::from_data(&svg, &resvg::usvg::Options::default())
        .expect("parse logo.svg");

    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let pngs: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&size| {
            let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
            let scale = size as f32 / tree.size().width();
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );
            pixmap.encode_png().expect("encode png")
        })
        .collect();

    // ICO container with PNG-compressed entries (supported since Vista).
    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (size, png) in sizes.iter().zip(&pngs) {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        ico.push(dim); // width
        ico.push(dim); // height
        ico.push(0); // palette
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for png in &pngs {
        ico.extend_from_slice(png);
    }
    let ico_path = out_dir.join("fastdir.ico");
    std::fs::write(&ico_path, &ico).expect("write ico");
    // Handy for packaging (the installer uses it as its own icon).
    let _ = std::fs::write(PathBuf::from("target").join("fastdir.ico"), &ico);

    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u32> = version
        .split(['.', '-'])
        .filter_map(|p| p.parse().ok())
        .collect();
    parts.resize(4, 0);
    let numeric = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "FastDir"
      VALUE "FileDescription", "FastDir - disk usage analyzer"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "fastdir"
      VALUE "OriginalFilename", "fastdir.exe"
      VALUE "ProductName", "FastDir"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = ico_path.display().to_string().replace('\\', "/"),
    );
    let rc_path = out_dir.join("fastdir.rc");
    std::fs::write(&rc_path, rc).expect("write rc");
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_required()
        .expect("embed icon resource");
}
