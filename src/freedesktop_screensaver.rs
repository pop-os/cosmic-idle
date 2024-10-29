// https://specifications.freedesktop.org/idle-inhibit-spec/latest
// https://invent.kde.org/plasma/kscreenlocker/-/blob/master/dbus/org.freedesktop.ScreenSaver.xml

use futures_lite::StreamExt;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex,
};

use crate::{Event, EventSender};

#[derive(Debug)]
pub struct Inhibitor {
    cookie: u32,
    application_name: String,
    reason_for_inhibit: String,
    client: zbus::names::UniqueName<'static>,
}

#[derive(Clone)]
struct Screensaver {
    inhibitors: Arc<Mutex<Vec<Inhibitor>>>,
    last_cookie: Arc<AtomicU32>,
    event_sender: EventSender,
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
            log::info!(
                "Added screensaver inhibitor for application '{}' {:?}, reason: {}, cookie: {}",
                application_name,
                sender,
                reason_for_inhibit,
                cookie
            );
            let mut inhibitors = self.inhibitors.lock().unwrap();
            if inhibitors.is_empty() {
                let _ = self.event_sender.send(Event::ScreensaverInhibit(true));
            }
            inhibitors.push(Inhibitor {
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
            let inhibitor = inhibitors.remove(idx);
            if inhibitors.is_empty() {
                let _ = self.event_sender.send(Event::ScreensaverInhibit(false));
            }
            log::info!(
                "Removed screensaver inhibitor for application '{}' {:?}, reason: {}, cookie: {}",
                inhibitor.application_name,
                inhibitor.client,
                inhibitor.reason_for_inhibit,
                inhibitor.cookie
            );
        }
    }
}

pub async fn serve(conn: &zbus::Connection, event_sender: EventSender) -> zbus::Result<()> {
    let inhibitors = Arc::new(Mutex::new(Vec::new()));

    conn.request_name_with_flags(
        "org.freedesktop.ScreenSaver",
        zbus::fdo::RequestNameFlags::ReplaceExisting.into(),
    )
    .await?;
    let screensaver = Screensaver {
        inhibitors: inhibitors.clone(),
        event_sender: event_sender.clone(),
        last_cookie: Arc::new(AtomicU32::new(0)),
    };
    // Clients vary in which path they use
    let object_server = conn.object_server();
    object_server
        .at("/ScreenSaver", screensaver.clone())
        .await?;
    object_server
        .at("/org/freedesktop/ScreenSaver", screensaver)
        .await?;

    // If a client disconnects from DBus, remove any inhibitors it has added.
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let mut name_owner_stream = dbus.receive_name_owner_changed().await?;
    while let Some(event) = name_owner_stream.next().await {
        let args = event.args()?;
        if args.new_owner.is_none() {
            if let zbus::names::BusName::Unique(name) = args.name {
                let mut inhibitors = inhibitors.lock().unwrap();
                if !inhibitors.is_empty() {
                    inhibitors.retain(|inhibitor| inhibitor.client != name);
                    if inhibitors.is_empty() {
                        let _ = event_sender.send(Event::ScreensaverInhibit(false));
                    }
                }
            }
        }
    }

    Ok(())
}
