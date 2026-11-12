//! The lab's own keychain on macOS: the `ngteams-lab` items live in
//! `~/Library/Application Support/ngteams-lab/ngteams-lab.keychain-db`, never in the user's login keychain.
//!
//! - Agents run the lab over SSH, where the login keychain is locked and can't ask to be unlocked; this keychain is
//!   unlocked by each operation with its password, which separates the lab from the user's keychains and is no secret:
//!   it guards the lab's tokens from the user's own programs no more than Secret Service does on Linux.
//! - `proto-lab auth` writes the items and the lab build's credential broker reads them: two programs, rebuilt often.
//!   So each item's decrypt rule names no program (as `security add-generic-password -A` makes it); the keychain's
//!   other rules stay the creator's.
//! - The keychain is never added to the user's search list, so nothing else of the user's sees or searches it.

use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::ptr;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, CFTypeRef, OSStatus, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::os::macos::access::SecAccess;
use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};
use security_framework_sys::base::SecAccessRef;
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecMatchSearchList, kSecReturnData,
    kSecUseKeychain, kSecValueData,
};
use security_framework_sys::keychain_item::{SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate};
use zeroize::Zeroizing;

use super::SecretError;

/// The namespace kept in the lab keychain.
pub(super) const APPLICATION: &str = "ngteams-lab";
const PASSWORD: &str = "ngteams-lab";
const FILE: &str = "Library/Application Support/ngteams-lab/ngteams-lab.keychain-db";

const ITEM_NOT_FOUND: OSStatus = -25300;
/// `errSecIO`: the keychain's directory couldn't be made.
const IO: OSStatus = -36;
/// `errSecNoSuchKeychain`: no absolute home to find the lab keychain in.
pub(super) const NO_SUCH_KEYCHAIN: OSStatus = -25294;
/// `errSecACLNotSimple`: a new item's access came without a decrypt rule to open.
const NO_DECRYPT_RULE: OSStatus = -25240;
/// `errSecDecode`: the item's data came back as something else than data.
const NOT_DATA: OSStatus = -26275;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecAttrAccess: CFStringRef;
    static kSecACLAuthorizationDecrypt: CFStringRef;
    fn SecAccessCreate(descriptor: CFStringRef, trusted: CFArrayRef, access: *mut SecAccessRef) -> OSStatus;
    fn SecAccessCopyMatchingACLList(access: SecAccessRef, authorization: CFTypeRef) -> CFArrayRef;
    fn SecACLSetContents(acl: CFTypeRef, applications: CFArrayRef, description: CFStringRef, prompt: u16) -> OSStatus;
}

/// The lab keychain under `home`; `None` without an absolute home.
pub(super) fn path(home: Option<OsString>) -> Option<PathBuf> {
    home.map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|home| home.join(FILE))
}

/// Writes or replaces the item, making the keychain when it isn't there yet.
pub(super) fn put(keychain: &Path, service: &str, account: &str, secret: &[u8]) -> Result<(), SecretError> {
    let keychain = open_or_create(keychain)?;
    let names = Names::get();
    let query = CFDictionary::from_CFType_pairs(&item(&names, &keychain, service, account));
    let data = CFData::from_buffer(secret);
    let change = CFDictionary::from_CFType_pairs(&[(names.value_data.clone(), data.as_CFType())]);
    // SAFETY: both dictionaries are valid CF objects for the call's duration
    match unsafe { SecItemUpdate(query.as_concrete_TypeRef(), change.as_concrete_TypeRef()) } {
        ITEM_NOT_FOUND => add(&names, &keychain, service, account, &data),
        status => checked(status),
    }
}

/// The item's data, or `None` when it (or the keychain) isn't there.
pub(super) fn get(keychain: &Path, service: &str, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretError> {
    let Some(keychain) = open(keychain)? else {
        return Ok(None);
    };
    let names = Names::get();
    let mut query = item(&names, &keychain, service, account);
    query.push((names.return_data, CFBoolean::true_value().as_CFType()));
    let query = CFDictionary::from_CFType_pairs(&query);
    let mut found: CFTypeRef = ptr::null();
    // SAFETY: a valid query; on success `found` receives an owned reference
    match unsafe { SecItemCopyMatching(query.as_concrete_TypeRef(), &raw mut found) } {
        ITEM_NOT_FOUND => return Ok(None),
        status => checked(status)?,
    }
    if found.is_null() {
        return Ok(None);
    }
    // SAFETY: the owned reference SecItemCopyMatching returned
    let found = unsafe { CFType::wrap_under_create_rule(found) };
    let data = found.downcast_into::<CFData>().ok_or(SecretError::Keychain(NOT_DATA))?;
    Ok(Some(Zeroizing::new(data.bytes().to_vec())))
}

/// Deletes the item; a missing item or keychain is fine.
pub(super) fn delete(keychain: &Path, service: &str, account: &str) -> Result<(), SecretError> {
    let Some(keychain) = open(keychain)? else {
        return Ok(());
    };
    let query = CFDictionary::from_CFType_pairs(&item(&Names::get(), &keychain, service, account));
    // SAFETY: a valid query dictionary
    match unsafe { SecItemDelete(query.as_concrete_TypeRef()) } {
        ITEM_NOT_FOUND => Ok(()),
        status => checked(status),
    }
}

/// The unlocked lab keychain, or `None` when it hasn't been made.
fn open(path: &Path) -> Result<Option<SecKeychain>, SecretError> {
    if !path.try_exists().map_err(|_| SecretError::Keychain(IO))? {
        return Ok(None);
    }
    let mut keychain = SecKeychain::open(path).map_err(failed)?;
    keychain.unlock(Some(PASSWORD)).map_err(failed)?;
    Ok(Some(keychain))
}

fn open_or_create(path: &Path) -> Result<SecKeychain, SecretError> {
    if let Some(keychain) = open(path)? {
        return Ok(keychain);
    }
    let parent = path.parent().ok_or(SecretError::Keychain(IO))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|_| SecretError::Keychain(IO))?;
    let keychain = CreateOptions::new()
        .password(PASSWORD)
        .prompt_user(false)
        .create(path)
        .map_err(failed)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|_| SecretError::Keychain(IO))?;
    Ok(keychain)
}

