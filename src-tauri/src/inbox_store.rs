//! 文件职责：读取、校验、初始化和指纹化仓库中的 `inbox.json`。
//! 主要内容：实现临时收集文档 `schemaVersion: 1` 的业务规则和稳定错误分类。
//! 重要约束：任何无效文件都不得被空文档覆盖；第一版文件上限为 10 MB。

use crate::error::AppError;
use crate::model::InboxDocument;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// 第一版允许加载或初始化的最大文件字节数，避免异常数据阻塞桌面应用。
const MAX_INBOX_BYTES: u64 = 10 * 1024 * 1024;

/// 读取并完整校验临时收集文档，同时返回原始字节的 SHA-256。
pub fn load_inbox_document(path: &Path) -> Result<(InboxDocument, String), AppError> {
    let metadata = fs::metadata(path).map_err(|error| {
        AppError::new(
            "INBOX_NOT_FOUND",
            format!("无法读取 inbox.json：{error}"),
            "确认临时收集文件仍在仓库根目录后重试。",
            true,
        )
    })?;
    if metadata.len() > MAX_INBOX_BYTES {
        return Err(too_large_error("inbox.json 超过第一版 10 MB 的加载上限。"));
    }

    let bytes = fs::read(path).map_err(|error| {
        AppError::new(
            "INBOX_READ_FAILED",
            format!("无法读取 inbox.json：{error}"),
            "检查文件权限和磁盘状态后重试。",
            true,
        )
    })?;
    parse_inbox_document_bytes(&bytes)
}

/// 从内存字节解析并完整校验临时收集文档，供磁盘读取和后续远端候选预检复用。
pub fn parse_inbox_document_bytes(bytes: &[u8]) -> Result<(InboxDocument, String), AppError> {
    if bytes.len() as u64 > MAX_INBOX_BYTES {
        return Err(too_large_error("inbox.json 超过第一版 10 MB 的加载上限。"));
    }
    std::str::from_utf8(bytes).map_err(|_| {
        AppError::new(
            "INBOX_INVALID",
            "inbox.json 不是有效 UTF-8 文本。",
            "用 UTF-8 编码修复文件后重新加载；应用不会覆盖原文件。",
            false,
        )
    })?;

    let document: InboxDocument = serde_json::from_slice(bytes).map_err(|error| {
        AppError::new(
            "INBOX_INVALID",
            format!("inbox.json 的 JSON 结构无效：{error}"),
            "修复文件格式后重新加载；应用不会覆盖原文件。",
            false,
        )
    })?;
    validate_inbox_document(&document)?;
    let hash = format!("{:x}", Sha256::digest(bytes));
    Ok((document, hash))
}

/// 校验第一版临时收集文档的版本、稳定 ID、内容和时间必填规则。
pub fn validate_inbox_document(document: &InboxDocument) -> Result<(), AppError> {
    if document.schema_version != 1 {
        return Err(AppError::new(
            "INBOX_UNSUPPORTED_SCHEMA",
            format!(
                "不支持 inbox.json 的 schemaVersion {}。",
                document.schema_version
            ),
            "使用支持 schemaVersion 1 的临时收集文件。",
            false,
        ));
    }

    let mut item_ids = HashSet::new();
    for item in &document.items {
        if item.id.trim().is_empty() {
            return Err(invalid_inbox("临时记录 ID 不能为空。"));
        }
        if !item_ids.insert(item.id.as_str()) {
            return Err(invalid_inbox("临时记录 ID 必须在文档内唯一。"));
        }
        if item.content.trim().is_empty() {
            return Err(invalid_inbox("临时记录内容不能为空。"));
        }
        if item.created_at.trim().is_empty() || item.updated_at.trim().is_empty() {
            return Err(invalid_inbox("临时记录的创建时间和更新时间不能为空。"));
        }
    }
    Ok(())
}

/// 校验并序列化临时收集文档，统一输出两空格缩进、LF 和结尾换行。
pub fn serialize_inbox_document(document: &InboxDocument) -> Result<Vec<u8>, AppError> {
    validate_inbox_document(document)?;
    let mut bytes = serde_json::to_vec_pretty(document).map_err(|error| {
        AppError::new(
            "INBOX_SERIALIZE_FAILED",
            format!("无法生成 inbox.json：{error}"),
            "保留当前内容并重试。",
            true,
        )
    })?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_INBOX_BYTES {
        return Err(too_large_error(
            "生成的 inbox.json 将超过第一版 10 MB 上限。",
        ));
    }
    Ok(bytes)
}

/// 仅在目标不存在时创建第一版空文档，使用 `create_new` 防止竞态覆盖已有内容。
///
/// 副作用：成功时创建并刷新 `inbox.json`；写入失败时尽力移除未完成的新文件。
pub fn initialize_empty_inbox_document(path: &Path) -> Result<(), AppError> {
    let bytes = serialize_inbox_document(&InboxDocument::empty())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                AppError::new(
                    "INBOX_ALREADY_EXISTS",
                    "inbox.json 已经存在，初始化操作已停止。",
                    "重新加载现有临时收集文件。",
                    false,
                )
            } else {
                AppError::new(
                    "INBOX_WRITE_FAILED",
                    format!("无法创建 inbox.json：{error}"),
                    "检查仓库目录权限和磁盘空间后重试。",
                    true,
                )
            }
        })?;

    // 新文件对其他进程可见后必须完整写入并刷新；失败时删除半成品，避免下次把它当作有效文档。
    let write_result = file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            AppError::new(
                "INBOX_WRITE_FAILED",
                format!("无法完整初始化 inbox.json：{error}"),
                "检查磁盘状态后重试。",
                true,
            )
        });
    drop(file);
    if write_result.is_err() {
        let _ = fs::remove_file(path);
    }
    write_result
}

