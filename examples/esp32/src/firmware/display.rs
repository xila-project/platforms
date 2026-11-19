//! Espressif QEMU virtual RGB device. This register interface is emulator-only.

extern crate alloc;

use alloc::{boxed::Box, vec};
use core::{
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};
use xila::graphics::lvgl::*;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 120;
const BYTES: usize = (WIDTH * HEIGHT * 2) as usize;
const FRAMEBUFFER: *mut u8 = 0x2000_0000 as *mut u8;
const REGISTERS: usize = 0x2100_0000;
const SIZE: usize = 0x04;
const UPDATE_FROM: usize = 0x08;
const UPDATE_TO: usize = 0x0c;
const UPDATE_CONTENT: usize = 0x10;
const UPDATE_STATUS: usize = 0x14;
const BITS_PER_PIXEL: usize = 0x18;
const QEMU_SIGNATURE: *const u32 = 0x6002_63f8 as *const u32;
static FLUSHED: AtomicBool = AtomicBool::new(false);

fn write_register(offset: usize, value: u32) {
    // SAFETY: the caller has checked the ESP32-S3 QEMU signature. Each register
    // is a naturally aligned, 32-bit MMIO location in the virtual RGB device.
    unsafe { ptr::write_volatile((REGISTERS + offset) as *mut u32, value) };
}

unsafe extern "C" fn flush(display: *mut lv_display_t, area: *const lv_area_t, pixels: *mut u8) {
    // SAFETY: LVGL calls with the complete frame in FULL mode, backed by the
    // live, exclusively owned buffer registered below.
    let area = unsafe { &*area };
    if (area.x1, area.y1, area.x2, area.y2) == (0, 0, WIDTH as i32 - 1, HEIGHT as i32 - 1) {
        // QEMU exposes VRAM as ordinary guest RAM, separate from the LVGL buffer.
        unsafe { ptr::copy_nonoverlapping(pixels, FRAMEBUFFER, BYTES) };
        write_register(UPDATE_FROM, 0);
        write_register(UPDATE_TO, (WIDTH << 16) | HEIGHT);
        write_register(UPDATE_CONTENT, FRAMEBUFFER as u32); // A guest address, not an offset.
        write_register(UPDATE_STATUS, 1);
        FLUSHED.store(true, Ordering::Relaxed);
    }
    unsafe { lv_display_flush_ready(display) };
}

pub(super) fn render() {
    // SAFETY: this is an aligned read inside the S3 SYSCON register range.
    assert_eq!(unsafe { ptr::read_volatile(QEMU_SIGNATURE) }, 0x5145_4d55);
    write_register(SIZE, (WIDTH << 16) | HEIGHT);
    write_register(BITS_PER_PIXEL, 16);
    let buffer = Box::leak(vec![0u32; BYTES / 4].into_boxed_slice());
    // SAFETY: startup is the sole caller of LVGL. The display and its buffers
    // live for the rest of the firmware, and the callback completes each flush.
    unsafe {
        lv_init();
        let display = lv_display_create(WIDTH as i32, HEIGHT as i32);
        assert!(!display.is_null());
        lv_display_set_color_format(display, lv_color_format_t_LV_COLOR_FORMAT_RGB565);
        lv_display_set_buffers(
            display,
            buffer.as_mut_ptr().cast(),
            ptr::null_mut(),
            BYTES as u32,
            lv_display_render_mode_t_LV_DISPLAY_RENDER_MODE_FULL,
        );
        lv_display_set_flush_cb(display, Some(flush));
        let label = lv_label_create(lv_display_get_screen_active(display));
        assert!(!label.is_null());
        lv_label_set_text(label, c"Xila / LittleFS ready".as_ptr());
        lv_obj_align(label, lv_align_t_LV_ALIGN_CENTER, 0, 0);
        lv_refr_now(display);
    }
    assert!(
        FLUSHED.load(Ordering::Relaxed),
        "LVGL did not flush a frame"
    );
    esp_println::println!("Xila: LVGL frame flushed");
}