fn add(names: &Names, keychain: &SecKeychain, service: &str, account: &str, data: &CFData) -> Result<(), SecretError> {
    let service_name = CFString::new(service);
    let access = any_reader(&service_name)?;
    let attributes = CFDictionary::from_CFType_pairs(&[
        (names.class.clone(), names.generic_password.as_CFType()),
        (names.service.clone(), service_name.as_CFType()),
        (names.account.clone(), CFString::new(account).as_CFType()),
        (names.use_keychain.clone(), keychain.as_CFType()),
        (names.access.clone(), access.as_CFType()),
        (names.value_data.clone(), data.as_CFType()),
    ]);
    // SAFETY: a valid attribute dictionary; no result is asked for
    checked(unsafe { SecItemAdd(attributes.as_concrete_TypeRef(), ptr::null_mut()) })
}

/// A new item's access whose decrypt rule names no program, so the broker reads what `proto-lab` wrote (and a rebuilt
/// program what an earlier build wrote) without a prompt, which an SSH session couldn't answer.
fn any_reader(description: &CFString) -> Result<SecAccess, SecretError> {
    let mut raw: SecAccessRef = ptr::null_mut();
    // SAFETY: a valid description and a null trusted list (the creator); `raw` receives an owned reference on success
    checked(unsafe { SecAccessCreate(description.as_concrete_TypeRef(), ptr::null(), &raw mut raw) })?;
    // SAFETY: the owned reference SecAccessCreate returned
    let access = unsafe { SecAccess::wrap_under_create_rule(raw) };
    // SAFETY: an immutable constant of Security.framework, valid for the whole process
    let decrypt = unsafe { kSecACLAuthorizationDecrypt };
    // SAFETY: a valid access and authorization tag; the result is an owned array or null
    let rules = unsafe { SecAccessCopyMatchingACLList(access.as_concrete_TypeRef(), decrypt.cast()) };
    if rules.is_null() {
        return Err(SecretError::Keychain(NO_DECRYPT_RULE));
    }
    // SAFETY: the owned array SecAccessCopyMatchingACLList returned
    let rules = unsafe { CFArray::<CFType>::wrap_under_create_rule(rules) };
    for rule in rules.iter() {
        // SAFETY: a rule `access` owns; a null application list lets any program use the item, with no prompt
        let status =
            unsafe { SecACLSetContents(rule.as_CFTypeRef(), ptr::null(), description.as_concrete_TypeRef(), 0) };
        checked(status)?;
    }
    Ok(access)
}

/// The one item of `service` and `account`, searched for only in the lab keychain.
fn item(names: &Names, keychain: &SecKeychain, service: &str, account: &str) -> Vec<(CFString, CFType)> {
    vec![
        (names.class.clone(), names.generic_password.as_CFType()),
        (names.service.clone(), CFString::new(service).as_CFType()),
        (names.account.clone(), CFString::new(account).as_CFType()),
        (
            names.search_list.clone(),
            CFArray::from_CFTypes(std::slice::from_ref(keychain)).as_CFType(),
        ),
    ]
}

/// Security.framework's names of item attributes and values.
struct Names {
    class: CFString,
    generic_password: CFString,
    service: CFString,
    account: CFString,
    search_list: CFString,
    use_keychain: CFString,
    access: CFString,
    return_data: CFString,
    value_data: CFString,
}

impl Names {
    fn get() -> Self {
        // SAFETY: each read below is of an immutable CFString constant of Security.framework, valid for the process
        let class = unsafe { kSecClass };
        // SAFETY: as above
        let generic_password = unsafe { kSecClassGenericPassword };
        // SAFETY: as above
        let service = unsafe { kSecAttrService };
        // SAFETY: as above
        let account = unsafe { kSecAttrAccount };
        // SAFETY: as above
        let search_list = unsafe { kSecMatchSearchList };
        // SAFETY: as above
        let use_keychain = unsafe { kSecUseKeychain };
        // SAFETY: as above
        let access = unsafe { kSecAttrAccess };
        // SAFETY: as above
        let return_data = unsafe { kSecReturnData };
        // SAFETY: as above
        let value_data = unsafe { kSecValueData };
        Self {
            class: constant(class),
            generic_password: constant(generic_password),
            service: constant(service),
            account: constant(account),
            search_list: constant(search_list),
            use_keychain: constant(use_keychain),
            access: constant(access),
            return_data: constant(return_data),
            value_data: constant(value_data),
        }
    }
}

/// A Security.framework constant as a `CFString`; only [`Names::get`] calls it, with those constants.
fn constant(raw: CFStringRef) -> CFString {
    // SAFETY: one of Security.framework's constant CFStrings: never null, never released
    unsafe { CFString::wrap_under_get_rule(raw) }
}

fn checked(status: OSStatus) -> Result<(), SecretError> {
    if status == 0 {
        Ok(())
    } else {
        Err(SecretError::Keychain(status))
    }
}

fn failed(error: security_framework::base::Error) -> SecretError {
    SecretError::Keychain(error.code())
}
