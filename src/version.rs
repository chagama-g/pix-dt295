//! The version of a build, for the build scripts of the DLL and the tools (they include
//! this file with `#[path]`). It has the form of the Arch package's `pkgver`,
//! `0.1.0.r126.ge4c1e81`: Cargo.toml's version, the commit count, the commit, and
//! `+dirty` when the sources differ from it. Without git it is Cargo.toml's version alone.
//!
//! [`version`] gives it to the crate as `DT295_VERSION`; [`resource`] makes the version
//! resource of a Windows file (the Details tab of its properties).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub struct Version {
    text: String,
    /// FILEVERSION: Cargo.toml's three numbers and the commit count.
    numbers: [u32; 4],
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    out.status.success().then_some(text)
}

/// Build again when the commit or a source changes. Only what exists: cargo takes a
/// missing file as changed, and would link anew on every build.
fn watch(root: &Path) {
    let reference = git(root, &["symbolic-ref", "-q", "HEAD"]);
    let refs = ["HEAD", "packed-refs"]
        .into_iter()
        .chain(reference.as_deref());
    let git_files = refs.filter_map(|name| git(root, &["rev-parse", "--git-path", name]));
    let sources = ["src", "Cargo.toml", "Cargo.lock"].map(String::from);
    for path in git_files.chain(sources) {
        let path = root.join(path);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

pub fn version() -> Version {
    let var = |name: &str| env::var(name).expect("set by cargo");
    let number = |name: &str| var(name).parse().expect("a number in Cargo.toml's version");
    let root = PathBuf::from(var("CARGO_MANIFEST_DIR")).join("../..");
    watch(&root);

    let commit = git(&root, &["rev-parse", "--short=7", "HEAD"]);
    let count = git(&root, &["rev-list", "--count", "HEAD"]);
    let changes = git(
        &root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            "src",
            "Cargo.toml",
            "Cargo.lock",
        ],
    );
    let mut text = var("CARGO_PKG_VERSION");
    if let (Some(commit), Some(count)) = (&commit, &count) {
        text = format!("{text}.r{count}.g{commit}");
        if changes.is_none_or(|c| !c.is_empty()) {
            text.push_str("+dirty");
        }
    }
    println!("cargo:rustc-env=DT295_VERSION={text}");
    let numbers = [
        number("CARGO_PKG_VERSION_MAJOR"),
        number("CARGO_PKG_VERSION_MINOR"),
        number("CARGO_PKG_VERSION_PATCH"),
        // one of the four 16-bit parts of FILEVERSION
        count.and_then(|c| c.parse().ok()).unwrap_or(0).min(0xFFFF),
    ];
    Version { text, numbers }
}

/// Gives `file` (`dt295.exe`, `BonDriver_DT295.dll`), built by this package, its version
/// resource. MSVC only; without rc.exe the file goes without, and cargo shows a warning.
pub fn resource(version: &Version, file: &str, description: &str) {
    let var = |name: &str| env::var(name).unwrap_or_default();
    if var("CARGO_CFG_TARGET_OS") != "windows" || var("CARGO_CFG_TARGET_ENV") != "msvc" {
        return;
    }
    let [a, b, c, d] = version.numbers;
    let dotted = format!("{a}.{b}.{c}.{d}");
    let comma = format!("{a},{b},{c},{d}");
    let text = &version.text;
    let (stem, is_dll) = match file.strip_suffix(".dll") {
        Some(stem) => (stem, true),
        None => (file.trim_end_matches(".exe"), false),
    };
    // 2 = VFT_DLL, 1 = VFT_APP; 0x40004 = VOS_NT_WINDOWS32
    let file_type = if is_dll { 2 } else { 1 };
    let rc = format!(
        r#"1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEOS 0x40004
FILETYPE {file_type}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "smpn2\0"
      VALUE "FileDescription", "{description}\0"
      VALUE "FileVersion", "{dotted}\0"
      VALUE "InternalName", "{stem}\0"
      VALUE "LegalCopyright", "Copyright (c) 2026 smpn2\0"
      VALUE "OriginalFilename", "{file}\0"
      VALUE "ProductName", "pix-dt295\0"
      VALUE "ProductVersion", "{text}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out_dir = PathBuf::from(var("OUT_DIR"));
    let (source, res) = (
        out_dir.join(format!("{stem}.rc")),
        out_dir.join(format!("{stem}.res")),
    );
    fs::write(&source, rc).expect("cannot write the version resource");

    // rc.exe is in the Windows SDK, on the PATH that cc finds for cl.exe.
    let mut command = Command::new("rc.exe");
    if let Some(cl) = cc::windows_registry::find_tool(&var("TARGET"), "cl.exe") {
        command.envs(cl.get_envs());
    }
    let done = command
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(&source)
        .output();
    match done {
        Ok(out) if out.status.success() => {
            let res = res.display();
            if is_dll {
                println!("cargo:rustc-cdylib-link-arg={res}");
            } else {
                println!("cargo:rustc-link-arg-bin={stem}={res}");
            }
        }
        Ok(out) => println!(
            "cargo:warning=no version resource in {file}: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        ),
        Err(e) => println!("cargo:warning=no version resource in {file}: rc.exe: {e}"),
    }
}
