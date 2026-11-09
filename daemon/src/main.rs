use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::time::Duration;

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, info, warn};
use tracing_subscriber::fmt::time::ChronoLocal;

use mac_trackpad::{
    init::{config, discovery},
    runtime::{
        gesture::GestureMachine, mt_proxy::MtProxy, virtual_trackpad, virtual_wheel::VirtualWheel,
    },
};

/// How often the config file's mtime is checked for hot reload.
const CFG_POLL: Duration = Duration::from_secs(2);
/// Hotplug: how long to keep retrying discovery after the touchpad
/// disappears (device re-enumeration, e.g. around suspend), before
/// giving up and letting the service manager restart us.
const REDISCOVER_ATTEMPTS: u32 = 60;
const REDISCOVER_BACKOFF: Duration = Duration::from_millis(500);
/// Exit status for "no touchpad here (yet)". The unit doesn't restart on
/// it (RestartPreventExitStatus) and doesn't count it as a failure;
/// 99-mac-trackpad.rules starts the service again when a touchpad
/// appears. Restarting on a timer instead burned through systemd's start
/// limit while the pad was unplugged, which then blocked udev's start
/// when it came back, leaving the pad without the daemon until login.
const EXIT_NO_TOUCHPAD: i32 = 75; // EX_TEMPFAIL

struct Args {
    /// Explicit touchpad device path (skips discovery). Mainly for the
    /// integration test harness, but also useful on multi-touchpad
    /// machines.
    device: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args { device: None };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--device" => {
                args.device = Some(
                    iter.next()
                        .ok_or_else(|| "--device requires a path argument".to_string())?,
                );
            }
            "--version" | "-V" => {
                println!("mac-trackpad {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!(
                    "mac-trackpad [--device /dev/input/eventN]\n\n\
                    Turns a sustained 3-finger touchpad touch into a drag \
                    (mouse-button-held movement).\n\n\
                      --device PATH   proxy this evdev device instead of \
                    auto-discovering the touchpad\n\
                      --version       print the version and exit"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    Ok(args)
}

/// Wraps just the raw fd for readiness-polling; the proxy keeps
/// ownership of the actual File.
struct FdWatch(RawFd);
impl AsRawFd for FdWatch {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

/// Sleep until `deadline`, or forever if there is none. Used as a
/// select! arm so gesture-decision deadlines fire exactly on time.
async fn sleep_until_opt(deadline: Option<std::time::Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
        None => std::future::pending::<()>().await,
    }
}

fn is_unplug(err: &io::Error) -> bool {
    err.raw_os_error() == Some(libc::ENODEV)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), io::Error> {
    let args = parse_args().map_err(|e| {
        eprintln!("{e}\nTry --help.");
        io::Error::new(io::ErrorKind::InvalidInput, e)
    })?;

    let configs = config::init_cfg();

    match config::init_file_logger(configs.clone()) {
        Some(logger) => logger.init(),
        None => {
            tracing_subscriber::fmt()
                .with_writer(std::io::stdout)
                .with_max_level(configs.log_level)
                .with_timer(ChronoLocal::rfc_3339())
                .init();
        }
    };

    // Find the touchpad before creating any virtual device, so a start
    // with no touchpad present leaves no hotplug traces behind.
    let first_path = match &args.device {
        Some(p) => p.clone(),
        None => {
            info!("Searching for the trackpad on your device...");
            match discovery::find_real_trackpads() {
                Ok(paths) => pick_first(paths),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    info!("No touchpad yet; exiting until udev starts us when one appears.");
                    std::process::exit(EXIT_NO_TOUCHPAD);
                }
                Err(e) => return Err(e),
            }
        }
    };

    let mut vtrackpad = virtual_trackpad::start_handler()?;
    // created only when a wheel-mode glide is configured (now or on a
    // reload), then kept until exit
    let mut wheel: Option<VirtualWheel> = None;

    // run() holds the real event loop; wrapping it like this guarantees
    // the virtual devices are destroyed on the way out no matter how it
    // returns (including the button being released if a drag was live).
    let result = run(&args, first_path, configs, &mut vtrackpad, &mut wheel).await;

    info!("Cleaning up and exiting...");
    vtrackpad.mouse_up()?; // just in case a drag was in flight
    vtrackpad.destruct()?;
    if let Some(w) = wheel.take() {
        w.destruct()?;
    }
    info!("Clean up successful.");
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound && args.device.is_none() => {
            warn!("{e}; exiting until udev starts us when a touchpad appears.");
            std::process::exit(EXIT_NO_TOUCHPAD);
        }
        other => other,
    }
}

