//! Handwritten shapes after a channel file's capture; made-up tenant, team, channel and file names.
use super::*;
use serde_json::json;

const CHANNEL: &str = "19:madeupchannel000000000000000000@thread.tacv2";
const SITE: &str = "https://madeup.sharepoint.com/sites/labteam";
const FILE: &str = "00000000-0000-4000-8000-0000000000f1";

fn thread(properties: &Value) -> Value {
    json!({"id": CHANNEL, "properties": properties})
}

fn folder() -> ChannelFolder {
    ChannelFolder::from_thread_in(
        &crate::Cloud::Microsoft,
        CHANNEL,
        &thread(&json!({
            "sharepointSiteUrl": SITE,
            "sharepointRootLibrary": "Shared Documents",
            "sharepointChannelDocsFolder": "General",
            "channelDocsFolderRelativeUrl": "/sites/labteam/Shared Documents/General",
        })),
    )
    .unwrap()
}

#[test]
fn a_team_site_is_a_tenant_site_never_a_personal_one() {
    let cloud = crate::Cloud::Microsoft;
    let site = TeamSite::from_url_in(&cloud, SITE).unwrap();
    assert_eq!(site.base(), "https://madeup.sharepoint.com/sites/labteam/");
    assert_eq!(site.url(), SITE);
    assert_eq!(site.path(), "/sites/labteam");
    assert_eq!(site.audience(), "https://madeup.sharepoint.com");
    assert!(TeamSite::from_url_in(&cloud, "https://madeup.sharepoint.com/teams/lab-team.1/").is_some());
    for url in [
        "http://madeup.sharepoint.com/sites/labteam",
        "https://madeup-my.sharepoint.com/sites/labteam",
        "https://madeup.sharepoint.com.outside.test/sites/labteam",
        "https://madeup.sharepoint.com:444/sites/labteam",
        "https://user@madeup.sharepoint.com/sites/labteam",
        "https://madeup.sharepoint.com/personal/labteam",
        "https://madeup.sharepoint.com/sites/labteam/more",
        "https://madeup.sharepoint.com/sites/..",
        "https://madeup.sharepoint.com/sites/lab%20team",
        "https://madeup.sharepoint.com/sites/labteam?x=1",
        "https://madeup.sharepoint.com/sites/labteam#x",
    ] {
        assert!(TeamSite::from_url_in(&cloud, url).is_none(), "{url}");
    }
}

#[test]
fn an_own_servers_team_site_is_under_its_sp_prefix() {
    let cloud = crate::Cloud::own("https://chat.example:8443").unwrap();
    let site = TeamSite::from_url_in(&cloud, "https://chat.example:8443/sp/sites/team1").unwrap();
    assert_eq!(site.path(), "/sp/sites/team1");
    assert_eq!(site.audience(), "https://chat.example:8443");
    assert!(TeamSite::from_url_in(&cloud, SITE).is_none());
    assert!(TeamSite::from_url_in(&cloud, "https://chat.example:8443/sites/team1").is_none());
    assert!(TeamSite::from_url_in(&crate::Cloud::Microsoft, "https://chat.example:8443/sp/sites/team1").is_none());
}