/// 创建统一的临时收集校验错误，明确提醒调用方保留原文件。
fn invalid_inbox(message: impl Into<String>) -> AppError {
    AppError::new(
        "INBOX_INVALID",
        message,
        "修复 inbox.json 后重新加载；应用不会覆盖原文件。",
        false,
    )
}

/// 创建统一的 10 MB 上限错误，加载和序列化使用同一稳定错误码。
fn too_large_error(message: impl Into<String>) -> AppError {
    AppError::new(
        "INBOX_TOO_LARGE",
        message,
        "精简临时记录内容后重试。",
        false,
    )
}

#[cfg(test)]
mod tests {
    //! 测试职责：锁定临时收集文档的初始化、校验、序列化和大小边界。

    use super::{
        initialize_empty_inbox_document, load_inbox_document, parse_inbox_document_bytes,
        serialize_inbox_document, validate_inbox_document, MAX_INBOX_BYTES,
    };
    use crate::model::{InboxDocument, InboxEntry};
    use std::fs;

    /// 构造包含一条文字与链接混合内容的最小合法文档。
    fn valid_document() -> InboxDocument {
        InboxDocument {
            schema_version: 1,
            items: vec![InboxEntry {
                id: "inbox-1".to_string(),
                content: "稍后查看\nhttps://example.com".to_string(),
                created_at: "2026-07-14T06:32:00.000Z".to_string(),
                updated_at: "2026-07-14T06:32:00.000Z".to_string(),
            }],
        }
    }

    /// 验证首次初始化可读回空文档和稳定 SHA-256，且再次初始化不会覆盖已有字节。
    #[test]
    fn initializes_once_without_replacing_existing_document() {
        let directory = tempfile::tempdir().expect("应能创建测试目录");
        let path = directory.path().join("inbox.json");

        initialize_empty_inbox_document(&path).expect("首次初始化应成功");
        let original_bytes = fs::read(&path).expect("应能读取初始化文件");
        let (document, hash) = load_inbox_document(&path).expect("初始化文档应可加载");

        assert_eq!(document, InboxDocument::empty());
        assert_eq!(hash.len(), 64, "SHA-256 应使用 64 位十六进制文本");
        let error = initialize_empty_inbox_document(&path).expect_err("已有文件不得被初始化覆盖");
        assert_eq!(error.code, "INBOX_ALREADY_EXISTS");
        assert_eq!(fs::read(&path).expect("已有文档应保留"), original_bytes);
    }

    /// 验证未知版本使用专用稳定错误码，而不是被当作空文档。
    #[test]
    fn rejects_unsupported_schema_version() {
        let mut document = valid_document();
        document.schema_version = 2;

        let error = validate_inbox_document(&document).expect_err("未来版本应被拒绝");
        assert_eq!(error.code, "INBOX_UNSUPPORTED_SCHEMA");
    }

    /// 验证重复 ID、空白内容和空时间字段分别违反第一版业务规则。
    #[test]
    fn rejects_duplicate_ids_and_blank_required_fields() {
        let mut duplicate = valid_document();
        duplicate.items.push(duplicate.items[0].clone());
        assert_eq!(
            validate_inbox_document(&duplicate)
                .expect_err("重复 ID 应被拒绝")
                .code,
            "INBOX_INVALID"
        );

        let mut blank_content = valid_document();
        blank_content.items[0].content = " \n ".to_string();
        assert_eq!(
            validate_inbox_document(&blank_content)
                .expect_err("空白内容应被拒绝")
                .code,
            "INBOX_INVALID"
        );

        let mut blank_time = valid_document();
        blank_time.items[0].updated_at.clear();
        assert_eq!(
            validate_inbox_document(&blank_time)
                .expect_err("空更新时间应被拒绝")
                .code,
            "INBOX_INVALID"
        );
    }

    /// 验证序列化字段名、结尾换行和数组顺序稳定，并可由同一解析器完整读回。
    #[test]
    fn serializes_stable_format_and_round_trips() {
        let document = valid_document();
        let bytes = serialize_inbox_document(&document).expect("合法文档应可序列化");
        let text = std::str::from_utf8(&bytes).expect("序列化结果应为 UTF-8");

        assert!(text.contains("\"schemaVersion\": 1"));
        assert!(text.contains("\"createdAt\""));
        assert!(text.ends_with('\n'));
        assert!(!text.contains("\r\n"));
        let (reloaded, _) = parse_inbox_document_bytes(&bytes).expect("序列化结果应可读回");
        assert_eq!(reloaded, document);
    }

    /// 验证超过 10 MB 的输入在 JSON 解析前即被拒绝，避免异常内存和解析开销。
    #[test]
    fn rejects_document_over_size_limit() {
        let bytes = vec![b' '; MAX_INBOX_BYTES as usize + 1];

        let error = parse_inbox_document_bytes(&bytes).expect_err("超限文件应被拒绝");
        assert_eq!(error.code, "INBOX_TOO_LARGE");
    }
}
