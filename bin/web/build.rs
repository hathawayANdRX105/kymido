use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate the ui-kit checkout whose assets/src must enter the Tailwind input.
/// Two layouts are supported: a sibling checkout (local iteration, path dep)
/// and the cargo git checkout of the `ui-kit` git dep. Newest rev dir wins so
/// a bumped lockfile revision doesn't get shadowed by a stale checkout.
fn uikit_dir(manifest: &Path) -> Option<PathBuf> {
    if let Some(projects) = manifest.ancestors().nth(3) {
        let sibling = projects.join("ui-kit");
        if sibling.join("src").is_dir() {
            return Some(sibling);
        }
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))?;
    for entry in std::fs::read_dir(cargo_home.join("git/checkouts"))
        .ok()?
        .flatten()
    {
        if !entry.file_name().to_string_lossy().starts_with("ui-kit-") {
            continue;
        }
        let mut revs: Vec<PathBuf> = std::fs::read_dir(entry.path())
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect();
        revs.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        if let Some(latest) = revs.pop() {
            return Some(latest);
        }
    }
    None
}

/// Compile the Tailwind v4 input (`assets/tailwind-input.css`) into a static
/// stylesheet that is embedded into the binary via `include_str!` in `app.rs`.
fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let input = manifest.join("assets/tailwind-input.css");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let output = out_dir.join("tailwind.gen.css");

    // Build the effective input: clone the project CSS, point its first `@source`
    // (bin/web/src) at an absolute path, and keep the crates/web line so every
    // web crate's rsx classes are scanned. Tailwind v4 resolves
    // `@import "tailwindcss"` from the INPUT FILE's directory hierarchy, so this
    // generated input must live inside the crate — we drop it next to the real
    // input and it is gitignored.
    let css = std::fs::read_to_string(&input).unwrap_or_default();
    let crate_src = manifest.join("src").to_string_lossy().replace('\\', "/") + "/**/*.rs";
    // repo root = two ancestors above bin/web; join from there so the glob has
    // no `..` segments that Tailwind's matcher may not normalize.
    let webui_src = manifest
        .ancestors()
        .nth(2)
        .expect("bin/web lives at <repo>/bin/web")
        .join("crates/web-ui/**/*.rs")
        .to_string_lossy()
        .replace('\\', "/");
    // Every `@source` glob must be absolute. Tailwind v4.3 resolves a relative
    // glob only when it carries an explicit `./` prefix, and resolves it
    // against the INPUT file's directory — the generated input in the crate
    // root, not assets/. The relative globs written in assets/tailwind-input.css
    // use inconsistent bases and a bare `../../..` silently matches nothing,
    // which is how the web crates' rsx classes fell out of the stylesheet.
    // So drop any `@source` line from the source CSS and emit two well-known
    // absolute directives instead: this crate's src + the merged web-ui crate.
    // ui-kit（ferrite 家族共享设计系统）的 token 桥 / 动画层 / rsx 类名也要进
    // 产物：kit 组件里的 shadcn 语义类（bg-primary 等）与 animate-in 工具类
    // 定义在 kit 自己的 assets/src 下，不在本仓，必须显式 @import + @source。
    let kit = uikit_dir(&manifest);
    if kit.is_none() {
        println!(
            "cargo:warning=ui-kit checkout not found (sibling ../ui-kit or cargo git checkout); kit classes will be missing from the stylesheet"
        );
    }
    let kit_imports = kit.as_ref().map(|k| {
        let k = k.to_string_lossy().replace('\\', "/");
        // ui-kit.css 是组件类层（ui-* 类名）。2026-09 起 ui-kit 把样式从 Rust 常量
        // 搬进了 CSS，rsx 侧直接写类名——不注入这份文件，ui-* 类全部缺失。
        format!(
            "@import \"{k}/assets/tokens.css\";\n@import \"{k}/assets/ui-kit.css\";\n@import \"{k}/assets/animation.css\";\n"
        )
    });
    let kit_src = kit.as_ref().map(|k| {
        format!(
            "@source \"{}/src/**/*.rs\";\n",
            k.to_string_lossy().replace('\\', "/")
        )
    });

    let mut body = String::with_capacity(css.len());
    let mut injected = false;
    for line in css.lines() {
        if line.trim_start().starts_with("@source ") {
            continue; // strip: we inject our own absolute directives below
        }
        body.push_str(line);
        body.push('\n');
        // Tailwind v4.3 only honors `@source` directives that sit near the top
        // (after the `@import`, before the `@theme`/`@layer` rules); appended at
        // the end they are silently ignored. Inject right after the import line.
        // kit 的 @import 必须与 @import "tailwindcss" 同段（import 语句不得
        // 出现在其他规则之后），三条 @source 紧随其后。
        if !injected && line.trim_start().starts_with("@import ") {
            body.push_str(&format!(
                "{}@source \"{crate_src}\";\n@source \"{webui_src}\";\n{}",
                kit_imports.as_deref().unwrap_or(""),
                kit_src.as_deref().unwrap_or("")
            ));
            injected = true;
        }
    }
    if !injected {
        // No `@import` found (shouldn't happen); fall back to a leading line.
        body.insert_str(
            0,
            &format!(
                "@source \"{crate_src}\";\n@source \"{webui_src}\";\n{}",
                kit_src.as_deref().unwrap_or("")
            ),
        );
    }
    let gen_input = manifest.join(".tailwind.gen-input.css");
    let _ = std::fs::write(&gen_input, &body);

    let local_bin = manifest.join("node_modules/.bin/tailwindcss");

    // Run Tailwind from the crate root so `@import "tailwindcss"` resolves via the
    // crate's node_modules (the generated input lives in OUT_DIR, whose ancestors
    // do not reach node_modules).
    let ran = if local_bin.exists() {
        Command::new(&local_bin)
            .current_dir(&manifest)
            .args([
                "-i",
                gen_input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        false
    };

    if !ran {
        let _ = Command::new("npx")
            .current_dir(&manifest)
            .args([
                "--yes",
                "@tailwindcss/cli",
                "-i",
                gen_input.to_str().unwrap(),
                "-o",
                output.to_str().unwrap(),
            ])
            .status();
    }

    // Guarantee the include target exists so compilation never breaks even if
    // the Tailwind toolchain is unavailable (UI would simply be unstyled).
    if !output.exists() {
        let _ = std::fs::write(&output, "");
    }

    println!("cargo:rerun-if-changed=assets/tailwind-input.css");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    // rsx classes live in the web crates; their edits must re-run this script.
    println!("cargo:rerun-if-changed=../../crates/web-ui");
}
