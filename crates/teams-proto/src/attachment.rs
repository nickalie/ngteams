//! Own-drive chat attachments: upload privately, grant named readers on Send, then post the captured file entry.

use crate::Mri;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

use crate::write::RawBody;
use crate::{Endpoint, Method, Read, Service};

/// The content endpoint accepts files up to 250 MB; larger files need an upload session.
pub const MAX_ATTACHMENT_SIZE: u64 = 250 * 1024 * 1024;

/// The account's own drive, discovered through Graph rather than a guessed tenant host or account path.
pub struct OwnDrive;
impl Read for OwnDrive {
    fn service(&self) -> Service {
        Service::Graph
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["me", "drive"]).with_query("$select", "id,owner,driveType,webUrl")
    }
}

/// A validated personal SharePoint site. Its token goes only to this site's SharePoint origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveSite(String);

impl DriveSite {
    #[must_use]
    pub fn from_web_url(web_url: &str) -> Option<Self> {
        Self::from_web_url_in(&crate::Cloud::Microsoft, web_url)
    }

    /// Validates a personal drive site within the account's file service.
    #[must_use]
    pub fn from_web_url_in(cloud: &crate::Cloud, web_url: &str) -> Option<Self> {
        let url = Url::parse(web_url).ok()?;
        if !cloud.is_microsoft() {
            let projected = cloud.project(&url, "/sp", "https://own-my.sharepoint.com")?;
            let site = Self::from_web_url(projected.as_str())?;
            let path = Url::parse(site.base()).ok()?.path().to_owned();
            return Some(Self(cloud.service_url(site.base(), &format!("/sp{path}")).ok()?.into()));
        }
        let host = url.host_str()?;
        let tenant = host.strip_suffix("-my.sharepoint.com")?;
        let parts: Vec<_> = url.path_segments()?.collect();
        if url.scheme() != "https"
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || tenant.is_empty()
            || !tenant.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || parts.len() < 2
            || parts[0] != "personal"
            || parts[1].is_empty()
            || !parts[1]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return None;
        }
        Some(Self(format!("https://{host}/personal/{}/", parts[1])))
    }
    #[must_use]
    pub fn base(&self) -> &str {
        &self.0
    }
    #[must_use]
    pub fn drive_base(&self) -> String {
        format!("{}_api/v2.0/drive", self.0)
    }
    #[must_use]
    pub fn audience(&self) -> String {
        Url::parse(&self.0).map_or_else(|_| String::new(), |url| url.origin().ascii_serialization())
    }
}

/// A completed private upload. This metadata survives a failed message send without uploading the file again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadedFile {
    site: String,
    item: String,
    guid: String,
    name: String,
    object_url: String,
    size: u64,
    drive: String,
    sharepoint_ids: Value,
}

impl UploadedFile {
    /// Reads the immutable metadata the native outbox stores in a file entry, rejecting unrecognized routes.
    #[must_use]
    pub fn from_entry(entry: &Value) -> Option<Self> {
        Self::from_entry_in(&crate::Cloud::Microsoft, entry)
    }

