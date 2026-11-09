#![cfg(feature = "neural-network-cmsis-nn")]
#[cfg(feature = "neural-network-cmsis-nn")]
use file_system::{BaseOperations, Context};
use file_system::{ControlCommand, ControlCommandIdentifier};
use neural_network_driver::{
    COMPUTE, GET_INPUT_COUNT, GET_INPUT_INFO, GET_INPUT_QUANTIZATION, GET_OUTPUT_COUNT,
    GET_OUTPUT_INFO, GET_OUTPUT_QUANTIZATION, QuantizationEntry, QuantizationRequest,
    SET_INPUT_INDEX, SET_OUTPUT_INDEX,
};

#[repr(C, align(16))]
#[cfg(feature = "neural-network-cmsis-nn")]
struct AlignedModel<const N: usize>([u8; N]);

#[cfg(feature = "neural-network-cmsis-nn")]
static MODEL: &AlignedModel<{ include_bytes!("fixtures/classifier.tflite").len() }> =
    &AlignedModel(*include_bytes!("fixtures/classifier.tflite"));

#[test]
#[cfg(feature = "neural-network-cmsis-nn")]
fn vfs_open_compute_read_and_close_follow_the_character_device_contract() {
    let device =
        drivers_shared::devices::neural_network::from_rom(&MODEL.0, nanoglia::Target::Cpu).unwrap();
    let mut context = Context::new_empty();
    let mut second_context = Context::new_empty();
    BaseOperations::open(&device, &mut context).unwrap();
    BaseOperations::open(&device, &mut second_context).unwrap();
    assert!(context.get_private_data().is_some());

    let mut input_count = 0;
    BaseOperations::control_sync(
        &device,
        &mut context,
        GET_INPUT_COUNT::IDENTIFIER,
        shared::AnyByLayout::NONE,
        shared::AnyByLayout::from_mutable(&mut input_count),
    )
    .unwrap();
    assert_eq!(input_count, 1);

    let mut output = [0; 3];
    assert_eq!(
        BaseOperations::read_sync(&device, &mut context, &mut output, 0),
        Err(file_system::Error::NotInitialized)
    );
    let mut input_info = neural_network::TensorInfo::default();
    BaseOperations::control_sync(
        &device,
        &mut context,
        GET_INPUT_INFO::IDENTIFIER,
        shared::AnyByLayout::from(&0u32),
        shared::AnyByLayout::from_mutable(&mut input_info),
    )
    .unwrap();
    let input = vec![0; input_info.byte_len as usize];
    assert_eq!(
        BaseOperations::write_sync(&device, &mut context, &input[..input.len() / 2], 0),
        Ok(input.len() / 2)
    );
    assert_eq!(
        BaseOperations::control_sync(
            &device,
            &mut context,
            COMPUTE::IDENTIFIER,
            shared::AnyByLayout::NONE,
            shared::AnyByLayout::NONE,
        ),
        Err(file_system::Error::InvalidParameter)
    );
    assert_eq!(
        BaseOperations::write_sync(&device, &mut context, &input, 0),
        Ok(input.len())
    );
    BaseOperations::control_sync(
        &device,
        &mut context,
        COMPUTE::IDENTIFIER,
        shared::AnyByLayout::NONE,
        shared::AnyByLayout::NONE,
    )
    .unwrap();
    assert!(BaseOperations::read_sync(&device, &mut context, &mut output, 0).unwrap() > 0);
    assert!(matches!(
        BaseOperations::clone_context(&device, &context),
        Err(file_system::Error::UnsupportedOperation)
    ));
    BaseOperations::close(&device, &mut context).unwrap();
    assert!(context.get_private_data().is_none());
    assert_eq!(
        BaseOperations::write_sync(&device, &mut second_context, &input, 0),
        Ok(input.len())
    );
    BaseOperations::control_sync(
        &device,
        &mut second_context,
        COMPUTE::IDENTIFIER,
        shared::AnyByLayout::NONE,
        shared::AnyByLayout::NONE,
    )
    .unwrap();
    assert!(BaseOperations::read_sync(&device, &mut second_context, &mut output, 0).unwrap() > 0);
    BaseOperations::close(&device, &mut second_context).unwrap();
}

