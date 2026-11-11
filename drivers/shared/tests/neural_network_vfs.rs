#![cfg(all(feature = "neural-network-cmsis-nn", target_os = "linux"))]
//! Exercise the VFS API with owned heap models and dynamic runtime loaders.
extern crate abi_definitions;
extern crate critical_section;
testing::instantiate_global_allocator!();
use drivers_neural_network::{COMPUTE, GET_INPUT_INFO, GET_OUTPUT_INFO, LOAD, LoadRequest};
use file_system::{AccessFlags, Flags, MemoryDevice, Position};
use virtual_file_system::File;

#[repr(C, align(16))]
struct AlignedModel<const N: usize>([u8; N]);

static MODEL: &AlignedModel<{ include_bytes!("fixtures/classifier.tflite").len() }> =
    &AlignedModel(*include_bytes!("fixtures/classifier.tflite"));

#[task::test]
async fn owned_model_runs_through_mounted_vfs_device() {
    let tasks = Box::leak(Box::new(task::Manager::new()));
    let users = Box::leak(Box::new(users::Manager::new()));
    let time = Box::leak(Box::new(time::Manager::new(&testing::TimeDevice).unwrap()));
    let task = tasks.get_current_task_identifier().await;
    let disk = MemoryDevice::<512>::new_static(512 * 1024);
    little_fs::FileSystem::format(disk, 256).unwrap();
    let fs = little_fs::FileSystem::new(disk, 256).unwrap();
    let vfs = virtual_file_system::initialize(tasks, users, time, fs).unwrap();
    let bytes = include_bytes!("fixtures/classifier.tflite");
    let mut model = nanoglia::core::Block::new_zeroed(bytes.len(), nanoglia::Global).unwrap();
    model.as_mut_slice().copy_from_slice(bytes);
    drivers_shared::devices::neural_network::mount_owned_model(
        &vfs,
        task,
        "/nn",
        model,
        nanoglia::Target::Cpu,
    )
    .await
    .unwrap();
    let mut first = File::open(&vfs, task, "/nn", Flags::from(AccessFlags::READ_WRITE))
        .await
        .unwrap();
    let mut second = File::open(&vfs, task, "/nn", Flags::from(AccessFlags::READ_WRITE))
        .await
        .unwrap();
    let info = first.control(GET_INPUT_INFO, &0u32).await.unwrap();
    let input = vec![0; info.byte_len as usize];
    for file in [&mut first, &mut second] {
        assert_eq!(file.write(&input).await.unwrap(), input.len());
        file.control(COMPUTE, &()).await.unwrap();
        file.set_position(&Position::Start(0)).await.unwrap();
        let mut output = [0; 64];
        assert!(file.read(&mut output).await.unwrap() > 0);
        assert!(file.control(COMPUTE, &()).await.is_err());
    }
    drop(first);
    drop(second);
    vfs.unmount("/nn", false).await.unwrap();
}

