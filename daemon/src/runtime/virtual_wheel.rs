//! The momentum wheel: a dedicated uinput device for wheel-mode glides
//! (`momentumOutput: "wheel"`). Only created when that mode is wanted.
//!
//! Why its own device rather than adding wheels to the drag pointer
//! (`virtual_trackpad`):
//!
//! * uinput capabilities are fixed at creation. Wheels on the drag
//!   pointer would mean either having them always (the default
//!   configuration would no longer leave that device as it was) or
//!   recreating the pointer on a hot reload, possibly with its button
//!   held mid-drag.
//! * Capabilities decide how udev, libinput and KWin treat a device. A
//!   wheel gives a pointer new per-device settings (natural scrolling,
//!   scroll speed) and a wheel state machine in libinput; keeping them
//!   off the drag pointer means pointer motion and the drag button go
//!   through exactly the same path as before, whatever the wheel does.
//! * Its own identity gives it its own KWin settings: natural scrolling
//!   or scroll speed can be set for the glide alone, without touching
//!   real mice.
//!
//! It is shaped like an ordinary wheel mouse (REL_X/REL_Y and BTN_LEFT
//! are declared but never sent) because udev only tags a device
//! ID_INPUT_MOUSE, which libinput needs to open it as a pointer, when it
//! has relative X/Y and a mouse button. BUS_VIRTUAL keeps libinput from
//! tagging it an external mouse (which "disable touchpad while a mouse
//! is connected" would act on).

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;

use input_linux::{
    EventKind, EventTime, InputEvent, InputId, Key, RelativeAxis, RelativeEvent,
    SynchronizeEvent, SynchronizeKind, UInputHandle,
};
use libc::O_NONBLOCK;
use tracing::debug;

pub const DEVICE_NAME: &[u8] = b"Momentum wheel (created by mac-trackpad)";

pub struct VirtualWheel {
    handle: UInputHandle<File>,
}

impl VirtualWheel {
    const ZERO: EventTime = EventTime::new(0, 0);

    /// Creates the device. Unlike the clone and the drag pointer this
    /// doesn't wait for the compositor to pick it up: nothing is sent on
    /// it until a flick, and the event loop shouldn't stall on a reload.
    pub fn new() -> Result<Self, std::io::Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(O_NONBLOCK)
            .open("/dev/uinput")?;
        let handle = UInputHandle::new(file);

        handle.set_evbit(EventKind::Key)?;
        handle.set_keybit(Key::ButtonLeft)?;
        handle.set_evbit(EventKind::Relative)?;
        for axis in [
            RelativeAxis::X,
            RelativeAxis::Y,
            RelativeAxis::Wheel,
            RelativeAxis::HorizontalWheel,
            RelativeAxis::WheelHiRes,
            RelativeAxis::HorizontalWheelHiRes,
        ] {
            handle.set_relbit(axis)?;
        }

        let id = InputId {
            bustype: input_linux::sys::BUS_VIRTUAL,
            vendor: 0x1234,
            product: 0x5679,
            version: 0,
        };
        handle.create(&id, DEVICE_NAME, 0, &[])?;
        debug!("Momentum wheel device created.");
        Ok(VirtualWheel { handle })
    }

    /// One wheel frame: hi-res units (120 per notch) plus whole notches
    /// for clients that only read REL_WHEEL/REL_HWHEEL. Signs as evdev:
    /// positive = up / right.
    pub fn scroll(&mut self, v120: i32, h120: i32, v: i32, h: i32) -> Result<(), std::io::Error> {
        let mut events = Vec::with_capacity(5);
        let mut rel = |axis, value: i32| {
            if value != 0 {
                events.push(InputEvent::from(RelativeEvent::new(Self::ZERO, axis, value)).into_raw());
            }
        };
        rel(RelativeAxis::WheelHiRes, v120);
        rel(RelativeAxis::Wheel, v);
        rel(RelativeAxis::HorizontalWheelHiRes, h120);
        rel(RelativeAxis::HorizontalWheel, h);
        if events.is_empty() {
            return Ok(());
        }
        events.push(
            InputEvent::from(SynchronizeEvent::new(Self::ZERO, SynchronizeKind::Report, 0))
                .into_raw(),
        );
        self.handle.write(&events)?;
        Ok(())
    }

    pub fn destruct(self) -> Result<(), std::io::Error> {
        self.handle.dev_destroy()
    }
}
