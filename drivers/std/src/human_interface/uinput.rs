//! Linux `uinput` human interface controller.
//!
//! [`UinputController::open`] opens [`UINPUT_DEVICE_PATH`] and creates one virtual input
//! device with the capabilities of `peripherals::human_interface::REPORT_DESCRIPTOR`.
//! [`HumanInterfaceController::send_report`] maps each report to Linux input events
//! (`mapping`), so the emulation protocol can be tested on a computer without hardware,
//! for example with `evtest`. The computer sends keyboard LED changes back as `EV_LED`
//! events on the same file, which [`HumanInterfaceController::indicators`] reports.
//!
//! Opening `/dev/uinput` needs write access: grant it with the `input` group or a udev
//! rule (see the native example README). When it fails, the caller logs one warning and
//! does not mount the device.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicU8, Ordering};

use device::human_interface::{INDICATOR_CAPS_LOCK, INDICATOR_NUM_LOCK, INDICATOR_SCROLL_LOCK};
use peripherals::human_interface::{
    Error, HumanInterfaceController, REPORT_ID_CONSUMER, REPORT_ID_KEYBOARD, REPORT_ID_MOUSE,
    REPORT_ID_SYSTEM,
};

use super::mapping;

/// Path of the Linux user input interface.
pub const UINPUT_DEVICE_PATH: &str = "/dev/uinput";

/// Name of the virtual input device.
const DEVICE_NAME: &[u8] = b"Xila human interface emulation";

/// Vendor and product of the virtual device: pid.codes community range, test device.
const VENDOR_ID: u16 = 0x1209;
const PRODUCT_ID: u16 = 0x0001;
const BUS_USB: u16 = 0x03;

// uinput ioctl request numbers (linux/uinput.h), computed like the kernel macros:
// direction in bits 30-31, size in bits 16-29, type in bits 8-15, number in bits 0-7.
const IOCTL_NONE: u32 = 0;
const IOCTL_WRITE: u32 = 1;
const UINPUT_IOCTL_BASE: u8 = b'U';

const fn ioctl(direction: u32, number: u8, size: usize) -> libc::c_ulong {
    ((direction << 30)
        | ((size as u32) << 16)
        | ((UINPUT_IOCTL_BASE as u32) << 8)
        | (number as u32)) as libc::c_ulong
}

const UI_DEV_CREATE: libc::c_ulong = ioctl(IOCTL_NONE, 1, 0);
const UI_DEV_DESTROY: libc::c_ulong = ioctl(IOCTL_NONE, 2, 0);
const UI_SET_EVBIT: libc::c_ulong = ioctl(IOCTL_WRITE, 100, core::mem::size_of::<libc::c_int>());
const UI_SET_KEYBIT: libc::c_ulong = ioctl(IOCTL_WRITE, 101, core::mem::size_of::<libc::c_int>());
const UI_SET_RELBIT: libc::c_ulong = ioctl(IOCTL_WRITE, 102, core::mem::size_of::<libc::c_int>());
const UI_SET_LEDBIT: libc::c_ulong = ioctl(IOCTL_WRITE, 105, core::mem::size_of::<libc::c_int>());

/// One virtual input device created through `/dev/uinput`.
///
/// `send_report` diffs every report against the previous state, because a report is the
/// full pressed state: released keys, cleared modifiers and released buttons are sent as
/// releases. While the controller exists, `/dev/uinput` is open and a computer can use the
/// device, so `is_connected` is always `true`.
pub struct UinputController {
    file: File,
    created: bool,
    indicators: AtomicU8,
    modifiers: u8,
    keys: [u8; 6],
    consumer_usage: u16,
    mouse_buttons: u8,
    system_usage: u8,
}

