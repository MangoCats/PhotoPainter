use std::process::Command;

fn main() {
    let version = Command::new("git")
        .args(["describe", "--always", "--tags", "--dirty"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_VERSION={}", version.trim());
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");
}
