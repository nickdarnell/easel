//! Embeds the built web UI (`web/dist`, from `npm run build` in `web/`) into the `frametv` binary.
//!
//! Without a build, a placeholder page is embedded instead, so the Rust code builds and tests without Node.
//!
//! Also records which commit is being built (`EASEL_COMMIT`), so the web page and the log can say
//! exactly which build is running: from the `EASEL_COMMIT` environment variable (the Docker build
//! gets it from CI), or else from git.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

const PLACEHOLDER: &str = "<!doctype html><meta charset=utf-8><title>Easel</title>\
<p style=\"font:16px system-ui;margin:40px\">The web UI wasn't built into this binary. \
Run <code>npm ci &amp;&amp; npm run build</code> in <code>web/</code>, then rebuild.</p>";

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let dist = manifest.join("web").join("dist");
    // Watching the directory re-runs this when any file in it changes, or when it appears.
    println!("cargo:rerun-if-changed={}", dist.display());

    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &mut files);
    } else {
        println!("cargo:warning=web/dist is missing; embedding a placeholder page (run `npm run build` in web/)");
        let page = out.join("placeholder.html");
        fs::write(&page, PLACEHOLDER).unwrap();
        files.push(("index.html".to_owned(), page));
    }
    files.sort();

    let mut code = String::from("/// The web UI's files, by path relative to `web/dist`.\n");
    code.push_str("pub static WEB_FILES: &[(&str, &[u8])] = &[\n");
    for (name, path) in &files {
        writeln!(code, "    ({name:?}, include_bytes!({:?})),", path.display().to_string()).unwrap();
    }
    code.push_str("];\n");
    fs::write(out.join("web_files.rs"), code).unwrap();

    println!("cargo:rustc-env=EASEL_COMMIT={}", commit(&manifest));
}

/// The short hash of the commit being built, or "" if nobody knows.
fn commit(manifest: &Path) -> String {
    println!("cargo:rerun-if-env-changed=EASEL_COMMIT");
    if let Some(sha) = env::var("EASEL_COMMIT").ok().filter(|s| !s.trim().is_empty()) {
        return sha.trim().chars().take(7).collect();
    }
    // Re-run after commits and checkouts (both append to logs/HEAD).
    for f in ["HEAD", "logs/HEAD"] {
        let path = manifest.join(".git").join(f);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(manifest)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn collect(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            files.push((rel, path));
        }
    }
}