impl UinputController {
    /// Open `/dev/uinput` and create the virtual device. Fails when the path cannot be
    /// opened (for example without write access) or the device cannot be created.
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(UINPUT_DEVICE_PATH)?;
        let mut controller = Self {
            file,
            created: false,
            indicators: AtomicU8::new(0),
            modifiers: 0,
            keys: [0; 6],
            consumer_usage: 0,
            mouse_buttons: 0,
            system_usage: 0,
        };
        controller.register_device()?;
        controller.created = true;
        Ok(controller)
    }

    fn fd(&self) -> libc::c_int {
        self.file.as_raw_fd()
    }

    fn ioctl(&self, request: libc::c_ulong, argument: usize) -> io::Result<()> {
        // SAFETY: the request numbers above match the uinput ioctls and the argument is a
        // valid pointer to the expected type.
        let result = unsafe { libc::ioctl(self.fd(), request, argument) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Register the event types, key, relative and LED bits of the shared report descriptor
    /// and create the virtual device.
    fn register_device(&self) -> io::Result<()> {
        self.ioctl(UI_SET_EVBIT, mapping::EV_KEY as usize)?;
        self.ioctl(UI_SET_EVBIT, mapping::EV_REL as usize)?;
        self.ioctl(UI_SET_EVBIT, mapping::EV_LED as usize)?;
        for code in mapping::all_key_codes() {
            self.ioctl(UI_SET_KEYBIT, code as usize)?;
        }
        for code in [
            mapping::REL_X,
            mapping::REL_Y,
            mapping::REL_WHEEL,
            mapping::REL_HWHEEL,
        ] {
            self.ioctl(UI_SET_RELBIT, code as usize)?;
        }
        for code in [mapping::LED_NUML, mapping::LED_CAPSL, mapping::LED_SCROLLL] {
            self.ioctl(UI_SET_LEDBIT, code as usize)?;
        }

        let mut user_device: libc::uinput_user_dev = unsafe { core::mem::zeroed() };
        let name = DEVICE_NAME;
        user_device.name[..name.len()].copy_from_slice(unsafe {
            core::slice::from_raw_parts(name.as_ptr().cast::<libc::c_char>(), name.len())
        });
        user_device.id = libc::input_id {
            bustype: BUS_USB,
            vendor: VENDOR_ID,
            product: PRODUCT_ID,
            version: 1,
        };
        self.ioctl(
            UI_DEV_CREATE,
            &user_device as *const libc::uinput_user_dev as usize,
        )
    }

    /// Write one input event to the virtual device.
    fn emit(&self, type_: u16, code: u16, value: i32) -> io::Result<()> {
        let event = libc::input_event {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_,
            code,
            value,
        };
        // SAFETY: `event` is a valid input_event for the write length.
        let written = unsafe {
            libc::write(
                self.fd(),
                &event as *const libc::input_event as *const libc::c_void,
                size_of::<libc::input_event>(),
            )
        };
        if written != size_of::<libc::input_event>() as isize {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn emit_key(&self, code: u16, pressed: bool) -> io::Result<()> {
        self.emit(mapping::EV_KEY, code, i32::from(pressed))
    }

    fn emit_relative(&self, code: u16, value: i8) -> io::Result<()> {
        if value != 0 {
            self.emit(mapping::EV_REL, code, i32::from(value))?;
        }
        Ok(())
    }

    fn synchronize(&self) -> io::Result<()> {
        self.emit(mapping::EV_SYN, mapping::SYN_REPORT, 0)
    }

    /// Keyboard report (identifier 1): modifiers, reserved byte, six key usages.
    fn send_keyboard_report(&mut self, report: &[u8]) -> Result<(), Error> {
        if report.len() != 8 {
            return Err(Error::Other);
        }
        let modifiers = report[0];
        let keys: [u8; 6] = report[2..].try_into().expect("six key bytes");
        let result = (|| {
            for bit in 0..8 {
                if (self.modifiers ^ modifiers) & (1 << bit) != 0 {
                    self.emit_key(
                        mapping::MODIFIER_KEY_CODES[bit],
                        modifiers & (1 << bit) != 0,
                    )?;
                }
            }
            for usage in self.keys {
                if usage != 0
                    && !keys.contains(&usage)
                    && let Some(code) = mapping::keyboard_usage(usage)
                {
                    self.emit_key(code, false)?;
                }
            }
            for usage in keys {
                if usage != 0
                    && !self.keys.contains(&usage)
                    && let Some(code) = mapping::keyboard_usage(usage)
                {
                    self.emit_key(code, true)?;
                }
            }
            self.synchronize()
        })();
        self.modifiers = modifiers;
        self.keys = keys;
        result.map_err(|_| Error::Other)
    }

    /// Consumer control report (identifier 2): one 16-bit usage, little endian, 0 = release.
    fn send_consumer_report(&mut self, report: &[u8]) -> Result<(), Error> {
        if report.len() != 2 {
            return Err(Error::Other);
        }
        let usage = u16::from_le_bytes([report[0], report[1]]);
        let result = (|| {
            if self.consumer_usage != usage {
                if let Some(code) = mapping::consumer_usage(self.consumer_usage) {
                    self.emit_key(code, false)?;
                }
                if let Some(code) = mapping::consumer_usage(usage) {
                    self.emit_key(code, true)?;
                }
                self.synchronize()?;
            }
            Ok(())
        })();
        self.consumer_usage = usage;
        result.map_err(|_: io::Error| Error::Other)
    }

    /// Mouse report (identifier 3): buttons, x, y, wheel, pan.
    fn send_mouse_report(&mut self, report: &[u8]) -> Result<(), Error> {
        if report.len() != 5 {
            return Err(Error::Other);
        }
        let buttons = report[0];
        let result = (|| {
            for bit in 0..3 {
                if (self.mouse_buttons ^ buttons) & (1 << bit) != 0 {
                    self.emit_key(mapping::MOUSE_BUTTON_CODES[bit], buttons & (1 << bit) != 0)?;
                }
            }
            self.emit_relative(mapping::REL_X, report[1] as i8)?;
            self.emit_relative(mapping::REL_Y, report[2] as i8)?;
            self.emit_relative(mapping::REL_WHEEL, report[3] as i8)?;
            self.emit_relative(mapping::REL_HWHEEL, report[4] as i8)?;
            self.synchronize()
        })();
        self.mouse_buttons = buttons;
        result.map_err(|_| Error::Other)
    }

    /// System control report (identifier 4): one usage, 0 = release.
    fn send_system_report(&mut self, report: &[u8]) -> Result<(), Error> {
        if report.len() != 1 {
            return Err(Error::Other);
        }
        let usage = report[0];
        let result = (|| {
            if self.system_usage != usage {
                if let Some(code) = mapping::system_usage(self.system_usage) {
                    self.emit_key(code, false)?;
                }
                if let Some(code) = mapping::system_usage(usage) {
                    self.emit_key(code, true)?;
                }
                self.synchronize()?;
            }
            Ok(())
        })();
        self.system_usage = usage;
        result.map_err(|_: io::Error| Error::Other)
    }

    /// Read the pending events of the virtual device and update the keyboard indicator
    /// bitmask from the `EV_LED` events the computer sends back.
    fn poll_indicators(&self) {
        let mut events = [libc::input_event {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_: 0,
            code: 0,
            value: 0,
        }; 16];
        loop {
            // SAFETY: `events` is a valid buffer for the read length.
            let read = unsafe {
                libc::read(
                    self.fd(),
                    events.as_mut_ptr() as *mut libc::c_void,
                    size_of::<libc::input_event>() * events.len(),
                )
            };
            if read <= 0 {
                break;
            }
            let count = read as usize / size_of::<libc::input_event>();
            for event in &events[..count] {
                if event.type_ == mapping::EV_LED {
                    let bit = match event.code {
                        mapping::LED_NUML => INDICATOR_NUM_LOCK,
                        mapping::LED_CAPSL => INDICATOR_CAPS_LOCK,
                        mapping::LED_SCROLLL => INDICATOR_SCROLL_LOCK,
                        _ => 0,
                    };
                    if bit != 0 {
                        let current = self.indicators.load(Ordering::Relaxed);
                        let updated = if event.value != 0 {
                            current | bit
                        } else {
                            current & !bit
                        };
                        self.indicators.store(updated, Ordering::Relaxed);
                    }
                }
            }
            if read as usize != size_of::<libc::input_event>() * events.len() {
                break;
            }
        }
    }
}

impl HumanInterfaceController for UinputController {
    fn send_report(&mut self, identifier: u8, report: &[u8]) -> Result<(), Error> {
        match identifier {
            REPORT_ID_KEYBOARD => self.send_keyboard_report(report),
            REPORT_ID_CONSUMER => self.send_consumer_report(report),
            REPORT_ID_MOUSE => self.send_mouse_report(report),
            REPORT_ID_SYSTEM => self.send_system_report(report),
            _ => Err(Error::Other),
        }
    }

    fn is_connected(&self) -> bool {
        // The virtual device exists while /dev/uinput is open.
        true
    }

    fn indicators(&self) -> u8 {
        self.poll_indicators();
        self.indicators.load(Ordering::Relaxed)
    }
}

impl Drop for UinputController {
    fn drop(&mut self) {
        if self.created {
            let _ = self.ioctl(UI_DEV_DESTROY, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A controller whose file descriptor points to `/dev/null`: every path tested here
    /// fails before an event is emitted, reads return end of file, and `Drop` skips
    /// `UI_DEV_DESTROY` because `created` is false.
    fn controller_without_device() -> UinputController {
        UinputController {
            file: File::open("/dev/null").unwrap(),
            created: false,
            indicators: AtomicU8::new(0),
            modifiers: 0,
            keys: [0; 6],
            consumer_usage: 0,
            mouse_buttons: 0,
            system_usage: 0,
        }
    }

    #[test]
    fn ioctl_requests_match_the_kernel_macros() {
        // _IOW('U', 100, int) with _IOC_WRITE = 1 and size 4.
        assert_eq!(
            UI_SET_EVBIT,
            (1u64 << 30) | (4 << 16) | ((b'U' as u64) << 8) | 100
        );
        assert_eq!(
            UI_SET_KEYBIT,
            (1u64 << 30) | (4 << 16) | ((b'U' as u64) << 8) | 101
        );
        assert_eq!(
            UI_SET_RELBIT,
            (1u64 << 30) | (4 << 16) | ((b'U' as u64) << 8) | 102
        );
        assert_eq!(
            UI_SET_LEDBIT,
            (1u64 << 30) | (4 << 16) | ((b'U' as u64) << 8) | 105
        );
        assert_eq!(UI_DEV_CREATE, ((b'U' as u64) << 8) | 1);
        assert_eq!(UI_DEV_DESTROY, ((b'U' as u64) << 8) | 2);
    }

    #[test]
    fn reports_with_a_wrong_length_are_rejected() {
        let mut controller = controller_without_device();
        assert_eq!(
            controller.send_report(REPORT_ID_KEYBOARD, &[0u8; 7]),
            Err(Error::Other)
        );
        assert_eq!(
            controller.send_report(REPORT_ID_CONSUMER, &[0u8; 3]),
            Err(Error::Other)
        );
        assert_eq!(
            controller.send_report(REPORT_ID_MOUSE, &[0u8; 4]),
            Err(Error::Other)
        );
        assert_eq!(
            controller.send_report(REPORT_ID_SYSTEM, &[0u8; 2]),
            Err(Error::Other)
        );
    }

    #[test]
    fn unknown_report_identifier_is_an_error() {
        let mut controller = controller_without_device();
        assert_eq!(controller.send_report(0, &[]), Err(Error::Other));
        assert_eq!(controller.send_report(5, &[]), Err(Error::Other));
    }

    #[test]
    fn a_created_controller_reports_connected() {
        let controller = controller_without_device();
        assert!(controller.is_connected());
        assert_eq!(controller.indicators(), 0);
    }
}
