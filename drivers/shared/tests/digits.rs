#![cfg(feature = "neural-network")]
//! The model Digits ships, loaded at runtime through the platform loader node.
extern crate abi_definitions;
extern crate critical_section;
testing::instantiate_global_allocator!();

use drivers_neural_network::{COMPUTE, GET_INPUT_INFO, GET_OUTPUT_INFO, LOAD, LoadRequest};
use file_system::{AccessFlags, Flags, MemoryDevice, Position};
use virtual_file_system::File;

#[task::test]
async fn mnist_digits_run_through_the_portable_vfs_backend() {
    let tasks = Box::leak(Box::new(task::Manager::new()));
    let users = Box::leak(Box::new(users::Manager::new()));
    let time = Box::leak(Box::new(time::Manager::new(&testing::TimeDevice).unwrap()));
    let task = tasks.get_current_task_identifier().await;
    let disk = MemoryDevice::<512>::new_static(512 * 1024);
    little_fs::FileSystem::format(disk, 256).unwrap();
    let fs = little_fs::FileSystem::new(disk, 256).unwrap();
    let vfs = virtual_file_system::initialize(tasks, users, time, fs).unwrap();
    drivers_shared::devices::neural_network::mount_loader(
        &vfs,
        task,
        "/cpu",
        drivers_shared::devices::neural_network::Target::Cpu,
        1024 * 1024,
    )
    .await
    .unwrap();
    let mut file = File::open(&vfs, task, "/cpu", Flags::from(AccessFlags::READ_WRITE))
        .await
        .unwrap();
    assert_eq!(
        file.write(digits::MODEL_BYTES).await.unwrap(),
        digits::MODEL_BYTES.len()
    );
    file.control(LOAD, &LoadRequest::tensorflow_lite())
        .await
        .unwrap();
    let input = file.control(GET_INPUT_INFO, &0).await.unwrap();
    let output = file.control(GET_OUTPUT_INFO, &0).await.unwrap();
    assert_eq!(input.byte_len, 784 * 4);
    assert_eq!(output.byte_len, 10 * 4);
    let samples = include_bytes!("fixtures/mnist-samples.bin");
    // NumPy oracle: the first MNIST "5" is misclassified as "8" by this
    // small model. Preserve that real limitation rather than cherry-pick samples.
    let expected = [0, 1, 2, 3, 4, 8, 6, 7, 8, 9];
    for (digit, sample) in samples.as_chunks::<784>().0.iter().enumerate() {
        let mut bytes = [0; 784 * 4];
        for (&value, chunk) in sample.iter().zip(bytes.as_chunks_mut::<4>().0) {
            chunk.copy_from_slice(&(value as f32 / 255.0).to_le_bytes());
        }
        file.set_position(&Position::Start(0)).await.unwrap();
        assert_eq!(file.write(&bytes).await.unwrap(), bytes.len());
        file.control(COMPUTE, &()).await.unwrap();
        file.set_position(&Position::Start(0)).await.unwrap();
        let mut bytes = [0; 40];
        assert_eq!(file.read(&mut bytes).await.unwrap(), 40);
        let scores = core::array::from_fn(|i| {
            f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        });
        assert_eq!(
            digits::top_three(scores).unwrap()[0].0 as usize,
            expected[digit]
        );
    }
    drop(file);
    vfs.unmount("/cpu", false).await.unwrap();
}
