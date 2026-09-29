use std::path::{Path, PathBuf};
use std::process::Command;

pub fn build() {
    println!("cargo:rerun-if-changed=../vendor/ComputerUse");
    println!("cargo:rerun-if-changed=resources/computer-use/Info.plist");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let output = out_dir.join("computer-use-native");
    match target.as_str() {
        "macos" => {
            let arch = match arch.as_str() {
                "aarch64" => "arm64",
                "x86_64" => "x86_64",
                other => panic!("Computer Use 不支持 macOS 架构 {other}"),
            };
            let mut sources: Vec<_> = std::fs::read_dir("../vendor/ComputerUse/macos")
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "swift")
                })
                .collect();
            sources.sort();
            let app = out_dir.join("Codey Computer Use.app");
            let contents = app.join("Contents");
            std::fs::create_dir_all(contents.join("MacOS")).unwrap();
            std::fs::copy(
                "resources/computer-use/Info.plist",
                contents.join("Info.plist"),
            )
            .unwrap();
            let executable = contents.join("MacOS/codey-computer-use");
            checked(
                Command::new("xcrun")
                    .args(["swiftc", "-O", "-swift-version", "5", "-target"])
                    .arg(format!("{arch}-apple-macosx14.0"))
                    .args(sources)
                    .arg("-o")
                    .arg(&executable),
            );
            checked(
                Command::new("codesign")
                    .args([
                        "--force",
                        "--sign",
                        "-",
                        "--identifier",
                        "com.codey.computer-use",
                    ])
                    .arg(&app),
            );
            std::fs::copy(executable, &output).unwrap();
            std::fs::copy(
                contents.join("_CodeSignature/CodeResources"),
                out_dir.join("computer-use-signature"),
            )
            .unwrap();
        }
        "windows" => {
            let arch = match arch.as_str() {
                "aarch64" => "arm64",
                "x86_64" => "amd64",
                other => panic!("Computer Use 不支持 Windows 架构 {other}"),
            };
            checked(
                Command::new("go")
                    .args(["build", "-trimpath", "-ldflags=-s -w -H=windowsgui", "-o"])
                    .arg(&output)
                    .arg(".")
                    .current_dir(Path::new("../vendor/ComputerUse/windows"))
                    .env("GOOS", "windows")
                    .env("GOARCH", arch)
                    .env("CGO_ENABLED", "0"),
            );
        }
        _ => {}
    }
}

fn checked(command: &mut Command) {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("运行 Computer Use 编译器失败：{error}"));
    assert!(
        output.status.success(),
        "Computer Use 构建失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
