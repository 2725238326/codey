use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result, ensure};
use codey_runtime_core::config_manager::ConfigManager;
use fs2::FileExt;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item};

const MARKETPLACE: &str = "codey-local";
const PLUGIN: &str = "codey-computer-use";
const OWNER: &[u8] = b"Codey managed computer-use plugin v1\n";
const LICENSE: &[u8] = include_bytes!("../../vendor/ComputerUse/LICENSE");
const INFO_PLIST: &[u8] = include_bytes!("../resources/computer-use/Info.plist");
#[cfg(any(target_os = "macos", windows))]
const NATIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/computer-use-native"));
#[cfg(not(any(target_os = "macos", windows)))]
const NATIVE: &[u8] = &[];
#[cfg(target_os = "macos")]
const SIGNATURE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/computer-use-signature"));
static NATIVE_VERSION: LazyLock<String> = LazyLock::new(|| plugin_version(NATIVE));

pub(crate) fn marketplace_path(home: &Path) -> PathBuf {
    marketplace_root(home).join(".agents/plugins/marketplace.json")
}

fn marketplace_root(home: &Path) -> PathBuf {
    home.join(".tmp/marketplaces/codey-local")
}

/// Called only by explicit user preparation; Codex owns installation and enablement.
pub(crate) fn prepare(home: &Path) -> Result<bool> {
    ensure!(!NATIVE.is_empty(), "当前平台不支持桌面工具");
    install(home, NATIVE)
}

pub(crate) fn status(home: &Path) -> Value {
    json!({
        "supported": !NATIVE.is_empty(),
        "ready": is_available(home),
    })
}

pub(crate) fn is_available(home: &Path) -> bool {
    if NATIVE.is_empty() {
        return false;
    }
    let Ok(home) = home.canonicalize() else {
        return false;
    };
    let root = marketplace_root(&home);
    let plugin = root.join("plugins").join(PLUGIN);
    let Some(document) = fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|text| {
            text.trim_start_matches('\u{feff}')
                .parse::<DocumentMut>()
                .ok()
        })
    else {
        return false;
    };
    matches!(registration(&document, &root), Ok(true))
        && marketplace_path(&home).is_file()
        && [".mcp.json", ".codex-plugin/plugin.json", "LICENSE"]
            .iter()
            .all(|path| plugin.join(path).is_file())
        && fs::read(root.join(".codey-owner")).is_ok_and(|bytes| bytes == OWNER)
        && executable_path(&root, NATIVE).is_file()
        && fs::read(plugin.join(".codex-plugin/plugin.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|manifest| manifest["version"].as_str() == Some(NATIVE_VERSION.as_str()))
}

fn plugin_version(native: &[u8]) -> String {
    format!("1.0.0+{}", &crate::fs_util::sha256_hex(native)[..16])
}

fn executable_path(root: &Path, native: &[u8]) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.join("runtime/Codey Computer Use.app/Contents/MacOS/codey-computer-use")
    } else {
        // Windows cannot replace a running executable. Existing sessions keep
        // their old binary until they exit; new installs use the new hash.
        root.join("runtime")
            .join(crate::fs_util::sha256_hex(native))
            .join("codey-computer-use.exe")
    }
}

