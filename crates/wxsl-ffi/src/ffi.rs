//! Foreign-memory access and result ownership. No planner/compiler logic lives here.

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::api::{self, Response};

/// Status codes have an explicit 32-bit representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u32)]
pub enum WxslStatus {
    Success = 0,
    InvalidArgument = 1,
    AbiMismatch = 2,
    InvalidDocument = 3,
    CompileError = 4,
    Incompatible = 5,
    InternalError = 6,
}

/// A borrowed byte view, not NUL-terminated. Empty views have a null pointer.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct WxslBytes {
    pub data: *const u8,
    pub len: usize,
}

impl WxslBytes {
    fn view(bytes: &[u8]) -> Self {
        Self {
            data: if bytes.is_empty() {
                std::ptr::null()
            } else {
                bytes.as_ptr()
            },
            len: bytes.len(),
        }
    }
}

/// A computed field: buffer 0=params, 1=user, 2=instance attributes, 3=effect params.
#[repr(C)]
pub struct WxslField {
    pub name: WxslBytes,
    /// Host storage type (`bool` uses `u32`); logical types are in interface JSON.
    pub ty: WxslBytes,
    pub buffer: u32,
    pub group: u32,
    pub binding: u32,
    pub offset: u32,
    pub size: u32,
    pub align: u32,
    pub buffer_size: u32,
    pub buffer_align: u32,
}

/// Opaque to C. All views remain valid until this result is freed.
pub struct WxslResult {
    response: Response,
    fields: Vec<WxslField>,
}

impl WxslResult {
    fn owned(response: Response) -> *mut Self {
        let fields = response
            .fields
            .iter()
            .map(|field| WxslField {
                name: WxslBytes::view(field.name.as_bytes()),
                ty: WxslBytes::view(field.ty.as_bytes()),
                buffer: field.buffer,
                group: field.group,
                binding: field.binding,
                offset: field.offset,
                size: field.size,
                align: field.align,
                buffer_size: field.buffer_size,
                buffer_align: field.buffer_align,
            })
            .collect();
        Box::into_raw(Box::new(Self { response, fields }))
    }
}

/// ABI revision. Compare before using the rest of the generated header.
#[no_mangle]
pub extern "C" fn wxsl_abi_version() -> u32 {
    crate::ABI_VERSION
}

/// Compile and schedule a versioned pipeline document.
/// # Safety
/// `request.data` must reference `request.len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn wxsl_compile_pipeline(
    abi_version: u32,
    request: WxslBytes,
) -> *mut WxslResult {
    // SAFETY: caller supplies the borrowed input span; run validates null/length/version.
    unsafe { run(abi_version, request, api::pipeline) }
}

/// Compile WXSL or a shipped effect to WGSL.
/// # Safety
/// `request.data` must reference `request.len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn wxsl_compile_shader(
    abi_version: u32,
    request: WxslBytes,
) -> *mut WxslResult {
    // SAFETY: same input-span contract as compile_pipeline.
    unsafe { run(abi_version, request, api::shader) }
}

/// Compile a material stage and its computed interface.
/// # Safety
/// `request.data` must reference `request.len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn wxsl_compile_material(
    abi_version: u32,
    request: WxslBytes,
) -> *mut WxslResult {
    // SAFETY: same input-span contract as compile_pipeline.
    unsafe { run(abi_version, request, api::material) }
}

/// Check scene/setup compatibility before allocating GPU objects.
/// # Safety
/// `request.data` must reference `request.len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn wxsl_check_setup(abi_version: u32, request: WxslBytes) -> *mut WxslResult {
    // SAFETY: same input-span contract as compile_pipeline.
    unsafe { run(abi_version, request, api::check) }
}

unsafe fn run(
    version: u32,
    request: WxslBytes,
    operation: fn(&[u8]) -> Response,
) -> *mut WxslResult {
    let response = catch_unwind(AssertUnwindSafe(|| {
        if version != crate::ABI_VERSION {
            return Response::error(
                WxslStatus::AbiMismatch,
                format!(
                    "C ABI version {version} does not match supported C ABI version {}",
                    crate::ABI_VERSION
                ),
            );
        }
        if request.len > isize::MAX as usize || (request.data.is_null() && request.len != 0) {
            return Response::error(
                WxslStatus::InvalidArgument,
                "request has a null pointer or invalid length",
            );
        }
        let bytes = if request.len == 0 {
            &[]
        } else {
            // SAFETY: null/length checked; readable extent and lifetime are the caller's contract.
            unsafe { std::slice::from_raw_parts(request.data, request.len) }
        };
        operation(bytes)
    }))
    .unwrap_or_else(|_| {
        Response::error(
            WxslStatus::InternalError,
            "Rust panic while processing request",
        )
    });
    WxslResult::owned(response)
}

/// Status of a result, or InvalidArgument for null.
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_status(result: *const WxslResult) -> WxslStatus {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }.map_or(WxslStatus::InvalidArgument, |r| r.response.status)
}

/// JSON metadata/diagnostics, or an empty view for null.
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_json(result: *const WxslResult) -> WxslBytes {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }.map_or(WxslBytes::view(&[]), |r| WxslBytes::view(&r.response.json))
}

/// UTF-8 WGSL, or an empty view on failure/null.
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_wgsl(result: *const WxslResult) -> WxslBytes {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }.map_or(WxslBytes::view(&[]), |r| WxslBytes::view(&r.response.wgsl))
}

/// Initial parameter bytes (material or effect), already padded by the computed layout.
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_params(result: *const WxslResult) -> WxslBytes {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }.map_or(WxslBytes::view(&[]), |r| {
        WxslBytes::view(&r.response.params)
    })
}

/// Number of computed field rows (zero for null).
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_field_count(result: *const WxslResult) -> usize {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }.map_or(0, |r| r.fields.len())
}

/// Contiguous computed field rows (null when empty). Borrowed from the result.
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_fields(result: *const WxslResult) -> *const WxslField {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }
        .filter(|r| !r.fields.is_empty())
        .map_or(std::ptr::null(), |r| r.fields.as_ptr())
}

/// Release a result and all its views; null is a no-op.
/// # Safety
/// A non-null pointer must be a live result returned here and freed exactly once.
/// No view/accessor may be used concurrently with or after this call.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_free(result: *mut WxslResult) {
    if !result.is_null() {
        // SAFETY: caller transfers sole ownership back, exactly once.
        drop(unsafe { Box::from_raw(result) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panics_are_results_not_foreign_unwinds() {
        // SAFETY: an empty input borrows no memory; result is read/freed once.
        unsafe {
            let result = run(crate::ABI_VERSION, WxslBytes::view(&[]), |_| {
                panic!("test panic")
            });
            assert_eq!(wxsl_result_status(result), WxslStatus::InternalError);
            wxsl_result_free(result);
        }
    }
}
