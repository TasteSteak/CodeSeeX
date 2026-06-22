use anyhow::{Context, Result};
use codeseex_core::AppConfig;

pub(crate) fn vision_api_key(config: &AppConfig) -> Option<String> {
    secret_store_read(&vision_secret_target(config))
        .ok()
        .flatten()
}

pub(crate) fn vision_api_key_configured(config: &AppConfig) -> bool {
    vision_api_key(config).is_some()
}

pub(crate) fn write_vision_api_key(config: &AppConfig, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(());
    }
    secret_store_write(&vision_secret_target(config), value)
}

pub(crate) fn clear_vision_api_key(config: &AppConfig) -> Result<()> {
    secret_store_delete(&vision_secret_target(config))
}

fn vision_secret_target(config: &AppConfig) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(config.data_dir.to_string_lossy().as_bytes());
    let hash = hasher.finalize();
    format!("CodeSeeX/vision_api_key/{hash:x}")
}

#[cfg(windows)]
fn secret_store_read(target: &str) -> Result<Option<String>> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
    use windows_sys::Win32::Security::Credentials::{
        CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
    };

    let target = wide_null(target);
    let mut credential: *mut CREDENTIALW = null_mut();
    let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(None);
        }
        return Err(error)
            .context("failed to read CodeSeeX secret from Windows Credential Manager");
    }
    let credential_ref = unsafe { &*credential };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            credential_ref.CredentialBlob,
            credential_ref.CredentialBlobSize as usize,
        )
    };
    let value = String::from_utf8_lossy(bytes).trim().to_owned();
    unsafe { CredFree(credential.cast()) };
    Ok((!value.is_empty()).then_some(value))
}

#[cfg(windows)]
fn secret_store_write(target: &str, value: &str) -> Result<()> {
    use std::ptr::null_mut;
    use windows_sys::Win32::Security::Credentials::{
        CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
    };

    let mut target = wide_null(target);
    let mut user = wide_null("CodeSeeX");
    let mut blob = value.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_GENERIC,
        TargetName: target.as_mut_ptr(),
        Comment: null_mut(),
        LastWritten: Default::default(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: null_mut(),
        TargetAlias: null_mut(),
        UserName: user.as_mut_ptr(),
    };
    let ok = unsafe { CredWriteW(&credential, 0) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to write CodeSeeX secret to Windows Credential Manager");
    }
    Ok(())
}

#[cfg(windows)]
fn secret_store_delete(target: &str) -> Result<()> {
    use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
    use windows_sys::Win32::Security::Credentials::{CredDeleteW, CRED_TYPE_GENERIC};

    let target = wide_null(target);
    let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(());
        }
        return Err(error)
            .context("failed to clear CodeSeeX secret from Windows Credential Manager");
    }
    Ok(())
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

#[cfg(not(windows))]
fn secret_store_read(_target: &str) -> Result<Option<String>> {
    Ok(None)
}

#[cfg(not(windows))]
fn secret_store_write(_target: &str, _value: &str) -> Result<()> {
    anyhow::bail!("secure OS credential storage is not available on this platform")
}

#[cfg(not(windows))]
fn secret_store_delete(_target: &str) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn vision_secret_round_trips_through_os_credential_store() {
        let config = AppConfig {
            data_dir: std::env::temp_dir().join(format!(
                "codeseex-secret-test-{}",
                uuid::Uuid::new_v4().simple()
            )),
            ..Default::default()
        };

        clear_vision_api_key(&config).expect("clear initial");
        assert!(!vision_api_key_configured(&config));

        write_vision_api_key(&config, "secret-value").expect("write secret");
        assert_eq!(vision_api_key(&config).as_deref(), Some("secret-value"));
        assert!(vision_api_key_configured(&config));

        clear_vision_api_key(&config).expect("clear written");
        assert!(vision_api_key(&config).is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn vision_secret_write_fails_closed_without_os_store() {
        let config = AppConfig {
            data_dir: std::env::temp_dir().join(format!(
                "codeseex-secret-test-{}",
                uuid::Uuid::new_v4().simple()
            )),
            ..Default::default()
        };
        assert!(write_vision_api_key(&config, "secret-value").is_err());
        assert!(vision_api_key(&config).is_none());
    }
}
