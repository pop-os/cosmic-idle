// https://specifications.freedesktop.org/idle-inhibit-spec/latest

use std::collections::HashMap;

#[derive(Debug)]
struct Inhibitor {
    application_name: String,
    reason_for_inhibit: String,
    client: zbus::names::UniqueName<'static>,
}

#[derive(Default)]
pub struct Screensaver {
    inhibitors: HashMap<u32, Inhibitor>,
    last_cookie: u32,
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl Screensaver {
    fn inhibit(
        &mut self,
        application_name: String,
        reason_for_inhibit: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> u32 {
        self.last_cookie += 1;
        if let Some(sender) = header.sender() {
            self.inhibitors.insert(
                self.last_cookie,
                Inhibitor {
                    application_name,
                    reason_for_inhibit,
                    client: sender.to_owned(),
                },
            );
        }
        self.last_cookie
    }

    fn un_inhibit(&mut self, cookie: u32) {
        self.inhibitors.remove(&cookie);
    }
}

pub async fn serve(conn: &zbus::Connection) -> zbus::Result<()> {
    conn.request_name_with_flags(
        "org.freedesktop.ScreenSaver",
        zbus::fdo::RequestNameFlags::ReplaceExisting.into(),
    )
    .await?;
    conn.object_server()
        .at("/org/freedesktop/ScreenSaver", Screensaver::default())
        .await?;
    Ok(())
}
