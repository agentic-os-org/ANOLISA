//! 消息混合加密模块
//!
//! 使用 RSA-OAEP(SHA-256) + AES-256-GCM 混合加密方案保护敏感消息字段。
//! 每次加密生成随机 AES-256 密钥和 nonce，用公钥加密 AES 密钥，
//! 最终输出 base64 编码的二进制密文。
//!
//! 公钥来源：由调用方从 agentsight.json 的 `encryption.public_key` 读取后传入。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use openssl::pkey::Public;
use openssl::rand::rand_bytes;
use openssl::rsa::{Padding, Rsa};
use openssl::symm::{Cipher, encrypt_aead};

/// AES-256 密钥长度（32 字节）
const AES_KEY_LEN: usize = 32;

/// AES-GCM nonce 长度（12 字节）
const NONCE_LEN: usize = 12;

/// AES-GCM 认证标签长度（16 字节）
const TAG_LEN: usize = 16;

/// 消息加密器，持有解析后的 RSA 公钥
pub struct MessageEncryptor {
    rsa: Rsa<Public>,
}

impl MessageEncryptor {
    /// 从指定 PEM 字符串创建加密器
    ///
    /// 解析失败时记录警告并返回 None（回退到明文模式）。
    /// PEM 来源由调用方决定（通常来自 agentsight.json 的 encryption.public_key）。
    pub fn from_pem(pem: &str) -> Option<Self> {
        match Rsa::public_key_from_pem(pem.as_bytes()) {
            Ok(rsa) => {
                log::info!(
                    "MessageEncryptor initialized (RSA-{} + AES-256-GCM)",
                    rsa.size() * 8
                );
                Some(MessageEncryptor { rsa })
            }
            Err(e) => {
                log::warn!("Failed to parse RSA public key, encryption disabled: {e}");
                None
            }
        }
    }

    /// 执行混合加密
    ///
    /// 输出格式（base64 编码）：
    /// `[2字节 encrypted_key 长度(big-endian)] [encrypted_key] [12字节 nonce] [ciphertext + 16字节 tag]`
    pub fn encrypt(&self, plaintext: &str) -> Result<String, String> {
        // 1. 生成随机 AES-256 密钥
        let mut aes_key = vec![0u8; AES_KEY_LEN];
        rand_bytes(&mut aes_key).map_err(|e| format!("rand_bytes for AES key failed: {e}"))?;

        // 2. 生成随机 12 字节 nonce
        let mut nonce = vec![0u8; NONCE_LEN];
        rand_bytes(&mut nonce).map_err(|e| format!("rand_bytes for nonce failed: {e}"))?;

        // 3. AES-256-GCM 加密明文
        let mut tag = vec![0u8; TAG_LEN];
        let ciphertext = encrypt_aead(
            Cipher::aes_256_gcm(),
            &aes_key,
            Some(&nonce),
            &[], // AAD (Additional Authenticated Data) - 不使用
            plaintext.as_bytes(),
            &mut tag,
        )
        .map_err(|e| format!("AES-256-GCM encryption failed: {e}"))?;

        // 4. RSA-OAEP(SHA-256) 加密 AES 密钥
        let mut encrypted_key = vec![0u8; self.rsa.size() as usize];
        let encrypted_key_len = self
            .rsa
            .public_encrypt(&aes_key, &mut encrypted_key, Padding::PKCS1_OAEP)
            .map_err(|e| format!("RSA-OAEP encryption failed: {e}"))?;
        encrypted_key.truncate(encrypted_key_len);

        // 5. 组装二进制输出：[2字节长度] [encrypted_key] [nonce] [ciphertext] [tag]
        let key_len_bytes = (encrypted_key_len as u16).to_be_bytes();
        let mut output =
            Vec::with_capacity(2 + encrypted_key_len + NONCE_LEN + ciphertext.len() + TAG_LEN);
        output.extend_from_slice(&key_len_bytes);
        output.extend_from_slice(&encrypted_key);
        output.extend_from_slice(&nonce);
        output.extend_from_slice(&ciphertext);
        output.extend_from_slice(&tag);

        // 6. Base64 编码
        Ok(BASE64.encode(&output))
    }

    /// 辅助方法：有加密器则加密，加密失败或无加密器时返回原文
    pub fn maybe_encrypt(encryptor: Option<&Self>, text: &str) -> String {
        match encryptor {
            Some(enc) => match enc.encrypt(text) {
                Ok(encrypted) => encrypted,
                Err(e) => {
                    log::warn!("Encryption failed, falling back to plaintext: {e}");
                    text.to_string()
                }
            },
            None => text.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_pem_invalid_returns_none() {
        // 非法 PEM 应该返回 None（不崩溃）
        let enc = MessageEncryptor::from_pem("not a valid pem");
        assert!(enc.is_none());
    }

    #[test]
    fn test_maybe_encrypt_without_encryptor() {
        let text = "plain text content";
        let result = MessageEncryptor::maybe_encrypt(None, text);
        assert_eq!(result, text);
    }
}
