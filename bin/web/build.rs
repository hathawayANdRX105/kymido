use std::path::{Path, PathBuf};
use std::process::Command;

/// Locate the ui-kit checkout whose assets/src must enter the Tailwind input.
/// cargo metadata 是唯一权威：它给出本仓 Cargo.lock 钉死的 rev 的确切 checkout。
/// 此前按 mtime 挑「最新」checkout——ferrite / kymido 共享 CARGO_HOME，另一仓
/// bump rev 后其 checkout 更新，就会把别的仓的 kit 版本混进本仓 CSS（rsx 编译
/// 对 A、样式导入对 B 的版本倾斜）。sibling checkout 仅作 metadata 失败时的
/// 本地兜底；两者皆失交由调用方硬失败（与下方 tailwind 体积护栏同一哲学）。
fn uikit_dir(manifest: &Path) -> Result<PathBuf, String> {
    let meta = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--manifest-path",
            &manifest.join("Cargo.toml").to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("cargo metadata 执行失败: {e}"))?;
    if !meta.status.success() {
        return Err(format!(
            "cargo metadata 退出码 {:?}: {}",
            meta.status.code(),
            String::from_utf8_lossy(&meta.stderr).trim()
        ));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&meta.stdout)
        .map_err(|e| format!("cargo metadata 输出解析失败: {e}"))?;
    if let Some(pkg) = parsed["packages"]
        .as_array()
        .and_then(|pkgs| pkgs.iter().find(|p| p["name"] == "ui-kit"))
    {
        let mp = PathBuf::from(
            pkg["manifest_path"]
                .as_str()
                .ok_or("ui-kit 包缺 manifest_path 字段")?,
        );
        return mp
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "ui-kit manifest_path 无父目录".into());
    }
    // 本地 path 迭代（临时把依赖指到 sibling）时 metadata 同样能解析到，走不到
    // 这里；这段只是 cargo 自身异常时的最后退路。
    if let Some(projects) = manifest.ancestors().nth(3) {
        let sibling = projects.join("ui-kit");
        if sibling.join("src").is_dir() {
            return Ok(sibling);
        }
    }
    Err("cargo metadata 里没有 ui-kit 包，且无 sibling ../ui-kit".into())
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
    // 解析不到就硬失败——静默缺 kit 类的「编译通过但 UI 半裸」在共享
    // CARGO_HOME 的多仓机器上是常态而非意外，必须在编译期拦住。
    let kit = uikit_dir(&manifest).unwrap_or_else(|e| {
        let repo_root = manifest
            .ancestors()
            .nth(2)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        eprintln!(
            "ui-kit checkout 解析失败（{e}）。tokens/组件/动画 css 无法注入，\
             继续编译会得到缺 kit 类的样式表。修复：cd {repo_root} && cargo fetch \
             （或放一个 sibling ../ui-kit）后重试。"
        );
        std::process::exit(1);
    });
    let kit_root = kit.to_string_lossy().replace('\\', "/");
    // ui-kit.css 是组件类层（ui-* 类名）。2026-09 起 ui-kit 把样式从 Rust 常量
    // 搬进了 CSS，rsx 侧直接写类名——不注入这份文件，ui-* 类全部缺失。
    let kit_imports = format!(
        "@import \"{kit_root}/assets/tokens.css\";\n@import \"{kit_root}/assets/ui-kit.css\";\n@import \"{kit_root}/assets/animation.css\";\n@import \"{kit_root}/assets/icon-anim.css\";\n"
    );
    let kit_src = format!("@source \"{kit_root}/src/**/*.rs\";\n");

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
                kit_imports.as_str(),
                kit_src.as_str()
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
                kit_src.as_str()
            ),
        );
    }
    let gen_input = manifest.join(".tailwind.gen-input.css");
    let _ = std::fs::write(&gen_input, &body);

    let local_bin = manifest.join("node_modules/.bin/tailwindcss");
    // 删掉上一次构建的陈旧产物：工具链本次没跑成时，护栏的体积检查必须
    // 看到的是「本次没产出」（0 字节），而不是上一轮的好文件。
    let _ = std::fs::remove_file(&output);

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

    // 护栏：Tailwind 没跑成（node_modules 缺失 / npx 失败 / 无网络）时产物
    // 为空或只有残片——嵌进二进制的后果是「编译通过但页面裸 HTML」，
    // 这种静默降级在 worktree 重建（node_modules 不跟 git）后必现，改成
    // 构建期硬失败 + 修复命令，把发现成本从「上线后肉眼」压到编译时。
    // 正常产物 = 完整设计系统（100KB+）；5KB 下限 = 连 Tailwind 自身
    // preflight（~12KB）都不到的残片，必是坏的。
    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    if size < 5_000 {
        let repo_root = manifest
            .ancestors()
            .nth(2)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        eprintln!(
            "tailwindcss 没有产出可用样式表（{output:?} 仅 {size} 字节）。\
              继续编译会得到零样式的 web UI。修复：cd {repo_root}/bin/web && bun install 后重试。"
        );
        std::process::exit(1);
    }

    println!("cargo:rerun-if-changed=assets/tailwind-input.css");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    // rsx classes live in the web crates; their edits must re-run this script.
    println!("cargo:rerun-if-changed=../../crates/web-ui");
    // kit checkout 随 lockfile rev 变化；纯 rev bump（只动 Cargo.lock）也必须重跑，
    // 否则 CSS 继续用旧 rev 的 kit 资产。
    println!("cargo:rerun-if-changed=../../Cargo.lock");
}
