//! Windows-only privacy boundary for the disabled action journal. It never
//! resolves a provider target or enables an action. Keep the returned guard
//! alive across every path-based journal operation.
#![allow(dead_code)]

use std::ffi::{c_void, OsStr};
use std::fs::File;
use std::mem::{size_of, MaybeUninit};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::{NamedTempFile, TempPath};
use windows_sys::Win32::Foundation::{
    GetLastError, LocalFree, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, IsValidSid, TokenUser, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, CreateFileW, FileAttributeTagInfo, FileIdInfo, GetDriveTypeW,
    GetFileInformationByHandleEx, GetVolumeInformationByHandleW, GetVolumePathNameW, CREATE_NEW,
    FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::WindowsProgramming::{DRIVE_FIXED, FS_PERSISTENT_ACLS};

static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
const SYSTEM_SID: &str = "S-1-5-18";
const SDDL_REVISION_1: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PrivacyError {
    Unsafe,
    Io,
}

struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

struct SecurityPolicy {
    user_sid: String,
    directory_descriptor: LocalMemory,
    file_descriptor: LocalMemory,
}

impl SecurityPolicy {
    fn current() -> Result<Self, PrivacyError> {
        let user_sid = current_user_sid()?;
        // D:P blocks inherited ACEs. Directory ACEs are inheritable so a
        // third-party temp-file API cannot briefly create a public child.
        let directory_descriptor = descriptor(&format!(
            "O:{user_sid}D:P(A;OICI;FA;;;{user_sid})(A;OICI;FA;;;SY)"
        ))?;
        let file_descriptor =
            descriptor(&format!("O:{user_sid}D:P(A;;FA;;;{user_sid})(A;;FA;;;SY)"))?;
        Ok(Self {
            user_sid,
            directory_descriptor,
            file_descriptor,
        })
    }

    fn attributes(&self, directory: bool) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: if directory {
                self.directory_descriptor.0
            } else {
                self.file_descriptor.0
            },
            bInheritHandle: 0,
        }
    }
}

/// This guard pins both directories without FILE_SHARE_DELETE. A caller must
/// retain it through file creation, event publication, and path validation.
pub(crate) struct PrivateJournal {
    root: PathBuf,
    journal: PathBuf,
    root_handle: OwnedHandle,
    journal_handle: OwnedHandle,
    policy: SecurityPolicy,
}

impl PrivateJournal {
    pub(crate) fn open_or_create(root: &Path, journal: &Path) -> Result<Self, PrivacyError> {
        Self::open(root, journal, true)
    }

    pub(crate) fn open_existing(root: &Path, journal: &Path) -> Result<Option<Self>, PrivacyError> {
        match std::fs::symlink_metadata(journal) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(PrivacyError::Io),
            Ok(_) => {}
        }
        Self::open(root, journal, false).map(Some)
    }

    fn open(root: &Path, journal: &Path, create: bool) -> Result<Self, PrivacyError> {
        if !root.is_absolute() || journal.parent() != Some(root) || journal.file_name().is_none() {
            return Err(PrivacyError::Unsafe);
        }
        let root_handle = open_directory(root)?;
        verify_type(&root_handle, true)?;
        verify_local_acl_volume(root, &root_handle)?;
        let policy = SecurityPolicy::current()?;
        let journal_wide = wide_path(journal)?;
        let attributes = policy.attributes(true);
        // An existing directory is inspected without changing its ACL.
        if create
            && unsafe { CreateDirectoryW(journal_wide.as_ptr(), &attributes) } == 0
            && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS
        {
            return Err(PrivacyError::Io);
        }
        let journal_handle = open_directory(journal)?;
        verify_type(&journal_handle, true)?;
        verify_security(&journal_handle, &policy.user_sid, true)?;
        Ok(Self {
            root: root.to_path_buf(),
            journal: journal.to_path_buf(),
            root_handle,
            journal_handle,
            policy,
        })
    }

    pub(crate) fn create_file(&self, path: &Path) -> Result<File, PrivacyError> {
        self.check_child(path)?;
        let wide = wide_path(path)?;
        let attributes = self.policy.attributes(false);
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_ALL_ACCESS,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(PrivacyError::Io);
        }
        let file = unsafe { File::from_raw_handle(raw) };
        verify_type(&file, false)?;
        verify_security(&file, &self.policy.user_sid, false)?;
        Ok(file)
    }

    pub(crate) fn create_event_stage(&self) -> Result<NamedTempFile<File>, PrivacyError> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| PrivacyError::Io)?
            .as_nanos();
        let seq = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
        let path = self.journal.join(format!(
            ".odometer-stage-{stamp:032x}-{:08x}-{seq:016x}",
            std::process::id()
        ));
        let file = self.create_file(&path)?;
        let path = TempPath::try_from_path(path).map_err(|_| PrivacyError::Io)?;
        Ok(NamedTempFile::from_parts(file, path))
    }

    pub(crate) fn open_file(&self, path: &Path) -> Result<File, PrivacyError> {
        self.open_file_optional(path)?.ok_or(PrivacyError::Io)
    }

    pub(crate) fn open_file_optional(&self, path: &Path) -> Result<Option<File>, PrivacyError> {
        self.check_child(path)?;
        let wide = wide_path(path)?;
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_READ_DATA,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            if unsafe { GetLastError() } == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            return Err(PrivacyError::Io);
        }
        let file = unsafe { File::from_raw_handle(raw) };
        verify_type(&file, false)?;
        verify_security(&file, &self.policy.user_sid, false)?;
        Ok(Some(file))
    }

    pub(crate) fn verify_binding(&self) -> Result<(), PrivacyError> {
        let reopened_root = open_directory(&self.root)?;
        let reopened_journal = open_directory(&self.journal)?;
        if file_id(&reopened_root)? != file_id(&self.root_handle)?
            || file_id(&reopened_journal)? != file_id(&self.journal_handle)?
        {
            return Err(PrivacyError::Unsafe);
        }
        verify_type(&reopened_journal, true)?;
        verify_security(&reopened_journal, &self.policy.user_sid, true)
    }

    fn check_child(&self, path: &Path) -> Result<(), PrivacyError> {
        if !path.is_absolute() || path.parent() != Some(self.journal.as_path()) {
            return Err(PrivacyError::Unsafe);
        }
        let name = path
            .file_name()
            .and_then(|part| part.to_str())
            .ok_or(PrivacyError::Unsafe)?;
        if name.is_empty()
            || name.len() > 160
            || name == "."
            || name == ".."
            || name
                .chars()
                .any(|character| character.is_control() || character == ':')
        {
            return Err(PrivacyError::Unsafe);
        }
        wide_path(path)?;
        self.verify_binding()
    }
}

