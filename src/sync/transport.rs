//! 传输抽象：把「读一个文件 / 写一个文件」从「WebDAV 还是 S3」里解耦。
//!
//! 协议层（[`super::engine`] 的上传 / 拉取 / 测试连接）只认识 [`Transport`] 的
//! 四个方法，加第三种存储只需再实现一个分支，不必改动同步逻辑。与 Agent2API
//! 把 `sync_protocol` 与 `webdav` / `s3` 分层的理由一致。
//!
//! ── 远端目录布局 ────────────────────────────────────────────
//! ```text
//! WebDAV: {url}/{path}/manifest.json
//!         {url}/{path}/snapshot.json
//! S3:     {bucket}/{prefix}/{path}/manifest.json
//!         {bucket}/{prefix}/{path}/snapshot.json
//! ```
//! 先传 snapshot、最后传 manifest：manifest 是「这次上传已完成」的唯一标志，
//! 中途断网只会让远端仍指向上一份完整快照。

use super::config::{path_segments, SyncConfig, PROVIDER_S3};
use super::snapshot::{REMOTE_MANIFEST, REMOTE_SNAPSHOT};
use super::{s3, webdav};

/// 一次同步要用到的传输实现。
pub(crate) enum Transport {
    WebDav {
        base_url: String,
        auth: webdav::WebDavAuth,
        /// 远端目录段（`path`），每次读写都要拼。
        segments: Vec<String>,
        user_agent: String,
    },
    S3 {
        credentials: s3::S3Credentials,
        /// 对象键前缀：`prefix/path`。
        prefix: String,
    },
}

impl Transport {
    /// 从设置构造（密码在这里解出明文，只在本次操作的内存里存活）。
    pub(crate) fn from_config(config: &SyncConfig) -> Result<Self, String> {
        let root_segments = path_segments(&config.path)?;
        match config.backend.as_str() {
            PROVIDER_S3 => {
                let mut segments = path_segments(&config.s3.prefix)?;
                segments.extend(root_segments);
                Ok(Transport::S3 {
                    credentials: s3::S3Credentials {
                        access_key: config.s3.access_key.clone(),
                        secret_key: config.s3_secret_plain()?,
                        region: config.s3.region.clone(),
                        bucket: config.s3.bucket.clone(),
                        endpoint: config.s3.endpoint.clone(),
                        user_agent: config.user_agent.clone(),
                    },
                    prefix: segments.join("/"),
                })
            }
            _ => Ok(Transport::WebDav {
                base_url: config.webdav.url.clone(),
                auth: webdav::auth_from_credentials(
                    &config.webdav.username,
                    &config.webdav_password_plain()?,
                ),
                segments: root_segments,
                user_agent: config.user_agent.clone(),
            }),
        }
    }

    /// 远端目录的展示路径。
    pub(crate) fn display_path(&self) -> String {
        match self {
            Transport::WebDav { segments, .. } => format!("/{}", segments.join("/")),
            Transport::S3 {
                credentials,
                prefix,
                ..
            } => {
                if prefix.is_empty() {
                    credentials.bucket.clone()
                } else {
                    format!("{}/{prefix}", credentials.bucket)
                }
            }
        }
    }

    /// 测试连接（WebDAV 顺带建目录，避免第一次上传才发现没权限）。
    pub(crate) async fn test_connection(&self) -> Result<(), String> {
        match self {
            Transport::WebDav {
                base_url,
                auth,
                segments,
                user_agent,
            } => {
                webdav::test_connection(base_url, auth, user_agent).await?;
                webdav::ensure_directories(base_url, segments, auth, user_agent).await
            }
            Transport::S3 { credentials, .. } => s3::test_connection(credentials).await,
        }
    }

