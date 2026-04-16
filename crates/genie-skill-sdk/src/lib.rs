//! # genie-skill-sdk
//!
//! SDK for building GeniePod Loadable Skill Modules (`.so` shared libraries).
//!
//! Like Linux kernel modules, GeniePod skills are dynamically loaded into the
//! core process at runtime. Each skill exports a `SkillVTable` via C ABI.
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use genie_skill_sdk::prelude::*;
//!
//! skill! {
//!     name: "hello_world",
//!     description: "A simple greeting skill",
//!     version: "0.1.0",
//!     parameters: {
//!         "name" => "string"
//!     },
//!     execute: |args| {
//!         let name = args.get_str("name").unwrap_or("world");
//!         Ok(format!("Hello, {}!", name))
//!     }
//! }
//! ```
//!
//! Build as a cdylib:
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//! ```

mod args;
mod result;
mod vtable;

pub use args::SkillArgs;
pub use result::SkillResult;
pub use vtable::SkillVTable;

/// ABI version. Core will reject skills with a different ABI version.
pub const ABI_VERSION: u32 = 1;

/// Convenience prelude for skill authors.
pub mod prelude {
    pub use crate::{ABI_VERSION, SkillArgs, SkillResult, SkillVTable, skill};
    pub use std::ffi::{CStr, CString, c_char};
}

/// Macro to define a skill with minimal boilerplate.
///
/// Generates the `genie_skill_init` C entry point, vtable,
/// and string lifecycle management.
///
/// # Example
///
/// ```rust,ignore
/// use genie_skill_sdk::prelude::*;
///
/// skill! {
///     name: "my_tool",
///     description: "Does something useful",
///     version: "0.1.0",
///     parameters: {
///         "query" => "string"
///     },
///     execute: |args| {
///         let q = args.get_str("query").unwrap_or("");
///         Ok(format!("Result for: {}", q))
///     }
/// }
/// ```
#[macro_export]
macro_rules! skill {
    (
        name: $name:expr,
        description: $desc:expr,
        version: $ver:expr,
        parameters: { $($param_name:expr => $param_type:expr),* $(,)? },
        execute: |$args:ident| $body:expr
    ) => {
        // Build parameter JSON schema at compile time.
        fn __skill_params_json_ptr() -> *const std::ffi::c_char {
            let mut props = serde_json::Map::new();
            $(
                props.insert(
                    $param_name.to_string(),
                    serde_json::json!({"type": $param_type}),
                );
            )*
            let schema = serde_json::json!({
                "type": "object",
                "properties": props,
            });
            let s = std::ffi::CString::new(schema.to_string())
                .expect("skill parameter schema must not contain interior nulls");
            // Leak into 'static — intentional for C ABI interop.
            s.into_raw() as *const std::ffi::c_char
        }

        fn __skill_execute(
            $args: &$crate::SkillArgs,
        ) -> $crate::SkillResult {
            $body
        }

        // C ABI wrapper for execute.
        extern "C" fn __c_execute(args_json: *const std::ffi::c_char) -> *mut std::ffi::c_char {
            let json_str = if args_json.is_null() {
                "{}"
            } else {
                unsafe { std::ffi::CStr::from_ptr(args_json) }
                    .to_str()
                    .unwrap_or("{}")
            };

            let args = $crate::SkillArgs::from_json(json_str);

            // catch_unwind to prevent panics from crashing the core.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                __skill_execute(&args)
            }));

            let output = match result {
                Ok(Ok(text)) => serde_json::json!({"success": true, "output": text}),
                Ok(Err(e)) => serde_json::json!({"success": false, "output": e}),
                Err(_) => serde_json::json!({"success": false, "output": "skill panicked"}),
            };

            let s = std::ffi::CString::new(output.to_string()).unwrap_or_default();
            s.into_raw()
        }

        // C ABI wrapper to free strings returned by execute.
        extern "C" fn __c_destroy(ptr: *mut std::ffi::c_char) {
            if !ptr.is_null() {
                unsafe { drop(std::ffi::CString::from_raw(ptr)); }
            }
        }

        // Static vtable — lives for the lifetime of the loaded .so.
        static __SKILL_VTABLE: std::sync::LazyLock<$crate::SkillVTable> =
            std::sync::LazyLock::new(|| {
                $crate::SkillVTable {
                    abi_version: $crate::ABI_VERSION,
                    name: concat!($name, "\0").as_ptr() as *const std::ffi::c_char,
                    description: concat!($desc, "\0").as_ptr() as *const std::ffi::c_char,
                    version: concat!($ver, "\0").as_ptr() as *const std::ffi::c_char,
                    parameters_json: __skill_params_json_ptr(),
                    execute: __c_execute,
                    destroy: __c_destroy,
                }
            });

        /// The single entry point — called by genie-core's skill loader.
        #[unsafe(no_mangle)]
        pub extern "C" fn genie_skill_init() -> *const $crate::SkillVTable {
            &*__SKILL_VTABLE as *const $crate::SkillVTable
        }
    };
}
