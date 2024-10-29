// https://specifications.freedesktop.org/idle-inhibit-spec/latest
// https://invent.kde.org/plasma/kscreenlocker/-/blob/master/dbus/org.freedesktop.ScreenSaver.xml

use futures_lite::StreamExt;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex,
};

#[derive(Debug)]
struct Inhibitor {
    cookie: u32,
    application_name: String,
    reason_for_inhibit: String,
    client: zbus::names::UniqueName<'static>,
}

pub struct Screensaver {
    inhibitors: Arc<Mutex<Vec<Inhibitor>>>,
    last_cookie: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.ScreenSaver")]
impl Screensaver {
    fn inhibit(
        &mut self,
        application_name: String,
        reason_for_inhibit: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> u32 {
        let cookie = self.last_cookie.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(sender) = header.sender() {
            self.inhibitors.lock().unwrap().push(Inhibitor {
                cookie,
                application_name,
                reason_for_inhibit,
                client: sender.to_owned(),
            });
        }
        cookie
    }

    fn un_inhibit(&mut self, cookie: u32) {
        let mut inhibitors = self.inhibitors.lock().unwrap();
        if let Some(idx) = inhibitors.iter().position(|x| x.cookie == cookie) {
            inhibitors.remove(idx);
        }
    }
}

pub async fn serve(conn: &zbus::Connection) -> zbus::Result<()> {
    let inhibitors = Arc::new(Mutex::new(Vec::new()));

    conn.request_name_with_flags(
        "org.freedesktop.ScreenSaver",
        zbus::fdo::RequestNameFlags::ReplaceExisting.into(),
    )
    .await?;
    conn.object_server()
        .at(
            "/org/freedesktop/ScreenSaver",
            Screensaver {
                inhibitors: inhibitors.clone(),
                last_cookie: AtomicU32::new(0),
            },
        )
        .await?;

    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let mut name_owner_stream = dbus.receive_name_owner_changed().await?;
    while let Some(event) = name_owner_stream.next().await {
        let args = event.args()?;
        if args.new_owner.is_none() {
            if let zbus::names::BusName::Unique(name) = args.name {
                inhibitors
                    .lock()
                    .unwrap()
                    .retain(|inhibitor| inhibitor.client != name);
            }
        }
    }

    Ok(())
}
