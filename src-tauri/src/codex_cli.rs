//! 文件职责：识别当前电脑上可由 CommandShelf 调用的 Codex CLI。
//! 主要内容：在系统命令行受控执行 `codex --version`，并返回前端可展示状态。
//! 重要约束：探测过程不接受用户参数、不调用模型，也不向前端泄露本机安装路径或底层错误。

use crate::process_runner::run_process;
use serde::Serialize;
use std::env;
use std::path::Path;
use std::time::Duration;

/// Codex CLI 版本探测的最长等待时间；版本查询不应触发网络访问或长时间初始化。
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
/// 版本输出的保留上限；异常大输出会被视为不可用，避免占用桌面进程内存。
const VERSION_OUTPUT_LIMIT: usize = 8 * 1024;

/// 前端可直接展示的 Codex CLI 可用状态。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexCliStatus {
    /// 当前 PATH 中的 Codex CLI 是否能成功完成版本查询。
    pub(crate) available: bool,
    /// CLI 返回的完整版本行；未安装、启动失败或输出异常时为空。
    pub(crate) version: Option<String>,
    /// 不含本机路径和底层错误细节的中文状态说明。
    pub(crate) status_message: String,
}

impl CodexCliStatus {
    /// 创建可用状态；版本文本已经过非空和输出大小校验。
    fn available(version: String) -> Self {
        Self {
            available: true,
            version: Some(version),
            status_message: "已检测到可用的 Codex CLI。".to_string(),
        }
    }

    /// 创建不可用状态；不回传退出码或进程错误，避免泄露本机细节。
    fn unavailable() -> Self {
        Self {
            available: false,
            version: None,
            status_message:
                "无法使用 Codex CLI，请先安装或在系统终端运行 codex --version 检查配置。"
                    .to_string(),
        }
    }
}

/// 直接通过系统命令行识别 Codex CLI，并查询版本。
///
/// 返回值：始终返回可序列化状态；未安装或探测失败属于可展示状态，不抛出应用错误。
/// 副作用：最多启动一次本机 Codex CLI 的 `--version` 子进程，不访问命令数据仓库。
pub(crate) fn detect_codex_cli() -> CodexCliStatus {
    let current_directory = env::current_dir().unwrap_or_else(|_| env::temp_dir());
    detect_codex_cli_with_environment(&current_directory, &[])
}

/// 使用指定环境执行固定版本命令；测试入口避免修改全局 PATH 造成并行测试竞态。
fn detect_codex_cli_with_environment(
    current_directory: &Path,
    environment: &[(&str, &str)],
) -> CodexCliStatus {
    let output = match run_codex_version(current_directory, environment) {
        Ok(output) => output,
        Err(_) => return CodexCliStatus::unavailable(),
    };

    if !output.status.success() || output.stdout_truncated || output.stderr_truncated {
        return CodexCliStatus::unavailable();
    }

    // Codex 当前把版本写入 stdout，同时兼容部分启动器把版本转发到 stderr 的情况。
    first_non_empty_line(&output.stdout)
        .or_else(|| first_non_empty_line(&output.stderr))
        .map(CodexCliStatus::available)
        .unwrap_or_else(CodexCliStatus::unavailable)
}

/// Windows 通过系统命令解释器执行固定命令，以兼容 npm 安装产生的 `codex.cmd`。
///
/// 安全边界：命令文本完全由程序固定，不拼接路径、用户问题或其他外部输入。
#[cfg(windows)]
fn run_codex_version(
    current_directory: &Path,
    environment: &[(&str, &str)],
) -> Result<crate::process_runner::ProcessOutput, crate::process_runner::ProcessFailure> {
    run_process(
        current_directory,
        "cmd.exe",
        &["/D", "/S", "/C", "codex --version"],
        environment,
        VERSION_TIMEOUT,
        VERSION_OUTPUT_LIMIT,
        VERSION_OUTPUT_LIMIT,
    )
}

/// 非 Windows 开发机构建直接执行 PATH 中的 `codex`，保持相同返回契约。
#[cfg(not(windows))]
fn run_codex_version(
    current_directory: &Path,
    environment: &[(&str, &str)],
) -> Result<crate::process_runner::ProcessOutput, crate::process_runner::ProcessFailure> {
    run_process(
        current_directory,
        "codex",
        &["--version"],
        environment,
        VERSION_TIMEOUT,
        VERSION_OUTPUT_LIMIT,
        VERSION_OUTPUT_LIMIT,
    )
}

/// 从有限输出中取得第一条非空版本行，并去除行首尾空白。
fn first_non_empty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    //! 测试职责：验证直接命令行探测的成功、缺失和失败状态均保持稳定契约。

    use super::detect_codex_cli_with_environment;
    use std::fs;
    use tempfile::tempdir;

    /// 验证命令行找不到 Codex 时返回不可用状态，而不是底层进程错误。
    #[test]
    #[cfg(windows)]
    fn reports_unavailable_when_command_line_cannot_find_codex() {
        let directory = tempdir().expect("应能创建临时 PATH 目录");
        let search_path = directory.path().to_str().expect("测试路径应为 Unicode");

        let status = detect_codex_cli_with_environment(directory.path(), &[("PATH", search_path)]);

        assert!(!status.available);
        assert_eq!(status.version, None);
        assert!(status.status_message.contains("无法使用"));
    }

    /// 验证成功输出只保留第一条非空版本行，防止额外诊断文本进入界面。
    #[test]
    #[cfg(windows)]
    fn detects_windows_command_script_and_reads_version() {
        let workspace = tempdir().expect("应能创建临时工作目录");
        let bin_directory = workspace.path().join("模拟命令目录");
        fs::create_dir(&bin_directory).expect("应能创建模拟 PATH 目录");
        let launcher = bin_directory.join("codex.cmd");
        fs::write(
            &launcher,
            b"@echo off\r\necho.\r\necho codex-cli 9.9.9\r\necho ignored\r\n",
        )
        .expect("应能创建模拟 Codex 启动器");
        let search_path = bin_directory.to_str().expect("测试路径应为 Unicode");

        let status = detect_codex_cli_with_environment(workspace.path(), &[("PATH", search_path)]);

        assert!(status.available);
        assert_eq!(status.version.as_deref(), Some("codex-cli 9.9.9"));
    }

    /// 验证启动器非零退出时只报告不可用，不把 stderr 或退出码泄露给前端。
    #[test]
    #[cfg(windows)]
    fn hides_process_details_when_version_command_fails() {
        let workspace = tempdir().expect("应能创建临时工作目录");
        let bin_directory = workspace.path().join("bin");
        fs::create_dir(&bin_directory).expect("应能创建模拟 PATH 目录");
        let launcher = bin_directory.join("codex.cmd");
        fs::write(
            &launcher,
            b"@echo off\r\necho private diagnostic 1>&2\r\nexit /b 7\r\n",
        )
        .expect("应能创建失败启动器");
        let search_path = bin_directory.to_str().expect("测试路径应为 Unicode");

        let status = detect_codex_cli_with_environment(workspace.path(), &[("PATH", search_path)]);

        assert!(!status.available);
        assert_eq!(status.version, None);
        assert!(!status.status_message.contains("private diagnostic"));
        assert!(!status.status_message.contains('7'));
    }
}
