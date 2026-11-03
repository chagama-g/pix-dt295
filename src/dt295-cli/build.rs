//! The version of the build: `DT295_VERSION`, and the resource of dt295.exe.

#[path = "../version.rs"]
mod version;

fn main() {
    let v = version::version();
    version::resource(&v, "dt295.exe", "PIX-DT295 tools");
}
