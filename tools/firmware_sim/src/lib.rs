#![no_std]
//! Experimental native NOR reference backend for `firmware_update`.

#[cfg(feature = "std")]
extern crate std;

pub mod nor;
pub mod simulator;
