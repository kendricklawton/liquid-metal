fn main() {
    // Force cargo to recompile when migration files change.
    // refinery's embed_migrations! reads from this directory at compile time,
    // but cargo doesn't know to watch it.
    println!("cargo:rerun-if-changed=../../migrations");

    // Embed the short git SHA so the API can log exactly which build is running.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    println!("cargo:rustc-env=GIT_SHA={}", sha.trim());
}
