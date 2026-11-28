use drivers_shared::devices::SharedPointerQueue;
use file_system::{DirectBaseOperations, DirectCharacterDevice, MountOperations, Size};
use graphics::{InputData, Point, State};

pub struct PointerDevice(&'static SharedPointerQueue);

impl PointerDevice {
    pub fn new(queue: &'static SharedPointerQueue) -> Self {
        Self(queue)
    }
}

impl DirectBaseOperations for PointerDevice {
    fn read(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        _: Size,
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| {
            // - Cast the pointer data to the buffer.
            let data: &mut InputData = buffer
                .try_into()
                .map_err(|_| io_error::Kind::InvalidArgument)?;

            // Deliver one queued sample per read; LVGL reads again while `continue` is set, so a
            // press and its release arriving within one frame both reach it.
            let (sample, more) = self.0.next_sample();
            data.set(
                Point::new(sample.x, sample.y),
                if sample.pressed {
                    State::Pressed
                } else {
                    State::Released
                },
            );
            data.set_continue(more);

            Ok(size_of::<InputData>())
        })
    }

    fn write(
        &self,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        _: Size,
    ) -> core::task::Poll<io_error::Result<usize>> {
        file_system::ready(|| {
            if buffer.len() != 6 {
                return Err(io_error::Kind::InvalidArgument);
            }
            let x = i16::from_le_bytes([buffer[1], buffer[2]]);
            let y = i16::from_le_bytes([buffer[3], buffer[4]]);
            let state = State::try_from(buffer[5]).map_err(|_| io_error::Kind::InvalidArgument)?;
            match buffer[0] {
                0 => self.0.move_to(x, y),
                1 => self.0.set_pressed(state == State::Pressed),
                _ => return Err(io_error::Kind::InvalidArgument),
            }
            Ok(buffer.len())
        })
    }
}

impl MountOperations for PointerDevice {}

impl DirectCharacterDevice for PointerDevice {}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(device: &PointerDevice) -> InputData {
        let mut data = InputData::default();
        device.read_sync(data.as_mut(), 0).unwrap();
        data
    }

    #[test]
    fn press_and_release_in_one_frame_are_read_with_continue() {
        static QUEUE: SharedPointerQueue = SharedPointerQueue::new();
        let device = PointerDevice::new(&QUEUE);

        QUEUE.update(40, 50, true);
        QUEUE.update(40, 50, false);

        let press = read(&device);
        assert_eq!(press.state, State::Pressed);
        assert_eq!(press.point, Point::new(40, 50));
        assert!(press.get_continue());

        let release = read(&device);
        assert_eq!(release.state, State::Released);
        assert!(!release.get_continue());
    }

    #[test]
    fn idle_reads_repeat_the_last_state() {
        static QUEUE: SharedPointerQueue = SharedPointerQueue::new();
        let device = PointerDevice::new(&QUEUE);

        QUEUE.update(7, 9, true);
        for _ in 0..3 {
            let data = read(&device);
            assert_eq!(data.state, State::Pressed);
            assert_eq!(data.point, Point::new(7, 9));
            assert!(!data.get_continue());
        }
    }

    #[test]
    fn debug_writes_queue_a_move_and_a_button_change() {
        static QUEUE: SharedPointerQueue = SharedPointerQueue::new();
        let device = PointerDevice::new(&QUEUE);

        let mut move_to = [0u8; 6];
        move_to[1..3].copy_from_slice(&11i16.to_le_bytes());
        move_to[3..5].copy_from_slice(&12i16.to_le_bytes());
        device.write_sync(&move_to, 0).unwrap();
        let mut press = [0u8; 6];
        press[0] = 1;
        press[5] = u8::from(State::Pressed);
        device.write_sync(&press, 0).unwrap();
        press[5] = u8::from(State::Released);
        device.write_sync(&press, 0).unwrap();

        let moved = read(&device);
        assert_eq!(
            (moved.point, moved.state),
            (Point::new(11, 12), State::Released)
        );
        let pressed = read(&device);
        assert_eq!(pressed.state, State::Pressed);
        let released = read(&device);
        assert_eq!(released.state, State::Released);
        assert!(!released.get_continue());
    }
}
