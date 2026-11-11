//! Xila on the Arduino Nano 33 BLE: a headless system behind the USB serial
//! port, with its root file system in the internal flash.
#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_std, no_main)]

#[cfg(all(target_arch = "arm", target_os = "none"))]
mod board;
#[cfg(all(target_arch = "arm", target_os = "none"))]
mod firmware;
#[cfg(all(target_arch = "arm", target_os = "none"))]
mod pins;

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
fn main() {
    panic!("This example only runs on the Arduino Nano 33 BLE.");
}
