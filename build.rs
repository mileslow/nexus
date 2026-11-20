//! Puts the icon and version details into `Nexus.exe`. Other systems get
//! theirs from the app bundle, so there is nothing to do for them.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    windows::embed_resources();
}

#[cfg(windows)]
mod windows {
    use std::path::PathBuf;

    pub fn embed_resources() {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
            return;
        }
        let version = std::env::var("CARGO_PKG_VERSION").unwrap();
        let icon = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("assets")
            .join("Nexus.ico");
        println!("cargo:rerun-if-changed={}", icon.display());

        // Resource id 1 is the icon GPUI gives every window.
        let script = format!(
            r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {commas},0
PRODUCTVERSION {commas},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "github.com/mileslow/nexus"
      VALUE "FileDescription", "Nexus"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "Nexus"
      VALUE "OriginalFilename", "Nexus.exe"
      VALUE "ProductName", "Nexus"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon = icon.display().to_string().replace('\\', "/"),
            commas = version.replace('.', ","),
        );
        let path = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("nexus.rc");
        std::fs::write(&path, script).unwrap();
        embed_resource::compile(&path, embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