fn wide(value: &OsStr) -> Result<Vec<u16>, PrivacyError> {
    let mut result: Vec<u16> = value.encode_wide().collect();
    if result.is_empty() || result.contains(&0) {
        return Err(PrivacyError::Unsafe);
    }
    result.push(0);
    Ok(result)
}

fn wide_path(path: &Path) -> Result<Vec<u16>, PrivacyError> {
    wide(path.as_os_str())
}

fn descriptor(sddl: &str) -> Result<LocalMemory, PrivacyError> {
    let text = wide(OsStr::new(sddl))?;
    let mut ptr = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut ptr,
            null_mut(),
        )
    } == 0
        || ptr.is_null()
    {
        return Err(PrivacyError::Io);
    }
    Ok(LocalMemory(ptr))
}

fn sid_string(sid: *mut c_void) -> Result<String, PrivacyError> {
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(PrivacyError::Unsafe);
    }
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
        return Err(PrivacyError::Io);
    }
    let owned = LocalMemory(text.cast());
    let mut length = 0usize;
    while length < 256 && unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    if length == 256 {
        return Err(PrivacyError::Unsafe);
    }
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|_| PrivacyError::Unsafe)?;
    drop(owned);
    Ok(result)
}

fn current_user_sid() -> Result<String, PrivacyError> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(PrivacyError::Io);
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut needed = 0u32;
    if unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed) }
        != 0
        || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
        || needed < size_of::<TOKEN_USER>() as u32
        || needed > 4096
    {
        return Err(PrivacyError::Io);
    }
    // Vec<usize> gives TOKEN_USER pointer alignment, unlike Vec<u8>.
    let words = (needed as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0usize; words];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(PrivacyError::Io);
    }
    let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
    sid_string(sid)
}

