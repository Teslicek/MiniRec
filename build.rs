use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

include!("src/icon.rs");

const ICON_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/cursor.hlsl");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is not set by cargo"));
    let icon    = out_dir.join("app.ico");
    let script  = out_dir.join("app.rc");

    fs::write(&icon, ico_file()).expect("Cannot write app.ico");
    fs::write(&script, format!("1 ICON \"{}\"\n", icon.display().to_string().replace('\\', "\\\\"))).expect("Cannot write app.rc");
    embed_resource::compile(&script, embed_resource::NONE).manifest_optional().expect("Cannot compile the icon resource");
    compile_shaders(&out_dir);
}

fn compile_shaders(out_dir: &Path) {
    let compiler = shader_compiler();

    for (entry, profile, output) in [("vs", "vs_4_0", "cursor_vs.cso"), ("ps", "ps_4_0", "cursor_ps.cso")] {
        let status = Command::new(&compiler)
            .args(["/nologo", "/O3", "/E", entry, "/T", profile, "/Fo"])
            .arg(out_dir.join(output))
            .arg("src/cursor.hlsl")
            .status()
            .expect("Cannot run fxc.exe");

        assert!(status.success(), "fxc.exe failed to compile the {entry} cursor shader");
    }
}

fn shader_compiler() -> PathBuf {
    let kits = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = fs::read_dir(kits)
        .unwrap_or_else(|e| panic!("Cannot read {}: {e}", kits.display()))
        .map(|entry| entry.expect("Cannot read a Windows Kits folder entry").path().join("x64").join("fxc.exe"))
        .filter(|path| path.exists())
        .collect();

    versions.sort();
    versions.pop().unwrap_or_else(|| panic!("fxc.exe was not found under {}. Install the Windows SDK.", kits.display()))
}

fn ico_file() -> Vec<u8> {
    let images: Vec<Vec<u8>> = ICON_SIZES.iter().map(|&size| bmp_image(size)).collect();
    let mut file = Vec::new();
    let mut offset = 6 + 16 * images.len() as u32;

    file.extend(0u16.to_le_bytes());
    file.extend(1u16.to_le_bytes());
    file.extend((images.len() as u16).to_le_bytes());

    for (size, image) in ICON_SIZES.iter().zip(&images) {
        let dimension = if *size >= 256 { 0 } else { *size as u8 };

        file.extend([dimension, dimension, 0, 0]);
        file.extend(1u16.to_le_bytes());
        file.extend(32u16.to_le_bytes());
        file.extend((image.len() as u32).to_le_bytes());
        file.extend(offset.to_le_bytes());
        offset += image.len() as u32;
    }

    for image in images {
        file.extend(image);
    }

    file
}

fn bmp_image(size: u32) -> Vec<u8> {
    let pixels = icon_pixels(size, false);
    let mask_row = size.div_ceil(32) * 4;
    let mut image = Vec::new();

    image.extend(40u32.to_le_bytes());
    image.extend((size as i32).to_le_bytes());
    image.extend((size as i32 * 2).to_le_bytes());
    image.extend(1u16.to_le_bytes());
    image.extend(32u16.to_le_bytes());
    image.extend([0u8; 24]);

    for row in (0..size).rev() {
        for pixel in &pixels[(row * size) as usize..((row + 1) * size) as usize] {
            image.extend(pixel);
        }
    }

    image.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    image
}
