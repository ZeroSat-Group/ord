macro_rules! conditional_log {
    ($condition:expr, $($arg:tt)*) => {
        if $condition {
            log::info!($($arg)*);
        }
    };
}