fn open_directory(path: &Path) -> Result<OwnedHandle, PrivacyError> {
    let wide = wide_path(path)?;
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(PrivacyError::Io);
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn verify_local_acl_volume(root: &Path, handle: &OwnedHandle) -> Result<(), PrivacyError> {
    let wide = wide_path(root)?;
    let mut volume_path = [0u16; 512];
    if unsafe {
        GetVolumePathNameW(
            wide.as_ptr(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
        )
    } == 0
    {
        return Err(PrivacyError::Unsafe);
    }
    let drive_type = unsafe { GetDriveTypeW(volume_path.as_ptr()) };
    let mut flags = 0u32;
    if unsafe {
        GetVolumeInformationByHandleW(
            handle.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        )
    } == 0
        || !volume_allowed(drive_type, flags)
    {
        return Err(PrivacyError::Unsafe);
    }
    Ok(())
}

fn volume_allowed(drive_type: u32, flags: u32) -> bool {
    drive_type == DRIVE_FIXED && flags & FS_PERSISTENT_ACLS != 0
}

fn attributes(handle: &impl AsRawHandle) -> Result<FILE_ATTRIBUTE_TAG_INFO, PrivacyError> {
    let mut info = MaybeUninit::<FILE_ATTRIBUTE_TAG_INFO>::uninit();
    if unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileAttributeTagInfo,
            info.as_mut_ptr().cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    } == 0
    {
        return Err(PrivacyError::Io);
    }
    Ok(unsafe { info.assume_init() })
}

fn verify_type(handle: &impl AsRawHandle, directory: bool) -> Result<(), PrivacyError> {
    let info = attributes(handle)?;
    if info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
    {
        return Err(PrivacyError::Unsafe);
    }
    Ok(())
}

fn file_id(handle: &impl AsRawHandle) -> Result<(u64, [u8; 16]), PrivacyError> {
    let mut info = MaybeUninit::<FILE_ID_INFO>::uninit();
    if unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(PrivacyError::Io);
    }
    let info = unsafe { info.assume_init() };
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

fn canonical_owner_dacl(descriptor: *mut c_void) -> Result<String, PrivacyError> {
    if descriptor.is_null() {
        return Err(PrivacyError::Unsafe);
    }
    let mut text = null_mut();
    let mut length = 0u32;
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut text,
            &mut length,
        )
    } == 0
        || text.is_null()
    {
        return Err(PrivacyError::Unsafe);
    }
    let _text = LocalMemory(text.cast());
    if length == 0 || length > 2048 {
        return Err(PrivacyError::Unsafe);
    }
    let units = unsafe { std::slice::from_raw_parts(text, length as usize) };
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    String::from_utf16(&units[..end]).map_err(|_| PrivacyError::Unsafe)
}

fn permitted_owner_dacl(user_sid: &str, directory: bool) -> Result<[String; 2], PrivacyError> {
    let flags = if directory { "OICI" } else { "" };
    let user_ace = format!("(A;{flags};FA;;;{user_sid})");
    let system_ace = format!("(A;{flags};FA;;;SY)");
    let prefix = format!("O:{user_sid}D:P");
    let first = descriptor(&format!("{prefix}{user_ace}{system_ace}"))?;
    let second = descriptor(&format!("{prefix}{system_ace}{user_ace}"))?;
    Ok([
        canonical_owner_dacl(first.0)?,
        canonical_owner_dacl(second.0)?,
    ])
}