#[task::test]
async fn loader_device_loads_model_in_chunks_and_runs_inference() {
    let tasks = Box::leak(Box::new(task::Manager::new()));
    let users = Box::leak(Box::new(users::Manager::new()));
    let time = Box::leak(Box::new(time::Manager::new(&testing::TimeDevice).unwrap()));
    let task = tasks.get_current_task_identifier().await;
    let disk = MemoryDevice::<512>::new_static(512 * 1024);
    little_fs::FileSystem::format(disk, 256).unwrap();
    let fs = little_fs::FileSystem::new(disk, 256).unwrap();
    let vfs = virtual_file_system::initialize(tasks, users, time, fs).unwrap();

    let fixture = include_bytes!("fixtures/classifier.tflite");

    // 1. Run inference on ROM-mounted reference device to get expected outputs
    drivers_shared::devices::neural_network::mount_rom(
        &vfs,
        task,
        "/nn_rom",
        &MODEL.0,
        nanoglia::Target::Cpu,
    )
    .await
    .unwrap();

    let mut rom_file = File::open(&vfs, task, "/nn_rom", Flags::from(AccessFlags::READ_WRITE))
        .await
        .unwrap();
    let rom_input_info = rom_file.control(GET_INPUT_INFO, &0u32).await.unwrap();
    let rom_output_info = rom_file.control(GET_OUTPUT_INFO, &0u32).await.unwrap();
    let input = vec![0u8; rom_input_info.byte_len as usize];
    assert_eq!(rom_file.write(&input).await.unwrap(), input.len());
    rom_file.control(COMPUTE, &()).await.unwrap();
    rom_file.set_position(&Position::Start(0)).await.unwrap();
    let mut rom_output = [0u8; 64];
    let rom_bytes_read = rom_file.read(&mut rom_output).await.unwrap();
    assert!(rom_bytes_read > 0);
    drop(rom_file);
    vfs.unmount("/nn_rom", false).await.unwrap();

    // 2. Mount dynamic loader device with sufficient limit
    drivers_shared::devices::neural_network::mount_loader(
        &vfs,
        task,
        "/nn_loader",
        nanoglia::Target::Cpu,
        64 * 1024,
    )
    .await
    .unwrap();

    let mut loader_file = File::open(
        &vfs,
        task,
        "/nn_loader",
        Flags::from(AccessFlags::READ_WRITE),
    )
    .await
    .unwrap();

    // Staging handle rejects inference commands before LOAD
    assert_eq!(
        loader_file.control(COMPUTE, &()).await.err(),
        Some(virtual_file_system::Error::FileSystem(
            file_system::Error::InvalidContext
        ))
    );

    // Write fixture in several chunks
    const CHUNK_SIZE: usize = 128;
    for chunk in fixture.chunks(CHUNK_SIZE) {
        assert_eq!(loader_file.write(chunk).await.unwrap(), chunk.len());
    }

    // Transition from Staging to Ready state
    loader_file
        .control(LOAD, &LoadRequest::tensorflow_lite())
        .await
        .unwrap();

    // Subsequent LOAD on Ready handle is rejected
    assert_eq!(
        loader_file
            .control(LOAD, &LoadRequest::tensorflow_lite())
            .await
            .err(),
        Some(virtual_file_system::Error::FileSystem(
            file_system::Error::InvalidContext
        ))
    );

    // Verify input/output metadata match ROM device
    let loader_input_info = loader_file.control(GET_INPUT_INFO, &0u32).await.unwrap();
    assert_eq!(loader_input_info, rom_input_info);
    let loader_output_info = loader_file.control(GET_OUTPUT_INFO, &0u32).await.unwrap();
    assert_eq!(loader_output_info, rom_output_info);

    // Execute inference and compare outputs to ROM reference
    loader_file.set_position(&Position::Start(0)).await.unwrap();
    assert_eq!(loader_file.write(&input).await.unwrap(), input.len());
    loader_file.control(COMPUTE, &()).await.unwrap();
    loader_file.set_position(&Position::Start(0)).await.unwrap();
    let mut loader_output = [0u8; 64];
    let loader_bytes_read = loader_file.read(&mut loader_output).await.unwrap();
    assert_eq!(loader_bytes_read, rom_bytes_read);
    assert_eq!(
        &loader_output[..loader_bytes_read],
        &rom_output[..rom_bytes_read]
    );

    drop(loader_file);
    vfs.unmount("/nn_loader", false).await.unwrap();

    // 3. Oversize rejection test with a small maximum model size
    const SMALL_MAXIMUM: usize = 100;
    drivers_shared::devices::neural_network::mount_loader(
        &vfs,
        task,
        "/nn_small",
        nanoglia::Target::Cpu,
        SMALL_MAXIMUM,
    )
    .await
    .unwrap();

    let mut small_file = File::open(
        &vfs,
        task,
        "/nn_small",
        Flags::from(AccessFlags::READ_WRITE),
    )
    .await
    .unwrap();

    // First write fits within SMALL_MAXIMUM
    assert_eq!(small_file.write(&[0u8; 60]).await.unwrap(), 60);
    // Writing beyond SMALL_MAXIMUM is rejected with FileTooLarge
    assert_eq!(
        small_file.write(&[0u8; 41]).await.err(),
        Some(virtual_file_system::Error::FileSystem(
            file_system::Error::FileTooLarge
        ))
    );

    drop(small_file);
    vfs.unmount("/nn_small", false).await.unwrap();
}
