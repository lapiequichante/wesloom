//! Foreign-memory access and result ownership. No planner/compiler logic lives here.

use std::panic::{catch_unwind, AssertUnwindSafe};

use crate::api::{self, Response};

/// Status codes have an explicit 32-bit representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u32)]
pub enum WxslStatus {
    /// The call succeeded; `data` holds its output.
    Success = 0,
    /// A null or oversized input span, or a null result.
    InvalidArgument = 1,
    /// The caller's ABI version is not this library's; nothing was read.
    AbiMismatch = 2,
    /// The request or a document in it did not parse or validate.
    InvalidDocument = 3,
    /// WXSL, codegen or pipeline compilation failed; `data.message` says why.
    CompileError = 4,
    /// The scene does not fit the setup; `data.incompatibilities` lists every mismatch.
    Incompatible = 5,
    /// A panic was contained at the boundary. A bug in this library.
    InternalError = 6,
}

/// A borrowed byte view, not NUL-terminated. Empty views have a null pointer.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct WxslBytes {
    /// First byte, or null when `len` is zero.
    pub data: *const u8,
    /// Length in bytes.
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
    /// Field name, as the graph or effect declared it.
    pub name: WxslBytes,
    /// Host storage type (`bool` uses `u32`); logical types are in interface JSON.
    pub ty: WxslBytes,
    /// Which buffer: 0 material params, 1 user block, 2 instance attributes, 3 effect params.
    pub buffer: u32,
    /// Bind group of the enclosing buffer.
    pub group: u32,
    /// Binding of the enclosing buffer within its group.
    pub binding: u32,
    /// Byte offset of the field in its buffer.
    pub offset: u32,
    /// Byte size of the field (WGSL padding included, e.g. 48 for `mat3x3f`).
    pub size: u32,
    /// Alignment of the field in bytes.
    pub align: u32,
    /// Byte size of the whole enclosing buffer.
    pub buffer_size: u32,
    /// Alignment of the whole enclosing buffer.
    pub buffer_align: u32,
}

/// One imported mesh (`wxsl_import_gltf`), as views into its result.
#[repr(C)]
pub struct WxslMesh {
    /// `WxslVertex` rows (`wxsl_host.h`), 48 bytes each.
    pub vertices: WxslBytes,
    /// `uint32_t` triangle indices, three per triangle.
    pub indices: WxslBytes,
    /// `float[4]` per vertex from `COLOR_0`, or empty.
    pub colors: WxslBytes,
    /// `float[2]` per vertex from `TEXCOORD_1`, or empty.
    pub uv1: WxslBytes,
}

/// Opaque to C. All views remain valid until this result is freed.
pub struct WxslResult {
    response: Response,
    fields: Vec<WxslField>,
    meshes: Vec<WxslMesh>,
    images: Vec<WxslBytes>,
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
        let (meshes, images) = match &response.import {
            Some(import) => (&import.meshes[..], &import.images[..]),
            None => (&[][..], &[][..]),
        };
        let meshes = meshes
            .iter()
            .map(|mesh| WxslMesh {
                vertices: WxslBytes::view(&mesh.vertices),
                indices: WxslBytes::view(&mesh.indices),
                colors: WxslBytes::view(&mesh.colors),
                uv1: WxslBytes::view(&mesh.uv1),
            })
            .collect();
        let images = images.iter().map(|image| WxslBytes::view(image)).collect();
        Box::into_raw(Box::new(Self {
            response,
            fields,
            meshes,
            images,
        }))
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

/// Import a glTF or GLB file: `{"path": "..."}` in, the scene document as
/// `data.scene` in the JSON, its meshes and embedded images as views.
/// # Safety
/// `request.data` must reference `request.len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn wxsl_import_gltf(abi_version: u32, request: WxslBytes) -> *mut WxslResult {
    // SAFETY: same input-span contract as compile_pipeline.
    unsafe { run(abi_version, request, api::import_gltf) }
}

/// Import a GLB from its bytes, as `wxsl_import_gltf` does from a file.
/// # Safety
/// `glb.data` must reference `glb.len` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn wxsl_import_glb(abi_version: u32, glb: WxslBytes) -> *mut WxslResult {
    // SAFETY: same input-span contract as compile_pipeline.
    unsafe { run(abi_version, glb, api::import_glb) }
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

/// Number of imported meshes (zero for null or a non-import result).
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_mesh_count(result: *const WxslResult) -> usize {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }.map_or(0, |r| r.meshes.len())
}

/// Contiguous imported meshes, indexed by the scene's `embedded` mesh
/// sources (null when empty). Borrowed from the result.
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_meshes(result: *const WxslResult) -> *const WxslMesh {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }
        .filter(|r| !r.meshes.is_empty())
        .map_or(std::ptr::null(), |r| r.meshes.as_ptr())
}

/// Number of embedded images (zero for null or a non-import result).
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_image_count(result: *const WxslResult) -> usize {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }.map_or(0, |r| r.images.len())
}

/// Contiguous encoded images (PNG or JPEG), indexed by the scene's
/// `embedded` texture sources (null when empty). Borrowed from the result.
/// # Safety
/// A non-null pointer must be a live result returned by this library.
#[no_mangle]
pub unsafe extern "C" fn wxsl_result_images(result: *const WxslResult) -> *const WxslBytes {
    // SAFETY: caller owns a live result, or passes null.
    unsafe { result.as_ref() }
        .filter(|r| !r.images.is_empty())
        .map_or(std::ptr::null(), |r| r.images.as_ptr())
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