fn verify_security(
    handle: &impl AsRawHandle,
    user_sid: &str,
    directory: bool,
) -> Result<(), PrivacyError> {
    let mut owner = null_mut();
    let mut dacl = null_mut();
    let mut descriptor = null_mut();
    let result = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    let _descriptor = LocalMemory(descriptor);
    if result != 0 || descriptor.is_null() {
        return Err(PrivacyError::Io);
    }
    if sid_string(owner)? != user_sid || dacl.is_null() {
        return Err(PrivacyError::Unsafe);
    }
    // Compare semantic owner/DACL policies through the same native formatter.
    // Windows may abbreviate a built-in SID (for example RID 500 as LA), so
    // comparing the observed string to a raw SID spelling rejects a safe ACL.
    // Native SDDL normalization may omit audit-only ACE flag bits; this is an
    // access-policy check, not a binary ACL identity check.
    let actual = canonical_owner_dacl(descriptor)?;
    let expected = permitted_owner_dacl(user_sid, directory)?;
    if expected.contains(&actual) {
        Ok(())
    } else {
        Err(PrivacyError::Unsafe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use windows_sys::Win32::Security::GetSecurityDescriptorLength;

    fn make_directory_with_acl(path: &Path, sddl: &str) {
        let descriptor = descriptor(sddl).unwrap();
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let name = wide_path(path).unwrap();
        assert_ne!(unsafe { CreateDirectoryW(name.as_ptr(), &attributes) }, 0);
    }

    fn security_bytes(path: &Path) -> Vec<u8> {
        let handle = open_directory(path).unwrap();
        let mut descriptor = null_mut();
        let result = unsafe {
            GetSecurityInfo(
                handle.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(result, 0);
        let _descriptor = LocalMemory(descriptor);
        let length = unsafe { GetSecurityDescriptorLength(descriptor) } as usize;
        assert!(length > 0 && length < 4096);
        unsafe { std::slice::from_raw_parts(descriptor.cast::<u8>(), length) }.to_vec()
    }

    #[test]
    fn creates_private_journal_backup_and_event_stage() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join("private-action-journal");
        let guard = PrivateJournal::open_or_create(root.path(), &journal).unwrap();
        let backup = journal.join("synthetic.backup");
        let file = guard.create_file(&backup).unwrap();
        drop(file);
        guard.open_file(&backup).unwrap();
        let stage = guard.create_event_stage().unwrap();
        let stage_path = stage.path().to_path_buf();
        guard.open_file(&stage_path).unwrap();
        guard.verify_binding().unwrap();
        assert!(fs::rename(&journal, root.path().join("swapped")).is_err());
        drop(stage);
        assert!(!stage_path.exists());
    }

    #[test]
    fn rejects_unsafe_existing_journal_without_rewriting_it() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join("private-action-journal");
        // Ordinary creation inherits the root's default ACL. Do not repair it
        // after creation, since it may already have exposed backup contents.
        fs::create_dir(&journal).unwrap();
        let before = security_bytes(&journal);
        assert!(PrivateJournal::open_or_create(root.path(), &journal).is_err());
        assert_eq!(security_bytes(&journal), before);
        assert!(journal.exists());
        assert_eq!(fs::read_dir(&journal).unwrap().count(), 0);
    }

    #[test]
    fn rejects_null_and_broad_dacls_without_changing_them() {
        let user = current_user_sid().unwrap();
        let cases = [
            format!("O:{user}D:NO_ACCESS_CONTROL"),
            format!("O:{user}D:P(A;OICI;FA;;;{user})(A;OICI;FA;;;SY)(A;OICI;FR;;;WD)"),
        ];
        for (case_index, sddl) in cases.into_iter().enumerate() {
            let root = tempfile::tempdir().unwrap();
            let journal = root.path().join("private-action-journal");
            make_directory_with_acl(&journal, &sddl);
            let before = security_bytes(&journal);
            assert!(
                PrivateJournal::open_or_create(root.path(), &journal).is_err(),
                "unsafe ACL case {case_index} was accepted"
            );
            assert_eq!(security_bytes(&journal), before);
            assert_eq!(fs::read_dir(&journal).unwrap().count(), 0);
        }
    }

    #[test]
    fn rejects_reparse_journal_when_symlink_creation_is_available() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("destination");
        let journal = root.path().join("private-action-journal");
        fs::create_dir(&destination).unwrap();
        match std::os::windows::fs::symlink_dir(&destination, &journal) {
            Ok(()) => assert!(PrivateJournal::open_or_create(root.path(), &journal).is_err()),
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314) => {}
            Err(error) => panic!("synthetic symlink creation failed: {error}"),
        }
    }

    #[test]
    fn rejects_junction_journal_without_touching_its_referent() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("destination");
        let journal = root.path().join("private-action-journal");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("synthetic-sentinel"), b"unchanged").unwrap();
        // Static test-only command; paths are data in environment variables,
        // never interpolated into PowerShell source or production behavior.
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path $env:ODOMETER_TEST_JUNCTION -Value $env:ODOMETER_TEST_DESTINATION | Out-Null",
            ])
            .env("ODOMETER_TEST_JUNCTION", &journal)
            .env("ODOMETER_TEST_DESTINATION", &destination)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(PrivateJournal::open_or_create(root.path(), &journal).is_err());
        assert_eq!(
            fs::read(destination.join("synthetic-sentinel")).unwrap(),
            b"unchanged"
        );
        fs::remove_dir(&journal).unwrap();
        assert_eq!(
            fs::read(destination.join("synthetic-sentinel")).unwrap(),
            b"unchanged"
        );
    }

    #[test]
    fn rejects_non_directory_and_embedded_nul() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join("private-action-journal");
        fs::write(&journal, b"synthetic").unwrap();
        assert!(PrivateJournal::open_or_create(root.path(), &journal).is_err());
        let bad = root.path().join("bad\0name");
        assert_eq!(wide_path(&bad), Err(PrivacyError::Unsafe));
    }

    #[test]
    fn rejects_wrong_owner_policy_and_nonpersistent_or_remote_volume_flags() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join("private-action-journal");
        let guard = PrivateJournal::open_or_create(root.path(), &journal).unwrap();
        assert_eq!(
            verify_security(&guard.journal_handle, SYSTEM_SID, true),
            Err(PrivacyError::Unsafe)
        );
        assert!(!volume_allowed(DRIVE_FIXED, 0));
        assert!(!volume_allowed(4, FS_PERSISTENT_ACLS));
    }

    #[test]
    fn canonicalizes_builtin_sid_alias_before_policy_comparison() {
        // LOCAL SERVICE has a stable well-known SID and is distinct from
        // SYSTEM. Windows abbreviates it to LS on every installation.
        let local_service = "S-1-5-19";
        for directory in [true, false] {
            let flags = if directory { "OICI" } else { "" };
            let raw =
                format!("O:{local_service}D:P(A;{flags};FA;;;{local_service})(A;{flags};FA;;;SY)");
            let policy = descriptor(&raw).unwrap();
            let canonical = canonical_owner_dacl(policy.0).unwrap();
            assert_ne!(canonical, raw);
            assert!(permitted_owner_dacl(local_service, directory)
                .unwrap()
                .contains(&canonical));
        }
    }
}
