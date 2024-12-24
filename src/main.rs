#![allow(clippy::single_match)]

use calloop::{channel, timer, EventLoop};
use calloop_wayland_source::WaylandSource;
use cosmic_config::{calloop::ConfigWatchSource, CosmicConfigEntry};
use cosmic_idle_config::CosmicIdleConfig;
use cosmic_settings_config::shortcuts;
use futures_lite::stream::StreamExt;
use std::{process::Command, time::Duration};
use upower_dbus::UPowerProxy;
use wayland_client::{
    delegate_noop,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_compositor, wl_output, wl_registry, wl_seat},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::{
    ext::idle_notify::v1::client::{ext_idle_notification_v1, ext_idle_notifier_v1},
    wp::{
        single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1,
        viewporter::client::{wp_viewport, wp_viewporter},
    },
};
use wayland_protocols_wlr::{
    layer_shell::v1::client::zwlr_layer_shell_v1,
    output_power_management::v1::client::{zwlr_output_power_manager_v1, zwlr_output_power_v1},
};

mod fade_black;
use fade_black::FadeBlackSurface;
mod freedesktop_screensaver;

// Delay between screen off and locking
const LOCK_SCREEN_DELAY: Duration = Duration::from_millis(500);

#[derive(Debug)]
enum Event {
    OnBattery(bool),
    ScreensaverInhibit(bool),
}

type EventSender = channel::Sender<Event>;

struct IdleNotification {
    notification: ext_idle_notification_v1::ExtIdleNotificationV1,
    time: u32,
}

impl IdleNotification {
    fn new(inner: &StateInner, time: u32) -> Self {
        let notification =
            inner
                .idle_notifier
                .get_idle_notification(time, &inner.seat, &inner.qh, ());
        Self { notification, time }
    }
}

impl Drop for IdleNotification {
    fn drop(&mut self) {
        self.notification.destroy();
    }
}

async fn receive_battery_task(sender: EventSender) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let upower = UPowerProxy::new(&connection).await?;
    let mut stream = upower.receive_on_battery_changed().await;
    while let Some(event) = stream.next().await {
        let _ = sender.send(Event::OnBattery(event.get().await?));
    }
    Ok(())
}

#[derive(Debug)]
struct Output {
    output: wl_output::WlOutput,
    output_power: zwlr_output_power_v1::ZwlrOutputPowerV1,
    fade_surface: Option<FadeBlackSurface>,
    global_name: u32,
}

// Immutate references to globals, needed for calls
struct StateInner {
    registry: wl_registry::WlRegistry,
    output_power_manager: zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
    compositor: wl_compositor::WlCompositor,
    layer_shell: zwlr_layer_shell_v1::ZwlrLayerShellV1,
    viewporter: wp_viewporter::WpViewporter,
    single_pixel_buffer_manager: wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
    idle_notifier: ext_idle_notifier_v1::ExtIdleNotifierV1,
    seat: wl_seat::WlSeat,
    qh: QueueHandle<State>,
}

struct State {
    inner: StateInner,
    outputs: Vec<Output>,
    conf: CosmicIdleConfig,
    screen_off_idle_notification: Option<IdleNotification>,
    suspend_idle_notification: Option<IdleNotification>,
    on_battery: bool,
    screensaver_inhibit: bool,
    system_actions: shortcuts::SystemActions,
    loop_handle: calloop::LoopHandle<'static, Self>,
}

fn run_command(command: String) {
    let mut child = match Command::new("/bin/sh").arg("-c").arg(&command).spawn() {
        Ok(child) => child,
        Err(err) => {
            log::error!("failed to execute command '{}': {}", command, err);
            return;
        }
    };

    std::thread::spawn(move || match child.wait() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            log::error!("command '{}' failed with exit status {}", command, status)
        }
        Err(err) => log::error!("failed to wait on command '{}': {}", command, err),
    });
}

