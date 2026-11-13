//! Freedesktop notification transport, on the explicit Linux session bus. A notice's picture goes as the spec's
//! `image-data` hint (its pixels: no file is written), which servers draw in the icon's place.
use super::*;
use crate::app_icon;
use futures_util::{StreamExt as _, stream::BoxStream};
use zbus::{Connection, proxy::CacheProperties, zvariant::Value};

/// `org.freedesktop.Notifications` on the bus the app is given (see the module docs).
pub struct BusNotifications {
    proxy: zbus::Proxy<'static>,
    app_name: String,
    /// The app's desktop entry, without `.desktop`: the desktop finds the app's name and icon by it.
    desktop_entry: String,
    /// The server reads markup in a body: the text is escaped.
    markup: bool,
}

impl BusNotifications {
    /// The server on `connection`, notifying as `app_name` with the app's icon ([`app_icon::NAME`]) and the desktop
    /// entry `app_id` names, and its activations: each notification's id with the action invoked on it (its body's is
    /// [`DEFAULT_ACTION`]).
    ///
    /// # Errors
    /// The server isn't on the bus, or the subscription to its signals is refused.
    pub async fn start(
        connection: &Connection,
        app_name: &str,
        app_id: &str,
    ) -> zbus::Result<(Self, BoxStream<'static, (u32, String)>)> {
        let proxy: zbus::Proxy<'static> = zbus::proxy::Builder::new(connection)
            .destination(SERVER_NAME)?
            .path(SERVER_PATH)?
            .interface(SERVER_NAME)?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let capabilities: Vec<String> = proxy.call("GetCapabilities", &()).await?;
        let invoked = proxy.receive_signal("ActionInvoked").await?;
        let activations = invoked
            .filter_map(|signal| async move { signal.body().deserialize::<(u32, String)>().ok() })
            .boxed();
        let server = Self {
            proxy,
            app_name: app_name.to_owned(),
            desktop_entry: app_id.to_owned(),
            markup: capabilities.iter().any(|capability| capability == "body-markup"),
        };
        Ok((server, activations))
    }
}

/// The hint a notice's picture goes as.
const IMAGE_DATA: &str = "image-data";

/// `picture` as the `image-data` hint's value, `(iiibiiay)`: width, height, row stride, alpha, bits per sample,
/// channels, the pixels. `None` for one too large to say in it.
#[must_use]
fn image_data(picture: &BannerPicture) -> Option<Value<'static>> {
    let width = i32::try_from(picture.width()).ok()?;
    let height = i32::try_from(picture.height()).ok()?;
    let stride = width.checked_mul(4)?;
    Some(Value::from((
        width,
        height,
        stride,
        true,
        8_i32,
        4_i32,
        picture.rgba().to_vec(),
    )))
}

impl NotificationServer for BusNotifications {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let proxy = self.proxy.clone();
        let app_name = self.app_name.clone();
        let desktop_entry = self.desktop_entry.clone();
        let summary = notice.summary.clone();
        let body = if self.markup {
            escape_markup(&notice.body)
        } else {
            notice.body.clone()
        };
        let call_notice = notice.kind == NoticeKind::Call;
        let meeting_notice = notice.kind == NoticeKind::Meeting;
        let channel = teams_proto::is_team_thread_id(&notice.conversation);
        let image = notice.picture.as_ref().and_then(image_data);
        Box::pin(async move {
            // an incoming call's: Answer and Decline, critical, shown until it is closed (its ring ended)
            let (actions, mut hints, expires) = if call_notice {
                let actions = vec![
                    DEFAULT_ACTION,
                    "Open",
                    ANSWER_ACTION,
                    "Answer",
                    DECLINE_ACTION,
                    "Decline",
                ];
                let hints: HashMap<&str, Value<'_>> = HashMap::from([
                    ("urgency", Value::U8(2)),
                    ("desktop-entry", Value::from(desktop_entry.as_str())),
                ]);
                (actions, hints, 0_i32)
            } else if meeting_notice {
                (meeting_actions(channel), meeting_hints(&desktop_entry), -1_i32)
            } else {
                let hints: HashMap<&str, Value<'_>> = HashMap::from([
                    ("category", Value::from("im.received")),
                    ("urgency", Value::U8(1)),
                    ("desktop-entry", Value::from(desktop_entry.as_str())),
                ]);
                (vec![DEFAULT_ACTION, "Open"], hints, -1_i32)
            };
            if let Some(image) = image {
                hints.insert(IMAGE_DATA, image);
            }
            let call = (
                app_name.as_str(),
                replaces,
                app_icon::NAME,
                summary.as_str(),
                body.as_str(),
                actions,
                hints,
                expires,
            );
            proxy
                .call::<_, _, u32>("Notify", &call)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        let proxy = self.proxy.clone();
        Box::pin(async move {
            // one the server already closed is no matter
            let _ = proxy.call::<_, _, ()>("CloseNotification", &(id,)).await;
        })
    }
}

/// A channel's meeting offers only Join; the unlabeled default keeps its body opening the channel.
fn meeting_actions(channel: bool) -> Vec<&'static str> {
    vec![
        DEFAULT_ACTION,
        if channel { "" } else { "Open chat" },
        JOIN_ACTION,
        "Join",
    ]
}

/// A started meeting's notification's hints: of the app, as a chat's.
fn meeting_hints(desktop_entry: &str) -> HashMap<&'static str, Value<'_>> {
    HashMap::from([
        ("category", Value::from("im.received")),
        ("urgency", Value::U8(1)),
        ("desktop-entry", Value::from(desktop_entry)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use teams_proto::{PastedFormat, PastedPicture};

    #[test]
    fn a_picture_is_its_pixels_in_the_image_data_hint() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([1, 2, 3, 4]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encoded");
        let picture = BannerPicture::of_app(&Arc::new(PastedPicture {
            format: PastedFormat::Png,
            bytes: png,
        }))
        .expect("decoded");
        let value = image_data(&picture).expect("a hint");
        assert_eq!(value.value_signature().to_string(), "(iiibiiay)");
        let (width, height, stride, alpha, bits, channels, pixels): (i32, i32, i32, bool, i32, i32, Vec<u8>) =
            value.try_into().expect("its fields");
        assert_eq!((width, height, stride, alpha, bits, channels), (3, 2, 12, true, 8, 4));
        assert_eq!(pixels, [1, 2, 3, 4].repeat(6));
    }
}
