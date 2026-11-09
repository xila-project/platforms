#![cfg(feature = "neural-network-cmsis-nn")]
//! Count all Rust heap allocations in the VFS driver path after open.
use drivers_neural_network::{COMPUTE, GET_INPUT_INFO};
use file_system::{BaseOperations, Context, ControlCommand};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! { static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) }; }
struct Counting;
fn record() {
    ALLOCATIONS.with(|count| {
        if let Some(n) = count.get() {
            count.set(Some(n + 1));
        }
    });
}
// SAFETY: all operations forward their original pointer and layout to System.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record();
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
#[repr(align(16))]
struct Aligned<const N: usize>([u8; N]);
static MODEL: Aligned<{ include_bytes!("fixtures/classifier.tflite").len() }> =
    Aligned(*include_bytes!("fixtures/classifier.tflite"));

fn exercise<B: neural_network::Backend>(device: &drivers_neural_network::NeuralNetworkDevice<B>) {
    let mut context = Context::new_empty();
    ALLOCATIONS.with(|count| count.set(Some(0)));
    device.open(&mut context).unwrap();
    let open_allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(
        open_allocations, 3,
        "arena, completion counters and VFS context; no backend wrapper allocation"
    );
    let mut info = neural_network::TensorInfo::default();
    device
        .control_sync(
            &mut context,
            GET_INPUT_INFO::IDENTIFIER,
            shared::AnyByLayout::from(&0u32),
            shared::AnyByLayout::from_mutable(&mut info),
        )
        .unwrap();
    let input = vec![0; info.byte_len as usize];
    let mut output = [0; 64];
    ALLOCATIONS.with(|count| count.set(Some(0)));
    for _ in 0..5 {
        assert_eq!(device.write_sync(&mut context, &input, 0), Ok(input.len()));
        device
            .control_sync(
                &mut context,
                COMPUTE::IDENTIFIER,
                shared::AnyByLayout::NONE,
                shared::AnyByLayout::NONE,
            )
            .unwrap();
        assert!(device.read_sync(&mut context, &mut output, 0).unwrap() > 0);
    }
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(allocations, 0);
    device.close(&mut context).unwrap();
}

#[test]
fn rom_device_open_has_three_allocations_and_inference_has_none() {
    let device =
        drivers_shared::devices::neural_network::from_rom(&MODEL.0, nanoglia::Target::Cpu).unwrap();
    exercise(&device);
}

#[test]
fn heap_device_open_has_three_allocations_and_inference_has_none() {
    let mut model = nanoglia::core::Block::new_zeroed(MODEL.0.len(), nanoglia::Global).unwrap();
    model.as_mut_slice().copy_from_slice(&MODEL.0);
    let device =
        drivers_shared::devices::neural_network::from_owned_model(model, nanoglia::Target::Cpu)
            .unwrap();
    exercise(&device);
}