    /// Restores an outbox file only when its site belongs to the account's cloud.
    #[must_use]
    pub fn from_entry_in(cloud: &crate::Cloud, entry: &Value) -> Option<Self> {
        if entry.get("@type")?.as_str()? != "http://schema.skype.com/File" {
            return None;
        }
        let site = DriveSite::from_web_url_in(cloud, entry.get("baseUrl")?.as_str()?)?;
        let object_url = entry.get("objectUrl")?.as_str()?;
        let guid = entry.get("itemid")?.as_str()?;
        let name = entry.get("fileName")?.as_str()?;
        if site.base() != entry.get("baseUrl")?.as_str()?
            || !crate::read::is_guid(guid)
            || !valid_name(name)
            || !object_url.starts_with(site.base())
            || !valid_object_url(&site, object_url)
        {
            return None;
        }
        Some(Self {
            site: site.base().to_owned(),
            item: entry.pointer("/fileInfo/itemId")?.as_str()?.to_owned(),
            guid: guid.to_owned(),
            name: name.to_owned(),
            object_url: object_url.to_owned(),
            size: 0,
            drive: entry.pointer("/sharepointIds/driveId")?.as_str()?.to_owned(),
            sharepoint_ids: entry.get("sharepointIds")?.clone(),
        })
    }
    /// The captured successful upload answer; links must remain in the site the upload went to.
    #[must_use]
    pub fn from_upload(site: &DriveSite, body: &[u8]) -> Option<Self> {
        let body: Value = serde_json::from_slice(body).ok()?;
        let item = body.get("id")?.as_str()?;
        let guid = body.pointer("/sharepointIds/listItemUniqueId")?.as_str()?;
        let name = body.get("name")?.as_str()?;
        let object_url = body.get("webUrl")?.as_str()?;
        if !crate::read::is_guid(guid)
            || item.is_empty()
            || !item.bytes().all(|byte| byte.is_ascii_alphanumeric())
            || !valid_name(name)
            || !object_url.starts_with(site.base())
            || !valid_object_url(site, object_url)
        {
            return None;
        }
        let drive = body.pointer("/parentReference/driveId")?.as_str()?.to_owned();
        Some(Self {
            site: site.base().to_owned(),
            item: item.to_owned(),
            guid: guid.to_owned(),
            name: name.to_owned(),
            object_url: object_url.to_owned(),
            size: body.get("size")?.as_u64()?,
            drive,
            sharepoint_ids: body.get("sharepointIds")?.clone(),
        })
    }
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }
    #[must_use]
    pub fn site(&self) -> Option<DriveSite> {
        self.site_in(&crate::Cloud::Microsoft)
    }

    /// This upload's site, checked against the selected account.
    #[must_use]
    pub fn site_in(&self, cloud: &crate::Cloud) -> Option<DriveSite> {
        DriveSite::from_web_url_in(cloud, &self.site)
    }
    #[must_use]
    pub fn item_url(&self) -> String {
        format!("{}_api/v2.0/sites/root/items/{}/driveItem", self.site, self.guid)
    }
    #[must_use]
    pub fn entry(&self) -> Value {
        let extension = self.name.rsplit_once('.').map_or("", |(_, extension)| extension);
        let mut ids = self.sharepoint_ids.clone();
        if let Some(ids) = ids.as_object_mut() {
            ids.insert("driveId".to_owned(), self.drive.clone().into());
        }
        json!({"itemid":self.guid,"fileName":self.name,"fileType":extension,
            "fileInfo":{"itemId":self.item,"fileUrl":self.object_url,"siteUrl":self.site,"serverRelativeUrl":"","shareUrl":self.object_url},
            "fileChicletState":{"serviceName":"p2p","state":"active"},"@type":"http://schema.skype.com/File",
            "version":2,"id":self.guid,"baseUrl":self.site,"objectUrl":self.object_url,"openUrl":null,
            "type":extension,"title":self.name,"state":"active","chicletBreadcrumbs":null,"providerData":"",
            "botFileProperties":{},"isUploadError":null,"progressComplete":null,
            "filePreview":{"previewUrl":"","previewHeight":0,"previewWidth":0},"sharepointIds":ids,"publication":null,"site":null})
    }
}

fn valid_object_url(site: &DriveSite, object_url: &str) -> bool {
    Url::parse(object_url).is_ok_and(|url| {
        url.as_str().starts_with(site.base())
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '?', ':', '*', '"', '<', '>', '|'])
        && !name.chars().any(char::is_control)
}

/// A verified owner's complete direct-user grants. Broad links, groups, pages and unknown forms return no answer.
#[must_use]
pub fn direct_file_grants(body: &Value, owner: &str) -> Option<Vec<Mri>> {
    let object = body.as_object()?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "value" | "@odata.context"))
    {
        return None;
    }
    let mut members = Vec::new();
    let mut owns = false;
    for grant in body.get("value")?.as_array()? {
        let grant = grant.as_object()?;
        if grant.keys().any(|key| {
            !matches!(
                key.as_str(),
                "@deprecated.GrantedTo" | "id" | "roles" | "shareId" | "grantedToV2" | "grantedTo" | "inheritedFrom"
            )
        }) {
            return None;
        }
        let roles = grant.get("roles")?.as_array()?;
        if roles.is_empty()
            || roles
                .iter()
                .any(|role| !matches!(role.as_str(), Some("read" | "write" | "owner")))
        {
            return None;
        }
        let identity = grant.get("grantedToV2")?.as_object()?;
        if identity.keys().any(|key| !matches!(key.as_str(), "user" | "siteUser")) {
            return None;
        }
        let id = identity.get("user")?.get("id")?.as_str()?;
        if !crate::read::is_guid(id) {
            return None;
        }
        if let Some(legacy) = grant.get("grantedTo")
            && legacy.pointer("/user/id")?.as_str()? != id
        {
            return None;
        }
        owns |= id == owner && roles.iter().any(|role| role.as_str() == Some("owner"));
        let mri = Mri::new(format!("8:orgid:{id}"));
        if !members.contains(&mri) {
            members.push(mri);
        }
    }
    owns.then_some(members)
}

/// An immutable own-drive write, checked against fresh ownership, effective grants and expected recipients before delivery.
#[derive(Clone, Debug)]
pub struct DriveWrite {
    url: Url,
    method: Method,
    body: RawBody,
    content_type: &'static str,
    pub(crate) owner: String,
    pub(crate) file: Option<UploadedFile>,
    pub(crate) conversation: Option<String>,
    pub(crate) recipients: Vec<(String, String)>,
}