impl State {
    fn add_output_global(&mut self, global_name: u32, version: u32) {
        let output = self
            .inner
            .registry
            .bind(global_name, version.min(3), &self.inner.qh, ());
        let output_power =
            self.inner
                .output_power_manager
                .get_output_power(&output, &self.inner.qh, ());
        self.outputs.push(Output {
            output,
            output_power,
            fade_surface: None,
            global_name,
        });
    }

    fn update_screen_off_idle(&mut self, is_idle: bool) {
        for output in &mut self.outputs {
            if is_idle {
                output.fade_surface = Some(FadeBlackSurface::new(&self.inner, &output.output));
            } else {
                output.fade_surface = None;
                output.output_power.set_mode(zwlr_output_power_v1::Mode::On);
            }
        }
    }

    // Fade surfaces on all outputs have finished fading out
    fn fade_done(&mut self) {
        for output in &mut self.outputs {
            output
                .output_power
                .set_mode(zwlr_output_power_v1::Mode::Off);
            output.fade_surface = None;
        }

        let timer = timer::Timer::from_duration(LOCK_SCREEN_DELAY);
        self.loop_handle
            .insert_source(timer, |_, _, state| {
                state.lock_screen();
                timer::TimeoutAction::Drop
            })
            .unwrap();
    }

    fn lock_screen(&self) {
        if let Some(command) = self
            .system_actions
            .get(&shortcuts::action::System::LockScreen)
        {
            crate::run_command(command.to_string());
        }
    }

    fn update_suspend_idle(&mut self, is_idle: bool) {
        if is_idle {
            // TODO: Make command configurable
            run_command("systemctl suspend".to_string());
        }
    }

    // If screen off or suspend idle times have changed, recreate idle notifications.
    fn recreate_notifications(&mut self) {
        let screen_off_time = if self.screensaver_inhibit {
            None
        } else {
            self.conf.screen_off_time
        };

        if self.screen_off_idle_notification.as_ref().map(|x| x.time) != screen_off_time {
            self.screen_off_idle_notification =
                screen_off_time.map(|time| IdleNotification::new(&self.inner, time));
            // Initially not idle; server sends `resumed` only after `idled`
            self.update_screen_off_idle(false);
        }

        let suspend_time = if self.screensaver_inhibit {
            None
        } else if self.on_battery {
            self.conf.suspend_on_battery_time
        } else {
            self.conf.suspend_on_ac_time
        };

        if self.suspend_idle_notification.as_ref().map(|x| x.time) != suspend_time {
            self.suspend_idle_notification =
                suspend_time.map(|time| IdleNotification::new(&self.inner, time));
            // Initially not idle; server sends `resumed` only after `idled`
            self.update_suspend_idle(false);
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::OnBattery(value) => {
                self.on_battery = value;
            }
            Event::ScreensaverInhibit(value) => {
                self.screensaver_inhibit = value;
                self.recreate_notifications();
            }
        }
    }
}

