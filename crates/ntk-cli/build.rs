// Платформа вшивается на компиляции: спрашивать её у операционной системы в
// рантайме — значит однажды спросить не то. Здесь она известна точно.
fn main() {
    let os = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => "darwin",
        Ok(other) => other.to_string().leak(),
        Err(_) => "unknown",
    };
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok(other) => other.to_string().leak(),
        Err(_) => "unknown",
    };
    println!("cargo:rustc-env=NTK_OS={os}");
    println!("cargo:rustc-env=NTK_ARCH={arch}");
}