    /// 写一个文件。
    pub(crate) async fn put(
        &self,
        name: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<(), String> {
        match self {
            Transport::WebDav {
                base_url,
                auth,
                segments,
                user_agent,
            } => {
                let mut file_segments = segments.clone();
                file_segments.extend(path_segments(name)?);
                let url = webdav::build_remote_url(base_url, &file_segments, false)?;
                webdav::put_bytes(&url, auth, bytes, content_type, user_agent).await
            }
            Transport::S3 {
                credentials,
                prefix,
            } => {
                let key = if prefix.is_empty() {
                    name.trim_start_matches('/').to_string()
                } else {
                    format!("{prefix}/{}", name.trim_start_matches('/'))
                };
                s3::put_object(credentials, &key, bytes, content_type).await
            }
        }
    }

    /// 读一个文件；不存在返回 `None`（「远端还没有」是正常状态）。
    pub(crate) async fn get(
        &self,
        name: &str,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, String> {
        match self {
            Transport::WebDav {
                base_url,
                auth,
                segments,
                user_agent,
            } => {
                let mut file_segments = segments.clone();
                file_segments.extend(path_segments(name)?);
                let url = webdav::build_remote_url(base_url, &file_segments, false)?;
                webdav::get_bytes(&url, auth, max_bytes, user_agent).await
            }
            Transport::S3 {
                credentials,
                prefix,
            } => {
                let key = if prefix.is_empty() {
                    name.trim_start_matches('/').to_string()
                } else {
                    format!("{prefix}/{}", name.trim_start_matches('/'))
                };
                s3::get_object(credentials, &key, max_bytes).await
            }
        }
    }

    /// 读远端 manifest（带体积上限）；不存在返回 `None`。
    pub(crate) async fn get_manifest(
        &self,
    ) -> Result<Option<(Vec<u8>, super::snapshot::SyncManifest)>, String> {
        let Some(bytes) = self
            .get(REMOTE_MANIFEST, super::snapshot::MAX_MANIFEST_BYTES)
            .await?
        else {
            return Ok(None);
        };
        let parsed: super::snapshot::SyncManifest =
            serde_json::from_slice(&bytes).map_err(|error| {
                format!("远端 manifest 不是合法 JSON（可能不是本功能生成的文件）: {error}")
            })?;
        Ok(Some((bytes, parsed)))
    }

    /// 读远端快照并按 manifest 校验（字节数 + sha256）。
    pub(crate) async fn get_snapshot(
        &self,
        manifest: &super::snapshot::SyncManifest,
    ) -> Result<super::snapshot::SyncSnapshot, String> {
        if manifest.snapshot_bytes > super::snapshot::MAX_SNAPSHOT_BYTES as u64 {
            return Err(format!(
                "远端快照超过上限（{} 字节 > {} 字节）",
                manifest.snapshot_bytes,
                super::snapshot::MAX_SNAPSHOT_BYTES
            ));
        }
        let bytes = self
            .get(REMOTE_SNAPSHOT, super::snapshot::MAX_SNAPSHOT_BYTES)
            .await?
            .ok_or_else(|| "远端缺少 snapshot.json（manifest 说有、实际读不到）".to_string())?;
        manifest.verify(&bytes)?;
        let snapshot: super::snapshot::SyncSnapshot = serde_json::from_slice(&bytes)
            .map_err(|error| format!("远端 snapshot.json 解析失败: {error}"))?;
        snapshot.validate()?;
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::config::{S3Config, WebDavConfig};

    #[test]
    fn display_path_combines_prefix_webdav() {
        let config = SyncConfig {
            path: "aigx-sync/default".to_string(),
            ..SyncConfig::default()
        };
        let transport = Transport::from_config(&config).unwrap();
        assert_eq!(transport.display_path(), "/aigx-sync/default");
    }

    #[test]
    fn display_path_combines_prefix_s3() {
        let config = SyncConfig {
            backend: PROVIDER_S3.to_string(),
            path: "aigx-sync".to_string(),
            s3: S3Config {
                bucket: "bucket".to_string(),
                prefix: "backups".to_string(),
                ..S3Config::default()
            },
            ..SyncConfig::default()
        };
        let transport = Transport::from_config(&config).unwrap();
        assert_eq!(transport.display_path(), "bucket/backups/aigx-sync");
    }

    #[test]
    fn webdav_transport_carries_custom_user_agent() {
        let config = SyncConfig {
            user_agent: "rclone/v1.67.0".to_string(),
            webdav: WebDavConfig {
                url: "https://dav.example.com/dav".to_string(),
                username: "u".to_string(),
                // 空密码 = 匿名凭据形态（None）不成立，但解出空串是允许的
                password: String::new(),
            },
            ..SyncConfig::default()
        };
        match Transport::from_config(&config).unwrap() {
            Transport::WebDav { user_agent, .. } => assert_eq!(user_agent, "rclone/v1.67.0"),
            _ => panic!("应构造 WebDAV 传输"),
        }
    }
}