fn main() {
    env_logger::init();

    let connection = Connection::connect_to_env().unwrap();
    let (globals, event_queue) = registry_queue_init::<State>(&connection).unwrap();
    let qh = event_queue.handle();

    let output_power_manager = globals
        .bind::<zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1, _, _>(&qh, 1..=1, ())
        .unwrap();

    let idle_notifier = globals
        .bind::<ext_idle_notifier_v1::ExtIdleNotifierV1, _, _>(&qh, 1..=1, ())
        .unwrap();

    let seat = globals
        .bind::<wl_seat::WlSeat, _, _>(&qh, 1..=1, ())
        .unwrap();
    seat.get_pointer(&qh, ());

    let compositor = globals
        .bind::<wl_compositor::WlCompositor, _, _>(&qh, 1..=1, ())
        .unwrap();

    let layer_shell = globals
        .bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, _, _>(&qh, 1..=4, ())
        .unwrap();

    let viewporter = globals
        .bind::<wp_viewporter::WpViewporter, _, _>(&qh, 1..=1, ())
        .unwrap();

    let single_pixel_buffer_manager = globals
        .bind::<wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1, _, _>(
            &qh,
            1..=1,
            (),
        )
        .unwrap();

    let config = cosmic_config::Config::new("com.system76.CosmicIdle", 1).unwrap();
    let conf = CosmicIdleConfig::get_entry(&config).unwrap_or_else(|(errs, conf)| {
        for err in errs {
            log::error!("Loading config: {}", err);
        }
        conf
    });

    let shortcuts_config = shortcuts::context().unwrap();
    let system_actions = shortcuts::system_actions(&shortcuts_config);

    let mut event_loop: EventLoop<State> = EventLoop::try_new().unwrap();

    let mut state = State {
        inner: StateInner {
            registry: globals.registry().clone(),
            compositor,
            output_power_manager,
            layer_shell,
            viewporter,
            single_pixel_buffer_manager,
            idle_notifier,
            seat,
            qh,
        },
        screen_off_idle_notification: None,
        suspend_idle_notification: None,
        outputs: Vec::new(),
        conf,
        on_battery: false,
        screensaver_inhibit: false,
        system_actions,
        loop_handle: event_loop.handle(),
    };
    globals.contents().with_list(|list| {
        for global in list {
            if global.interface == wl_output::WlOutput::interface().name {
                state.add_output_global(global.name, global.version);
            }
        }
    });
    state.recreate_notifications();

    WaylandSource::new(connection, event_queue)
        .insert(event_loop.handle())
        .unwrap();

    if let Ok(source) = ConfigWatchSource::new(&config) {
        event_loop
            .handle()
            .insert_source(source, |(config, keys), _, state| {
                state.conf.update_keys(&config, &keys);
                state.recreate_notifications();
            })
            .unwrap();
    }

    let (executor, scheduler) = calloop::futures::executor().unwrap();
    let (sender, receiver) = channel::channel();
    let sender_clone = sender.clone();
    scheduler
        .schedule(async move {
            if let Err(err) = receive_battery_task(sender_clone).await {
                log::error!("Getting battery status from upower: {}", err);
            }
        })
        .unwrap();
    scheduler
        .schedule(async move {
            if let Err(err) = freedesktop_screensaver::serve(sender).await {
                log::error!("failed to serve FreeDesktop screensaver interface: {}", err);
            }
        })
        .unwrap();
    event_loop
        .handle()
        .insert_source(executor, |_, _, _| {})
        .unwrap();
    event_loop
        .handle()
        .insert_source(receiver, |event, _, state| {
            if let channel::Event::Msg(event) = event {
                state.handle_event(event);
            }
        })
        .unwrap();

    while let Ok(_) = event_loop.dispatch(None, &mut state) {}
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                if interface == "wl_output" {
                    state.add_output_global(name, version);
                }
            }
            wl_registry::Event::GlobalRemove { name } => {
                state.outputs.retain(|output| output.global_name != name);
            }
            _ => {}
        }
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, ()> for State {
    fn event(
        state: &mut Self,
        notification: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let is_idle = match event {
            ext_idle_notification_v1::Event::Idled => true,
            ext_idle_notification_v1::Event::Resumed => false,
            _ => unreachable!(),
        };
        if state
            .screen_off_idle_notification
            .as_ref()
            .map(|x| &x.notification)
            == Some(notification)
        {
            state.update_screen_off_idle(is_idle);
        } else if state
            .suspend_idle_notification
            .as_ref()
            .map(|x| &x.notification)
            == Some(notification)
        {
            state.update_suspend_idle(is_idle);
        }
    }
}

delegate_noop!(State: ignore wl_output::WlOutput);
delegate_noop!(State: zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1);
delegate_noop!(State: ignore zwlr_output_power_v1::ZwlrOutputPowerV1);
delegate_noop!(State: ext_idle_notifier_v1::ExtIdleNotifierV1);
delegate_noop!(State: ignore wl_seat::WlSeat); // TODO: Capabilties
delegate_noop!(State: zwlr_layer_shell_v1::ZwlrLayerShellV1);
delegate_noop!(State: wp_viewporter::WpViewporter);
delegate_noop!(State: wp_viewport::WpViewport);
delegate_noop!(State: wl_compositor::WlCompositor);