fn registration(doc: &DocumentMut, root: &Path) -> Result<bool> {
    let Some(markets) = doc.get("marketplaces") else {
        return Ok(false);
    };
    let markets = markets
        .as_table_like()
        .context("marketplaces 必须是配置表")?;
    let Some(entry) = markets.get(MARKETPLACE) else {
        return Ok(false);
    };
    let matches = entry.as_table_like().is_some_and(|entry| {
        entry.get("source_type").and_then(Item::as_str) == Some("local")
            && entry
                .get("source")
                .and_then(Item::as_str)
                .is_some_and(|source| {
                    let target = root.to_string_lossy();
                    Path::new(source.strip_prefix(r"\\?\").unwrap_or(source))
                        == Path::new(target.strip_prefix(r"\\?\").unwrap_or(target.as_ref()))
                })
    });
    ensure!(matches, "codey-local 已使用自定义来源，保留现有配置");
    Ok(true)
}

fn install(home: &Path, native: &[u8]) -> Result<bool> {
    let home = home.canonicalize().context("无法定位 Codex 配置目录")?;
    let root = marketplace_root(&home);
    let manager = ConfigManager::for_home(&home);
    // Refuse custom registrations before writing any managed resources.
    registration(manager.load()?.document(), &root)?;
    private_directory(&home, &root)?;
    let lock_path = root.join(".install.lock");
    reject_non_file(&lock_path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let lock = options.open(&lock_path)?;
    FileExt::try_lock_exclusive(&lock).context("Computer Use 插件正在更新，请稍后重试")?;
    let marker = root.join(".codey-owner");
    reject_non_file(&marker)?;
    if marker.exists() {
        ensure!(fs::read(&marker)? == OWNER, "保留非 Codey 管理的插件目录");
    } else {
        ensure!(
            fs::read_dir(&root)?
                .all(|entry| entry.is_ok_and(|entry| entry.file_name() == ".install.lock")),
            "保留非 Codey 管理的插件目录"
        );
        crate::fs_util::atomic_write_private(&marker, OWNER)?;
    }
    let snapshot = manager.load()?;
    let registered = registration(snapshot.document(), &root)?;
    let executable = executable_path(&root, native);
    let plugin = root.join("plugins").join(PLUGIN);
    let mut changed = write_managed(&root, &executable, native)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    }
    if cfg!(target_os = "macos") {
        let contents = executable.parent().unwrap().parent().unwrap();
        changed |= write_managed(&root, &contents.join("Info.plist"), INFO_PLIST)?;
        #[cfg(target_os = "macos")]
        {
            changed |= write_managed(
                &root,
                &contents.join("_CodeSignature/CodeResources"),
                SIGNATURE,
            )?;
        }
    }
    changed |= write_managed(&root, &plugin.join("LICENSE"), LICENSE)?;
    let manifest = json!({
        "name": PLUGIN, "version": plugin_version(native),
        "description": "读取桌面应用状态，通过辅助功能完成点击、输入和滚动。",
        "author": {"name": "Codey"}, "license": "MIT",
        "mcpServers": "./.mcp.json",
        "interface": {
            "displayName": "Codey Computer Use",
            "shortDescription": "本地桌面应用操作",
            "longDescription": "读取桌面应用并执行点击、输入和滚动，支持 macOS 14 及以上和 Windows。macOS 需要辅助功能与屏幕录制权限；Windows 依赖系统 Windows PowerShell。",
            "developerName": "Codey", "category": "Productivity",
            "capabilities": ["Read", "Write"],
            "defaultPrompt": ["查看当前运行的应用", "帮我操作桌面应用"]
        }
    });
    let mcp = json!({"mcpServers": {"codey_computer_use": {"command": executable, "args": []}}});
    let marketplace = json!({
        "name": MARKETPLACE, "interface": {"displayName": "Codey"},
        "plugins": [{"name": PLUGIN,
            "source": {"source": "local", "path": format!("./plugins/{PLUGIN}")},
            "policy": {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
            "category": "Productivity"}]
    });
    for (path, document) in [
        (plugin.join(".codex-plugin/plugin.json"), manifest),
        (plugin.join(".mcp.json"), mcp),
        (marketplace_path(&home), marketplace),
    ] {
        changed |= write_managed(&root, &path, &serde_json::to_vec_pretty(&document)?)?;
    }
    if !registered {
        let mut doc = snapshot.document().clone();
        if doc.get("marketplaces").is_none() {
            doc["marketplaces"] = toml_edit::table();
        }
        doc["marketplaces"][MARKETPLACE] = toml_edit::table();
        doc["marketplaces"][MARKETPLACE]["source_type"] = toml_edit::value("local");
        doc["marketplaces"][MARKETPLACE]["source"] =
            toml_edit::value(root.to_string_lossy().as_ref());
        manager.replace_document(
            Some(snapshot.revision()),
            doc,
            "注册内置桌面工具",
            "computer_use",
        )?;
        changed = true;
    }
    Ok(changed)
}

fn private_directory(base: &Path, directory: &Path) -> Result<()> {
    let relative = directory.strip_prefix(base)?;
    let mut current = base.to_path_buf();
    for component in relative.components() {
        ensure!(
            matches!(component, std::path::Component::Normal(_)),
            "非法插件目录"
        );
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) => ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "插件目录不能使用符号链接：{}",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&current, fs::Permissions::from_mode(0o700))?;
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn reject_non_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "插件文件不能使用符号链接或目录：{}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn write_managed(root: &Path, path: &Path, bytes: &[u8]) -> Result<bool> {
    private_directory(root, path.parent().context("插件文件缺少父目录")?)?;
    reject_non_file(path)?;
    if fs::metadata(path).is_ok_and(|meta| meta.len() == bytes.len() as u64)
        && fs::read(path)? == bytes
    {
        return Ok(false);
    }
    crate::fs_util::atomic_write_private(path, bytes)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_does_not_create_resources_or_config() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        assert_eq!(status(&missing)["ready"], false);
        assert!(!missing.exists());
        assert_eq!(status(temp.path())["ready"], false);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn explicit_preparation_updates_version_without_enabling_plugin() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        fs::write(
            home.join("config.toml"),
            "[plugins.\"codey-computer-use@codey-local\"]\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(status(home), json!({"supported": true, "ready": false}));
        assert!(prepare(home).unwrap());
        let config = fs::read(home.join("config.toml")).unwrap();
        let manifest =
            marketplace_root(home).join("plugins/codey-computer-use/.codex-plugin/plugin.json");
        let mut previous: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        previous["version"] = json!("1.0.0+previous");
        fs::write(&manifest, serde_json::to_vec(&previous).unwrap()).unwrap();
        assert_eq!(status(home)["ready"], false);
        assert!(prepare(home).unwrap());
        assert_eq!(status(home)["ready"], true);
        assert!(!prepare(home).unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), config);
        assert_eq!(
            ConfigManager::for_home(home).load().unwrap().document()["plugins"]["codey-computer-use@codey-local"]["enabled"].as_bool(),
            Some(false),
        );
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    #[test]
    fn unsupported_platform_does_not_report_preparation_success() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            status(temp.path()),
            json!({"supported": false, "ready": false})
        );
        assert!(prepare(temp.path()).is_err());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[cfg(any(target_os = "macos", windows))]
    #[tokio::test]
    async fn native_mcp_handles_invalid_requests_without_desktop_actions() {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        let temp = tempfile::tempdir().unwrap();
        prepare(temp.path()).unwrap();
        assert!(is_available(temp.path()));
        let root = marketplace_root(&temp.path().canonicalize().unwrap());
        let executable = executable_path(&root, NATIVE);
        #[cfg(target_os = "macos")]
        assert!(
            std::process::Command::new("codesign")
                .args(["--verify", "--strict"])
                .arg(
                    executable
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap()
                )
                .status()
                .unwrap()
                .success()
        );
        let mut child = tokio::process::Command::new(executable)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut input = String::new();
        for value in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!([]),
            json!({"jsonrpc":"1.0","id":3,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":4,"method":"unknown"}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"unknown"}}),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"click","arguments":{"app":"test.invalid","click_count":true}}}),
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"click","arguments":{"app":"test.invalid","click_count":1e30}}}),
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"drag","arguments":{"app":"test.invalid"}}}),
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"type_text","arguments":{"app":"test.invalid","text":true}}}),
            json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"list_apps"}}),
        ] {
            input.push_str(&value.to_string());
            input.push('\n');
        }
        input.push_str("not-json\n");
        input.push_str(&"x".repeat(1_052_672));
        input.push('\n');
        input.push_str(&json!({"jsonrpc":"2.0","id":10,"method":"ping"}).to_string());
        let mut stdin = child.stdin.take().unwrap();
        let writer = tokio::spawn(async move {
            stdin.write_all(input.as_bytes()).await.unwrap();
        });
        let output =
            tokio::time::timeout(std::time::Duration::from_secs(20), child.wait_with_output())
                .await
                .expect("MCP server hung on invalid input or EOF")
                .unwrap();
        writer.await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let responses: Vec<serde_json::Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("stdout must contain only JSON-RPC"))
            .collect();
        assert_eq!(
            responses.len(),
            13,
            "notifications must not receive replies"
        );
        let reply = |id| responses.iter().find(|reply| reply["id"] == id).unwrap();
        assert_eq!(reply(1)["result"]["serverInfo"]["name"], PLUGIN);
        let tools = reply(2)["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 9);
        assert!(tools.iter().find(|tool| tool["name"] == "click").unwrap()["annotations"]["destructiveHint"].as_bool().unwrap());
        assert_eq!(reply(4)["error"]["code"], -32601);
        assert_eq!(reply(5)["error"]["code"], -32602);
        for id in 6..=9 {
            assert_eq!(reply(id)["result"]["isError"], true);
        }
        assert!(reply(10)["result"].is_object());
        assert_eq!(
            responses
                .iter()
                .filter(|reply| reply["error"]["code"] == -32700)
                .count(),
            2
        );
        let manifest = root
            .join("plugins")
            .join(PLUGIN)
            .join(".codex-plugin/plugin.json");
        fs::remove_file(manifest).unwrap();
        assert!(!is_available(temp.path()));
        assert!(prepare(temp.path()).unwrap());
        assert!(is_available(temp.path()));
    }

    #[test]
    fn install_is_idempotent_preserves_disabled_plugins_and_handles_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("Codex Home");
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "model = 'user-model'\n[plugins.\"codey-computer-use@codey-local\"]\nenabled = false\n",
        )
        .unwrap();
        assert!(install(&home, b"test-native").unwrap());
        let before = fs::read(home.join("config.toml")).unwrap();
        assert!(!install(&home, b"test-native").unwrap());
        assert_eq!(fs::read(home.join("config.toml")).unwrap(), before);
        let doc = ConfigManager::for_home(&home).load().unwrap();
        assert_eq!(doc.document()["model"].as_str(), Some("user-model"));
        assert_eq!(
            doc.document()["plugins"]["codey-computer-use@codey-local"]["enabled"].as_bool(),
            Some(false)
        );
        let root = marketplace_root(&home.canonicalize().unwrap());
        let plugin = root.join("plugins").join(PLUGIN);
        let mcp: serde_json::Value =
            serde_json::from_slice(&fs::read(plugin.join(".mcp.json")).unwrap()).unwrap();
        assert_eq!(
            Path::new(
                mcp["mcpServers"]["codey_computer_use"]["command"]
                    .as_str()
                    .unwrap()
            ),
            executable_path(&root, b"test-native")
        );
        assert!(!mcp.to_string().contains("node"));
        assert!(install(&home, b"updated-native").unwrap());
    }

    #[test]
    fn refuses_custom_marketplace_and_unowned_directory() {
        let temp = tempfile::tempdir().unwrap();
        let config = "[marketplaces.codey-local]\nsource_type='local'\nsource='/custom'\n";
        fs::write(temp.path().join("config.toml"), config).unwrap();
        assert!(install(temp.path(), b"native").is_err());
        assert!(!marketplace_root(temp.path()).exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("config.toml")).unwrap(),
            config
        );
        let other = tempfile::tempdir().unwrap();
        let root = marketplace_root(other.path());
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("user-data"), b"keep").unwrap();
        assert!(install(other.path(), b"native").is_err());
        assert_eq!(fs::read(root.join("user-data")).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_linked_directories_and_files() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), temp.path().join(".tmp")).unwrap();
        assert!(install(temp.path(), b"native").is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
        fs::remove_file(temp.path().join(".tmp")).unwrap();
        install(temp.path(), b"native").unwrap();
        let root = marketplace_root(temp.path());
        let path = root.join("plugins").join(PLUGIN).join(".mcp.json");
        fs::remove_file(&path).unwrap();
        let destination = outside.path().join("keep");
        fs::write(&destination, b"keep").unwrap();
        symlink(&destination, &path).unwrap();
        assert!(install(temp.path(), b"native").is_err());
        assert_eq!(fs::read(destination).unwrap(), b"keep");
    }
}
