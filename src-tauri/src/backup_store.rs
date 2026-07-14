//! 文件职责：在覆盖受管 JSON 数据文件前保存当前有效版本，并限制本地备份数量。
//! 主要内容：按仓库路径指纹和数据文件名隔离备份，写入后刷新磁盘并保留最近十份。
//! 重要约束：备份位于机器配置目录，不进入用户 Git 仓库；备份失败时停止正式写入。

use crate::error::AppError;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// 每个数据仓库最多保留的成功写入前备份数量。
const RETAINED_BACKUPS: usize = 10;
/// 同一毫秒发生多次保存时用于保证文件名唯一的进程内序号。
static BACKUP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// 计算不泄露完整路径且跨重启稳定的仓库目录指纹。
fn repository_fingerprint(repository_root: &Path) -> String {
    let normalized = repository_root.to_string_lossy().to_lowercase();
    let digest = format!("{:x}", Sha256::digest(normalized.as_bytes()));
    digest[..16].to_string()
}

/// 构造当前备份文件路径；系统时间异常时返回结构化错误而不是覆盖旧备份。
fn next_backup_path(directory: &Path, file_stem: &str) -> Result<PathBuf, AppError> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            AppError::new(
                "BACKUP_FAILED",
                format!("系统时间无法用于创建备份：{error}"),
                "校准系统时间后重试保存。",
                true,
            )
        })?
        .as_millis();
    let sequence = BACKUP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(directory.join(format!("{file_stem}.{milliseconds}.{sequence}.json")))
}

/// 删除指定数据文件超过保留数量的最旧备份，不影响同仓库内其他数据文件的备份。
fn rotate_backups(directory: &Path, file_stem: &str) -> Result<(), AppError> {
    let mut backups: Vec<PathBuf> = fs::read_dir(directory)
        .map_err(|error| {
            AppError::new(
                "BACKUP_FAILED",
                format!("无法检查本地备份目录：{error}"),
                "检查 APPDATA 目录权限后重试。",
                true,
            )
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| {
                    name.starts_with(&format!("{file_stem}.")) && name.ends_with(".json")
                })
        })
        .collect();
    backups.sort();

    let remove_count = backups.len().saturating_sub(RETAINED_BACKUPS);
    for path in backups.into_iter().take(remove_count) {
        fs::remove_file(&path).map_err(|error| {
            AppError::new(
                "BACKUP_FAILED",
                format!("无法清理旧备份 {}：{error}", path.display()),
                "检查备份目录是否被其他程序占用后重试。",
                true,
            )
        })?;
    }
    Ok(())
}

/// 为一个已存在的受管 JSON 文件创建仓库外备份，并按文件类型独立轮换。
///
/// 参数：`file_stem` 只接受代码内固定的安全名称，用于区分命令与临时收集备份。
/// 返回值：新备份的绝对路径，主要供测试和后续恢复界面使用。
/// 副作用：可能创建仓库指纹目录并删除该文件类型超过十份的最旧备份。
fn backup_managed_file(
    config_directory: &Path,
    repository_root: &Path,
    document_path: &Path,
    file_stem: &str,
) -> Result<PathBuf, AppError> {
    let directory = config_directory
        .join("backups")
        .join(repository_fingerprint(repository_root));
    fs::create_dir_all(&directory).map_err(|error| {
        AppError::new(
            "BACKUP_FAILED",
            format!("无法创建本地备份目录：{error}"),
            "检查 APPDATA 目录权限后重试保存。",
            true,
        )
    })?;
    let backup_path = next_backup_path(&directory, file_stem)?;
    fs::copy(document_path, &backup_path).map_err(|error| {
        AppError::new(
            "BACKUP_FAILED",
            format!("无法备份当前 {file_stem}.json：{error}"),
            "确认数据文件可读且磁盘空间充足后重试。",
            true,
        )
    })?;
    // Windows 对只读句柄执行 FlushFileBuffers 会拒绝访问，因此显式申请写权限后刷新副本。
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&backup_path)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            AppError::new(
                "BACKUP_FAILED",
                format!("无法把本地备份刷新到磁盘：{error}"),
                "检查磁盘状态后重试。",
                true,
            )
        })?;
    rotate_backups(&directory, file_stem)?;
    Ok(backup_path)
}

/// 在覆盖数据文件前创建并刷新一份应用外备份。
///
/// 返回值：新备份的绝对路径，主要供测试和后续恢复界面使用。
/// 副作用：可能创建仓库指纹目录并删除超过十份的最旧备份。
pub fn backup_document(
    config_directory: &Path,
    repository_root: &Path,
    document_path: &Path,
) -> Result<PathBuf, AppError> {
    backup_managed_file(config_directory, repository_root, document_path, "commands")
}

/// 在覆盖 `inbox.json` 前创建并刷新一份应用外备份。
///
/// 返回值：新备份的绝对路径，文件名以 `inbox.` 开头且不占用命令备份保留数。
/// 副作用：可能创建仓库指纹目录并删除超过十份的最旧临时收集备份。
pub fn backup_inbox_document(
    config_directory: &Path,
    repository_root: &Path,
    document_path: &Path,
) -> Result<PathBuf, AppError> {
    backup_managed_file(config_directory, repository_root, document_path, "inbox")
}

#[cfg(test)]
mod tests {
    //! 测试职责：确认备份与数据仓库隔离，并严格保留最近十份。

    use super::{backup_document, backup_inbox_document};
    use std::fs;

    /// 验证连续十一份备份会清理最旧一份，且每份内容来自写入前文件。
    #[test]
    fn retains_only_ten_repository_scoped_backups() {
        let directory = tempfile::tempdir().expect("应能创建测试目录");
        let config = directory.path().join("config");
        let repository = directory.path().join("repository");
        fs::create_dir_all(&repository).expect("应能创建仓库目录");
        let document = repository.join("commands.json");

        for index in 0..11 {
            fs::write(&document, format!("version-{index}")).expect("应能写入测试文档");
            backup_document(&config, &repository, &document).expect("备份应成功");
        }

        let fingerprint_directories: Vec<_> = fs::read_dir(config.join("backups"))
            .expect("应能读取备份根目录")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(fingerprint_directories.len(), 1);
        let backups: Vec<_> = fs::read_dir(fingerprint_directories[0].path())
            .expect("应能读取仓库备份目录")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(backups.len(), 10);
        assert!(
            !repository.join("backups").exists(),
            "备份不得进入用户 Git 仓库"
        );
    }

    /// 验证命令和临时收集备份使用独立前缀与保留计数，且都位于仓库之外。
    #[test]
    fn separates_command_and_inbox_backups() {
        let directory = tempfile::tempdir().expect("应能创建测试目录");
        let config = directory.path().join("config");
        let repository = directory.path().join("repository");
        fs::create_dir_all(&repository).expect("应能创建仓库目录");
        let commands = repository.join("commands.json");
        let inbox = repository.join("inbox.json");
        fs::write(&commands, "commands").expect("应能写入命令测试文档");
        fs::write(&inbox, "inbox").expect("应能写入临时收集测试文档");

        let command_backup =
            backup_document(&config, &repository, &commands).expect("命令备份应成功");
        let inbox_backup =
            backup_inbox_document(&config, &repository, &inbox).expect("临时收集备份应成功");

        assert!(command_backup
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with("commands.")));
        assert!(inbox_backup
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with("inbox.")));
        assert_eq!(
            fs::read_to_string(inbox_backup).expect("应能读取备份"),
            "inbox"
        );
        assert!(!repository.join("backups").exists());
    }
}
