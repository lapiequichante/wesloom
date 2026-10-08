/* Generated from wxsl-ffi/src/ffi.rs. Do not edit. */
#ifndef WXSL_H
#define WXSL_H
#include <stddef.h>
#include <stdint.h>
#define WXSL_ABI_VERSION 1u
#ifdef __cplusplus
extern "C" {
#endif

typedef struct WxslResult WxslResult;
/* Status codes have an explicit 32-bit representation. */
typedef uint32_t WxslStatus;
enum {
    WXSL_STATUS_SUCCESS = 0,
    WXSL_STATUS_INVALID_ARGUMENT = 1,
    WXSL_STATUS_ABI_MISMATCH = 2,
    WXSL_STATUS_INVALID_DOCUMENT = 3,
    WXSL_STATUS_COMPILE_ERROR = 4,
    WXSL_STATUS_INCOMPATIBLE = 5,
    WXSL_STATUS_INTERNAL_ERROR = 6,
};

/* A borrowed byte view, not NUL-terminated. Empty views have a null pointer. */
typedef struct WxslBytes {
    const uint8_t * data;
    size_t len;
} WxslBytes;

/* A computed field: buffer 0=params, 1=user, 2=instance attributes, 3=effect params. */
typedef struct WxslField {
    WxslBytes name;
/* Host storage type (`bool` uses `u32`); logical types are in interface JSON. */
    WxslBytes ty;
    uint32_t buffer;
    uint32_t group;
    uint32_t binding;
    uint32_t offset;
    uint32_t size;
    uint32_t align;
    uint32_t buffer_size;
    uint32_t buffer_align;
} WxslField;

/* ABI revision. Compare before using the rest of the generated header. */
uint32_t wxsl_abi_version(void);

typedef uint32_t (*wxsl_fn_wxsl_abi_version)(void);

/* Compile and schedule a versioned pipeline document. */
/* # Safety */
/* `request.data` must reference `request.len` readable bytes for the duration of the call. */
WxslResult * wxsl_compile_pipeline(uint32_t abi_version, WxslBytes request);

typedef WxslResult * (*wxsl_fn_wxsl_compile_pipeline)(uint32_t abi_version, WxslBytes request);

/* Compile WXSL or a shipped effect to WGSL. */
/* # Safety */
/* `request.data` must reference `request.len` readable bytes for the duration of the call. */
WxslResult * wxsl_compile_shader(uint32_t abi_version, WxslBytes request);

typedef WxslResult * (*wxsl_fn_wxsl_compile_shader)(uint32_t abi_version, WxslBytes request);

/* Compile a material stage and its computed interface. */
/* # Safety */
/* `request.data` must reference `request.len` readable bytes for the duration of the call. */
WxslResult * wxsl_compile_material(uint32_t abi_version, WxslBytes request);

typedef WxslResult * (*wxsl_fn_wxsl_compile_material)(uint32_t abi_version, WxslBytes request);

/* Check scene/setup compatibility before allocating GPU objects. */
/* # Safety */
/* `request.data` must reference `request.len` readable bytes for the duration of the call. */
WxslResult * wxsl_check_setup(uint32_t abi_version, WxslBytes request);

typedef WxslResult * (*wxsl_fn_wxsl_check_setup)(uint32_t abi_version, WxslBytes request);

/* Status of a result, or InvalidArgument for null. */
/* # Safety */
/* A non-null pointer must be a live result returned by this library. */
WxslStatus wxsl_result_status(const WxslResult * result);

typedef WxslStatus (*wxsl_fn_wxsl_result_status)(const WxslResult * result);

/* JSON metadata/diagnostics, or an empty view for null. */
/* # Safety */
/* A non-null pointer must be a live result returned by this library. */
WxslBytes wxsl_result_json(const WxslResult * result);

typedef WxslBytes (*wxsl_fn_wxsl_result_json)(const WxslResult * result);

/* UTF-8 WGSL, or an empty view on failure/null. */
/* # Safety */
/* A non-null pointer must be a live result returned by this library. */
WxslBytes wxsl_result_wgsl(const WxslResult * result);

typedef WxslBytes (*wxsl_fn_wxsl_result_wgsl)(const WxslResult * result);

/* Initial parameter bytes (material or effect), already padded by the computed layout. */
/* # Safety */
/* A non-null pointer must be a live result returned by this library. */
WxslBytes wxsl_result_params(const WxslResult * result);

typedef WxslBytes (*wxsl_fn_wxsl_result_params)(const WxslResult * result);

/* Number of computed field rows (zero for null). */
/* # Safety */
/* A non-null pointer must be a live result returned by this library. */
size_t wxsl_result_field_count(const WxslResult * result);

typedef size_t (*wxsl_fn_wxsl_result_field_count)(const WxslResult * result);

/* Contiguous computed field rows (null when empty). Borrowed from the result. */
/* # Safety */
/* A non-null pointer must be a live result returned by this library. */
const WxslField * wxsl_result_fields(const WxslResult * result);

typedef const WxslField * (*wxsl_fn_wxsl_result_fields)(const WxslResult * result);

/* Release a result and all its views; null is a no-op. */
/* # Safety */
/* A non-null pointer must be a live result returned here and freed exactly once. */
/* No view/accessor may be used concurrently with or after this call. */
void wxsl_result_free(WxslResult * result);

typedef void (*wxsl_fn_wxsl_result_free)(WxslResult * result);

#ifdef __cplusplus
}
#endif
#endif