#[test]
fn the_folder_is_the_channels_own_from_its_thread() {
    let folder = folder();
    assert_eq!(folder.channel(), CHANNEL);
    assert_eq!(folder.path(), "/sites/labteam/Shared Documents/General");
    // the library and the folder name when the thread gives no relative path
    let derived = ChannelFolder::from_thread_in(
        &crate::Cloud::Microsoft,
        CHANNEL,
        &thread(
            &json!({"sharepointSiteUrl": SITE, "sharepointRootLibrary": "Shared Documents",
            "sharepointChannelDocsFolder": "Nick's notes"}),
        ),
    )
    .unwrap();
    assert_eq!(derived.path(), "/sites/labteam/Shared Documents/Nick's notes");
    let cloud = crate::Cloud::Microsoft;
    for properties in [
        json!({"sharepointSiteUrl": SITE, "channelDocsFolderRelativeUrl": "/sites/other/Shared Documents/General"}),
        json!({"sharepointSiteUrl": SITE, "channelDocsFolderRelativeUrl": "/sites/labteam/Shared Documents/../x"}),
        json!({"sharepointSiteUrl": SITE, "channelDocsFolderRelativeUrl": "/sites/labteam/Shared Documents"}),
        json!({"sharepointSiteUrl": SITE, "channelDocsFolderRelativeUrl": "/sites/labteam/Shared Documents/a#b"}),
        json!({"sharepointSiteUrl": "https://madeup-my.sharepoint.com/personal/a", "channelDocsFolderRelativeUrl": "/personal/a/x/y"}),
        json!({"channelDocsFolderRelativeUrl": "/sites/labteam/Shared Documents/General"}),
    ] {
        assert!(
            ChannelFolder::from_thread_in(&cloud, CHANNEL, &thread(&properties)).is_none(),
            "{properties}"
        );
    }
    let other = json!({"id": "19:another@thread.tacv2", "properties": {"sharepointSiteUrl": SITE,
        "channelDocsFolderRelativeUrl": "/sites/labteam/Shared Documents/General"}});
    assert!(ChannelFolder::from_thread_in(&cloud, CHANNEL, &other).is_none());
    assert!(ChannelFolder::from_thread_in(&cloud, "19:chat@thread.v2", &thread(&json!({}))).is_none());
    assert!(ChannelThread::of("19:chat@thread.v2").is_none());
    let read = ChannelThread::of(CHANNEL).unwrap();
    assert_eq!(read.service(), Service::ChatService);
    assert_eq!(read.endpoint().segments(), ["v1", "threads", CHANNEL]);
}

#[test]
fn the_upload_steps_are_the_web_clients_rest_calls() {
    let folder = folder();
    let stub = ChannelFileWrite::add_stub(&folder, "lab notes.txt").unwrap();
    assert_eq!(stub.method(), Method::Post);
    assert!(stub.body().is_empty());
    assert_eq!(stub.file(), None);
    assert_eq!(
        stub.url().as_str(),
        "https://madeup.sharepoint.com/sites/labteam/_api/web/GetFolderByServerRelativeUrl(@v)/Files/addStub(@fn)\
         ?@v=%27%2Fsites%2Flabteam%2FShared%20Documents%2FGeneral%27&@fn=%27lab%20notes.txt%27"
    );
    let start = ChannelFileWrite::start_upload(&folder, FILE, "lab notes.txt", b"abc".to_vec()).unwrap();
    assert_eq!(start.body(), b"abc");
    assert_eq!(start.file(), Some(FILE));
    let url = start.url().as_str();
    assert!(url.starts_with(&format!(
        "{SITE}/_api/web/GetFileById('{FILE}')/StartUpload(uploadId=guid'"
    )));
    assert!(
        url.ends_with("?@folder=%27%2Fsites%2Flabteam%2FShared%20Documents%2FGeneral%27&@file=%27lab%20notes.txt%27")
    );
    let finish = ChannelFileWrite::finish_upload(&start, "lab notes.txt", 3).unwrap();
    assert!(finish.body().is_empty());
    assert!(finish.url().as_str().contains(",fileOffset=3)?@folder="));
    assert!(ChannelFileWrite::finish_upload(&finish, "lab notes.txt", 3).is_none());
    let recycle = ChannelFileWrite::recycle(&folder, FILE).unwrap();
    assert_eq!(recycle.method(), Method::Delete);
    assert_eq!(recycle.step(), &ChannelFileStep::Recycle);
    assert_eq!(
        recycle.url().as_str(),
        format!("{SITE}/_api/web/GetFileById('{FILE}')/recycle")
    );
    // a quote is doubled inside the OData string
    let quoted = ChannelFileWrite::add_stub(&folder, "it's.txt").unwrap();
    assert!(quoted.url().as_str().ends_with("&@fn=%27it%27%27s.txt%27"));
    for name in ["", ".", "..", "a/b.txt", "a#b.txt", "50%.txt", " lead.txt", "tab\t.txt"] {
        assert!(ChannelFileWrite::add_stub(&folder, name).is_none(), "{name:?}");
    }
    assert!(ChannelFileWrite::start_upload(&folder, "not-a-guid", "a.txt", Vec::new()).is_none());
    assert!(ChannelFileWrite::recycle(&folder, "../x").is_none());
    assert_eq!(
        folder.file_read(FILE).unwrap().as_str(),
        format!("{SITE}/_api/web/GetFileById('{FILE}')?$select=ServerRelativeUrl,UniqueId")
    );
}

