use core::fmt;

use log::LoggerTrait;

pub struct Logger;

impl LoggerTrait for Logger {
    fn enabled(&self, level: log::Level) -> bool {
        match level {
            log::Level::Error => true,
            log::Level::Warning => true,
            log::Level::Information => true,
            log::Level::Debug => false,
            log::Level::Trace => false,
        }
    }

    fn write(&self, args: fmt::Arguments) {
        println!("{args}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testing_support::logger;

    static LOGGER: Logger = Logger;

    #[test]
    fn test_write() {
        logger::test_write(&LOGGER);
    }

    #[test]
    fn test_log() {
        logger::test_log(&LOGGER);
    }

    #[test]
    fn test_flush() {
        logger::test_flush(&LOGGER);
    }
}