fn pick_first(paths: Vec<String>) -> String {
    if paths.len() > 1 {
        warn!(
            "Found {} touchpads; only proxying the first one ({}).",
            paths.len(),
            paths[0]
        );
    }
    paths[0].clone()
}

/// Creates the momentum wheel device if the configuration wants it and
/// it doesn't exist yet. A failure is logged; wheel glides are then
/// dropped, everything else works.
fn ensure_wheel(cfg: &config::Configuration, wheel: &mut Option<VirtualWheel>) {
    if wheel.is_none() && cfg.wants_wheel_device() {
        match VirtualWheel::new() {
            Ok(w) => {
                info!("Momentum wheel device created (momentumOutput: wheel).");
                *wheel = Some(w);
            }
            Err(e) => warn!("Could not create the momentum wheel device: {e}"),
        }
    }
}

async fn run(
    args: &Args,
    first_path: String,
    mut cfg: config::Configuration,
    vtp: &mut virtual_trackpad::VirtualTrackpad,
    wheel: &mut Option<VirtualWheel>,
) -> Result<(), io::Error> {
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    ensure_wheel(&cfg, wheel);

    let cfg_path = config::get_config_file_path()?;
    let mut cfg_mtime = std::fs::metadata(&cfg_path).and_then(|m| m.modified()).ok();
    let mut cfg_timer = tokio::time::interval(CFG_POLL);
    cfg_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Outer loop: one iteration per (re)acquired touchpad. Re-entered
    // only if the device disappears (ENODEV) and rediscovery succeeds.
    let mut next_path = Some(first_path);
    'device: loop {
        let path = match (next_path.take(), &args.device) {
            (Some(p), _) => p,
            (None, Some(p)) => p.clone(),
            (None, None) => {
                info!("Searching for the trackpad on your device...");
                pick_first(discovery::find_real_trackpads()?)
            }
        };

        let mut proxy = MtProxy::new(&path)?;
        let mut machine = GestureMachine::new(
            cfg.timing(),
            proxy.x_res(),
            proxy.y_res(),
            proxy.slot_count(),
        );
        let watch = AsyncFd::with_interest(FdWatch(proxy.as_raw_fd()), Interest::READABLE)?;

        info!("mac-trackpad started successfully!");

        // Inner loop: fully event-driven. We wake for exactly three
        // reasons: the touchpad has events, a gesture decision deadline
        // arrived, or housekeeping (config reload / shutdown signal).
        let lost_device = loop {
            tokio::select! {
                ready = watch.readable() => {
                    let mut guard = ready?;
                    match proxy.drain(&mut machine, vtp, wheel) {
                        Ok(()) => { guard.clear_ready(); }
                        Err(e) if is_unplug(&e) => break true,
                        Err(e) => return Err(e),
                    }
                }

                _ = sleep_until_opt(machine.next_deadline()) => {
                    let outs = machine.on_tick(std::time::Instant::now());
                    proxy.apply(&outs, vtp, wheel)?;
                }

                _ = cfg_timer.tick() => {
                    let new_mtime = std::fs::metadata(&cfg_path)
                        .and_then(|m| m.modified())
                        .ok();
                    if new_mtime.is_some() && new_mtime != cfg_mtime {
                        cfg_mtime = new_mtime;
                        cfg = config::init_cfg();
                        ensure_wheel(&cfg, wheel);
                        machine.set_timing(cfg.timing());
                        info!("Configuration reloaded (log settings need a restart).");
                    }
                }

                _ = sigterm.recv() => break false,
                _ = sigint.recv() => break false,
            }
        };

        if !lost_device {
            proxy.destruct()?;
            return Ok(());
        }

        // The touchpad vanished (re-enumeration / suspend quirk). Drop
        // the dead handles, release the button if a drag was mid-flight,
        // and try to find it again -- the systemd unit's Restart is the
        // backstop if it never comes back.
        warn!("Touchpad disappeared (ENODEV); attempting rediscovery...");
        if machine.button_held() {
            vtp.mouse_up()?;
        }
        let _ = proxy.destruct();

        if args.device.is_some() {
            // an explicitly given device won't be re-discovered; bail
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "explicitly specified device disappeared",
            ));
        }

        for attempt in 1..=REDISCOVER_ATTEMPTS {
            tokio::time::sleep(REDISCOVER_BACKOFF).await;
            match discovery::find_real_trackpads() {
                Ok(paths) if !paths.is_empty() => {
                    debug!("Touchpad back after {attempt} attempt(s).");
                    next_path = Some(pick_first(paths));
                    continue 'device;
                }
                _ => {}
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "touchpad did not reappear after re-enumeration",
        ));
    }
}