#[test]
fn the_answered_file_must_be_right_in_the_folder() {
    let folder = folder();
    let answer = |name: &str, path: &str| {
        serde_json::to_vec(&json!({"odata.type": "SP.File", "UniqueId": FILE, "Name": name,
            "ServerRelativeUrl": path, "Length": "40"}))
        .unwrap()
    };
    let file = uploaded_file(
        &folder,
        &answer("a.txt", "/sites/labteam/Shared Documents/General/a.txt"),
    )
    .unwrap();
    assert_eq!(file.name(), "a.txt");
    assert_eq!(file.size(), 40);
    assert_eq!(file.channel(), Some(CHANNEL));
    assert_eq!(file.unique_id(), FILE);
    assert!(
        file.site().is_none(),
        "a channel file is never shared like an own-drive file"
    );
    assert!(file.is_in(&crate::Cloud::Microsoft));
    assert!(!file.is_in(&crate::Cloud::own("https://chat.example").unwrap()));
    for (name, path) in [
        ("a.txt", "/sites/labteam/Shared Documents/Other/a.txt"),
        ("a.txt", "/sites/labteam/Shared Documents/General/sub/a.txt"),
        ("b.txt", "/sites/labteam/Shared Documents/General/a.txt"),
    ] {
        assert!(uploaded_file(&folder, &answer(name, path)).is_none(), "{path}");
    }
    let entry = file.entry();
    assert_eq!(
        entry["fileInfo"]["fileUrl"],
        format!("{SITE}/Shared Documents/General/a.txt")
    );
    assert_eq!(entry["objectUrl"], entry["fileInfo"]["fileUrl"]);
    assert_eq!(entry["baseUrl"], SITE);
    assert_eq!(entry["fileInfo"]["siteUrl"], SITE);
    assert_eq!(entry["fileChicletState"]["serviceName"], "teams");
    assert_eq!(entry["fileInfo"]["shareUrl"], Value::Null);
    assert_eq!(entry["fileInfo"]["itemId"], Value::Null);
    assert_eq!(entry["sharepointIds"], Value::Null);
    assert_eq!(entry["permissionScope"], Value::Null);
    assert_eq!(entry["itemid"], FILE);
    assert_eq!(entry["fileType"], "txt");
    // kept in the outbox and read back the same
    let stored: crate::UploadedFile = serde_json::from_value(serde_json::to_value(&file).unwrap()).unwrap();
    assert_eq!(stored, file);
}

#[test]
fn a_taken_name_is_numbered_as_the_web_client_numbers_it() {
    assert!(is_name_taken(
        br#"{"odata.error":{"code":"-2130575257, Microsoft.SharePoint.SPException","message":{"value":"exists"}}}"#
    ));
    assert!(!is_name_taken(br#"{"odata.error":{"code":"-1, Other"}}"#));
    assert!(!is_name_taken(b"not json"));
    assert_eq!(numbered_name("notes.txt", 1), "notes 1.txt");
    assert_eq!(numbered_name("archive.tar.gz", 2), "archive.tar 2.gz");
    assert_eq!(numbered_name("README", 3), "README 3");
    assert_eq!(numbered_name(".env", 1), ".env 1");
    assert_eq!(
        read_file_path(
            br#"{"UniqueId":"00000000-0000-4000-8000-0000000000F1","ServerRelativeUrl":"/sites/labteam/x"}"#,
            FILE
        ),
        Some("/sites/labteam/x".to_owned())
    );
    assert_eq!(
        read_file_path(br#"{"UniqueId":"other","ServerRelativeUrl":"/x"}"#, FILE),
        None
    );
}