impl DriveWrite {
    #[must_use]
    pub fn upload(site: &DriveSite, name: &str, bytes: Vec<u8>, owner: &str) -> Option<Self> {
        crate::object_id(owner)?;
        if !valid_name(name) || bytes.len() as u64 > MAX_ATTACHMENT_SIZE {
            return None;
        }
        let mut url = Url::parse(&site.drive_base()).ok()?;
        url.path_segments_mut()
            .ok()?
            .push("root:")
            .push("Microsoft Teams Chat Files")
            .push(&format!("{name}:"))
            .push("content");
        url.query_pairs_mut()
            .append_pair("@name.conflictBehavior", "rename")
            .append_pair("$select", "*,sharepointIds,webDavUrl");
        Some(Self {
            url,
            method: Method::Put,
            body: bytes.into(),
            content_type: "application/octet-stream",
            owner: owner.to_owned(),
            file: None,
            conversation: None,
            recipients: Vec::new(),
        })
    }
    #[must_use]
    pub fn share(
        file: &UploadedFile,
        conversation: &str,
        recipients: &[(String, String)],
        owner: &str,
    ) -> Option<Self> {
        Self::share_in(&crate::Cloud::Microsoft, file, conversation, recipients, owner)
    }

    /// Grants named readers access only to an upload on this account's file service.
    #[must_use]
    pub fn share_in(
        cloud: &crate::Cloud,
        file: &UploadedFile,
        conversation: &str,
        recipients: &[(String, String)],
        owner: &str,
    ) -> Option<Self> {
        crate::object_id(owner)?;
        let addresses: Vec<String> = recipients.iter().map(|(_, address)| address.clone()).collect();
        if !crate::read::is_guid(&file.guid)
            || file.site_in(cloud).as_ref().map(DriveSite::base) != Some(file.site.as_str())
            || recipients.is_empty()
            || addresses.iter().any(|email| {
                email.trim() != email
                    || email.matches('@').count() != 1
                    || email.starts_with('@')
                    || email.ends_with('@')
                    || email.contains([',', ';', '<', '>'])
                    || email.chars().any(char::is_whitespace)
            })
        {
            return None;
        }
        let url = Url::parse(&format!("{}/invite", file.item_url())).ok()?;
        let body = json!({"recipients":addresses.iter().map(|email|json!({"email":email})).collect::<Vec<_>>(),"roles":["write"],"requireSignIn":true,"sendInvitation":false});
        Some(Self {
            url,
            method: Method::Post,
            body: body.to_string().into_bytes().into(),
            content_type: "application/json",
            owner: owner.to_owned(),
            file: Some(file.clone()),
            conversation: Some(conversation.to_owned()),
            recipients: recipients.to_vec(),
        })
    }
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }
    pub(crate) fn method(&self) -> Method {
        self.method
    }
    pub(crate) fn body(&self) -> &[u8] {
        self.body.bytes()
    }
    pub(crate) fn content_type(&self) -> &'static str {
        self.content_type
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const OWNER: &str = "00000000-0000-4000-8000-00000000000a";
    #[test]
    fn only_complete_owner_visible_direct_grants_are_evidence() {
        let owner = json!({"id":"owner","roles":["owner"],"grantedToV2":{"user":{"id":OWNER}}});
        assert_eq!(
            direct_file_grants(&json!({"value":[owner.clone()]}), OWNER)
                .unwrap()
                .len(),
            1
        );
        for grant in [
            json!({"roles":["write"],"link":{"scope":"anonymous"}}),
            json!({"roles":["write"],"grantedToV2":{"group":{"id":OWNER}}}),
            json!({"roles":["write"],"invitation":{"email":"peer@madeup.test"}}),
        ] {
            assert!(direct_file_grants(&json!({"value":[owner.clone(),grant]}), OWNER).is_none());
        }
        assert!(direct_file_grants(&json!({"value":[owner],"@odata.nextLink":"next"}), OWNER).is_none());
        assert!(direct_file_grants(&json!({"value":[]}), OWNER).is_none());
    }
    #[test]
    fn file_entries_cannot_escape_the_upload_site_or_smuggle_a_sharing_link() {
        let site = DriveSite::from_web_url("https://madeup-my.sharepoint.com/personal/lab_a/").unwrap();
        let mut body = json!({"id":"ABC1","name":"demo.txt","size":10,"parentReference":{"driveId":"drive1"},"webUrl":format!("{}Documents/demo.txt",site.base()),"sharepointIds":{"listItemUniqueId":"00000000-0000-4000-8000-000000000001"}});
        let file = UploadedFile::from_upload(&site, &serde_json::to_vec(&body).unwrap()).unwrap();
        let entry = file.entry();
        assert!(UploadedFile::from_entry(&entry).is_some());
        for url in [
            "https://outside.test/file",
            "https://madeup-my.sharepoint.com/personal/lab_a/../elsewhere/file",
            "https://madeup-my.sharepoint.com/personal/lab_a/demo.txt?share=anyone",
            "https://madeup-my.sharepoint.com/personal/lab_a/demo.txt#fragment",
        ] {
            body["webUrl"] = url.into();
            assert!(
                UploadedFile::from_upload(&site, &serde_json::to_vec(&body).unwrap()).is_none(),
                "{url}"
            );
        }
        let mut entry = entry;
        entry["baseUrl"] = "https://outside.test/".into();
        assert!(UploadedFile::from_entry(&entry).is_none());
    }
}
