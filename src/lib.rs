#![no_std]

extern crate alloc;

#[cfg(feature = "esp")]
pub mod app;
pub mod board;
pub mod display;
pub mod drivers;
#[cfg(feature = "esp")]
pub mod input;
pub mod scenes;
pub mod time;
pub mod trig;