#[test]
fn command_identifiers_use_the_nn_prefix_and_stable_numbers() {
    assert_eq!(
        GET_INPUT_COUNT::IDENTIFIER,
        ControlCommandIdentifier::new_read::<(), u32>(b'N', 0x01)
    );
    assert_eq!(
        GET_INPUT_INFO::IDENTIFIER,
        ControlCommandIdentifier::new_read::<u32, neural_network::TensorInfo>(b'N', 0x02)
    );
    assert_eq!(
        SET_INPUT_INDEX::IDENTIFIER,
        ControlCommandIdentifier::new_write::<u32, ()>(b'N', 0x03)
    );
    assert_eq!(
        GET_OUTPUT_COUNT::IDENTIFIER,
        ControlCommandIdentifier::new_read::<(), u32>(b'N', 0x04)
    );
    assert_eq!(
        GET_OUTPUT_INFO::IDENTIFIER,
        ControlCommandIdentifier::new_read::<u32, neural_network::TensorInfo>(b'N', 0x05)
    );
    assert_eq!(
        SET_OUTPUT_INDEX::IDENTIFIER,
        ControlCommandIdentifier::new_write::<u32, ()>(b'N', 0x06)
    );
    assert_eq!(
        COMPUTE::IDENTIFIER,
        ControlCommandIdentifier::new_write::<(), ()>(b'N', 0x07)
    );
    assert_eq!(
        GET_INPUT_QUANTIZATION::IDENTIFIER,
        ControlCommandIdentifier::new_read::<QuantizationRequest, QuantizationEntry>(b'N', 0x08)
    );
    assert_eq!(
        GET_OUTPUT_QUANTIZATION::IDENTIFIER,
        ControlCommandIdentifier::new_read::<QuantizationRequest, QuantizationEntry>(b'N', 0x09)
    );
}

#[test]
#[cfg(feature = "neural-network-cmsis-nn")]
fn heap_model_and_open_contexts_outlive_the_device() {
    let source = include_bytes!("fixtures/classifier.tflite");
    let mut model = nanoglia::core::Block::new_zeroed(source.len(), nanoglia::Global).unwrap();
    model.as_mut_slice().copy_from_slice(source);
    let device =
        drivers_shared::devices::neural_network::from_owned_model(model, nanoglia::Target::Cpu)
            .unwrap();
    let mut first = Context::new_empty();
    let mut second = Context::new_empty();
    device.open(&mut first).unwrap();
    device.open(&mut second).unwrap();
    let mut info = neural_network::TensorInfo::default();
    device
        .control_sync(
            &mut first,
            GET_INPUT_INFO::IDENTIFIER,
            shared::AnyByLayout::from(&0u32),
            shared::AnyByLayout::from_mutable(&mut info),
        )
        .unwrap();
    let input = vec![0; info.byte_len as usize];
    for _ in 0..2 {
        assert_eq!(device.write_sync(&mut first, &input, 0), Ok(input.len()));
        device
            .control_sync(
                &mut first,
                COMPUTE::IDENTIFIER,
                shared::AnyByLayout::NONE,
                shared::AnyByLayout::NONE,
            )
            .unwrap();
        let mut output = [0; 64];
        assert!(device.read_sync(&mut first, &mut output, 0).unwrap() > 0);
        assert_eq!(
            device.read_sync(&mut second, &mut output, 0),
            Err(file_system::Error::NotInitialized)
        );
        assert_eq!(
            device.control_sync(
                &mut first,
                COMPUTE::IDENTIFIER,
                shared::AnyByLayout::NONE,
                shared::AnyByLayout::NONE
            ),
            Err(file_system::Error::InvalidParameter)
        );
        assert_eq!(
            device.read_sync(&mut first, &mut output, 0),
            Err(file_system::Error::NotInitialized)
        );
    }
    device.close(&mut first).unwrap();
    drop(device);
    // Remaining Context owns its graph/model handle; ordinary Context drop releases it.
    drop(second);
}

#[test]
#[cfg(feature = "neural-network-cmsis-nn")]
fn context_from_another_device_is_rejected() {
    let first =
        drivers_shared::devices::neural_network::from_rom(&MODEL.0, nanoglia::Target::Cpu).unwrap();
    let second =
        drivers_shared::devices::neural_network::from_rom(&MODEL.0, nanoglia::Target::Cpu).unwrap();
    let mut context = Context::new_empty();
    first.open(&mut context).unwrap();
    assert_eq!(
        second.write_sync(&mut context, &[0], 0),
        Err(file_system::Error::InvalidContext)
    );
    first.close(&mut context).unwrap();
}

#[test]
fn malformed_and_unaligned_rom_models_are_rejected() {
    static MALFORMED: AlignedModel<32> = AlignedModel([0; 32]);
    static UNALIGNED: AlignedModel<33> = AlignedModel([0; 33]);
    assert!(
        drivers_shared::devices::neural_network::from_rom(&MALFORMED.0, nanoglia::Target::Cpu)
            .is_err()
    );
    assert_eq!(
        drivers_shared::devices::neural_network::from_rom(&UNALIGNED.0[1..], nanoglia::Target::Cpu)
            .err(),
        Some(file_system::Error::InvalidParameter)
    );
}
