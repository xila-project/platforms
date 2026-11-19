extern crate alloc;

use alloc::boxed::Box;
use core::{mem::MaybeUninit, panic::PanicInfo, ptr::addr_of_mut};

use esp_println::println;
use esp_storage::FlashStorage;
use xila::{little_fs, memory::ManagerTrait};

#[cfg(feature = "qemu-display")]
mod display;
mod flash;

const HEAP_SIZE: usize = 256 * 1024;
static mut HEAP: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];

drivers_espressif::instantiate_global_allocator!();
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let heap = &__DRIVERS_ESPRESSIF_MEMORY_ALLOCATOR;
    // SAFETY: startup hands this static SRAM exclusively to the allocator.
    assert!(unsafe { heap.init_internal(addr_of_mut!(HEAP).cast(), HEAP_SIZE) });
    println!("Xila ESP32 startup");
    let rng = esp_hal::rng::Rng::new();
    let (first, second) = (rng.random(), rng.random());
    println!("Xila: RNG {first:08x} {second:08x}");
    assert_ne!(first, second, "RNG returned a constant value");

    let flash = Box::leak(Box::new(flash::LittleFsPartition::new(FlashStorage::new(
        peripherals.FLASH,
    ))));
    let options = little_fs::Options::new(256).set_block_cycles(500);
    // Keep existing data: format only when the partition holds no valid file
    // system (a fresh, erased image in the emulator, or a new board).
    let _file_system = little_fs::FileSystem::new(flash, options)
        .or_else(|_| little_fs::FileSystem::new_format(flash, options))
        .expect("Failed to format and mount LittleFS");
    println!("Xila: LittleFS mounted");

    #[cfg(feature = "qemu-display")]
    display::render();

    println!(
        "Xila heap: {} bytes used, {} free, {} total ({}-byte budget)",
        heap.get_used(),
        heap.get_free(),
        heap.get_total_size(),
        HEAP_SIZE
    );
    println!("Xila: boot smoke ready");
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!("Xila panic: {info}");
    loop {
        core::hint::spin_loop();
    }
}
