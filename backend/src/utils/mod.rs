mod json_utils;
mod sys_utils;
mod hash_utils;
mod compression;
mod file;
mod network;
mod bincode_utils;
mod crypto_utils;
mod step_measure;
mod logging;
mod trakt;
mod serde_utils;

pub use self::logging::*;
pub use self::trakt::*;
pub use self::serde_utils::*;


#[macro_export]
macro_rules! debug_if_enabled {
    ($fmt:expr, $( $args:expr ),*) => {
        if log::log_enabled!(log::Level::Debug) {
            log::log!(log::Level::Debug, $fmt, $($args),*);
        }
    };

    ($txt:expr) => {
        if log::log_enabled!(log::Level::Debug) {
            log::log!(Level::Debug, $txt);
        }
    };
}

#[macro_export]
macro_rules! trace_if_enabled {
    ($fmt:expr, $( $args:expr ),*) => {
        if log::log_enabled!(log::Level::Trace) {
            log::log!(log::Level::Trace, $fmt, $($args),*);
        }
    };

    ($txt:expr) => {
        if log::log_enabled!(log::Level::Trace) {
            log::log!(Level::Trace, $txt);
        }
    };
}

#[macro_export]
macro_rules! with {
    (mut $target:expr => $alias:ident $block:block) => {{
        let $alias = &mut $target;
        $block
    }};
    ($target:expr => $alias:ident $block:block) => {{
        let $alias = &$target;
        $block
    }};
}

pub use debug_if_enabled;
pub use trace_if_enabled;
pub use with;

pub use self::json_utils::*;
pub use self::sys_utils::*;
pub use self::hash_utils::*;
pub use self::compression::*;
pub use self::file::*;
pub use self::network::*;
pub use self::bincode_utils::*;
pub use self::crypto_utils::*;
pub use self::step_measure::*;
