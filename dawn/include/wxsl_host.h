/* Generated from wxsl_core::host; do not edit. */
#ifndef WXSL_HOST_H
#define WXSL_HOST_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
#define WXSL_LAYOUT_ASSERT static_assert
#else
#define WXSL_LAYOUT_ASSERT _Static_assert
#endif
typedef struct WxslCameraUniform {
    float view_proj[4][4];
    float inverse_view_proj[4][4];
    float position[3];
    float _padding;
    float previous_view_proj[4][4];
    float previous_position[3];
    float _padding1;
} WxslCameraUniform;
WXSL_LAYOUT_ASSERT(sizeof(WxslCameraUniform) == 224, "CameraUniform size");
WXSL_LAYOUT_ASSERT(offsetof(WxslCameraUniform, view_proj) == 0, "CameraUniform.view_proj offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslCameraUniform, inverse_view_proj) == 64, "CameraUniform.inverse_view_proj offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslCameraUniform, position) == 128, "CameraUniform.position offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslCameraUniform, _padding) == 140, "CameraUniform._padding offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslCameraUniform, previous_view_proj) == 144, "CameraUniform.previous_view_proj offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslCameraUniform, previous_position) == 208, "CameraUniform.previous_position offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslCameraUniform, _padding1) == 220, "CameraUniform._padding1 offset");
typedef struct WxslLightUniform {
    float position_or_direction[3];
    float kind;
    float color[3];
    float intensity;
    float shadow_normal_bias;
    uint32_t tile_first;
    uint32_t tile_count;
    float range;
    float area_u[3];
    float _padding1;
    float area_v[3];
    float _padding2;
    float spot_direction[3];
    float spot_outer_cos;
    float spot_inner_cos;
    uint32_t cookie_layer;
    float _padding3[2];
} WxslLightUniform;
WXSL_LAYOUT_ASSERT(sizeof(WxslLightUniform) == 112, "LightUniform size");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, position_or_direction) == 0, "LightUniform.position_or_direction offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, kind) == 12, "LightUniform.kind offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, color) == 16, "LightUniform.color offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, intensity) == 28, "LightUniform.intensity offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, shadow_normal_bias) == 32, "LightUniform.shadow_normal_bias offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, tile_first) == 36, "LightUniform.tile_first offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, tile_count) == 40, "LightUniform.tile_count offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, range) == 44, "LightUniform.range offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, area_u) == 48, "LightUniform.area_u offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, _padding1) == 60, "LightUniform._padding1 offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, area_v) == 64, "LightUniform.area_v offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, _padding2) == 76, "LightUniform._padding2 offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, spot_direction) == 80, "LightUniform.spot_direction offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, spot_outer_cos) == 92, "LightUniform.spot_outer_cos offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, spot_inner_cos) == 96, "LightUniform.spot_inner_cos offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, cookie_layer) == 100, "LightUniform.cookie_layer offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslLightUniform, _padding3) == 104, "LightUniform._padding3 offset");
typedef struct WxslShadowTileUniform {
    float rect[4];
    float view_proj[4][4];
    float split;
    uint8_t _host_padding3[4];
    float _padding[2];
} WxslShadowTileUniform;
WXSL_LAYOUT_ASSERT(sizeof(WxslShadowTileUniform) == 96, "ShadowTileUniform size");
WXSL_LAYOUT_ASSERT(offsetof(WxslShadowTileUniform, rect) == 0, "ShadowTileUniform.rect offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslShadowTileUniform, view_proj) == 16, "ShadowTileUniform.view_proj offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslShadowTileUniform, split) == 80, "ShadowTileUniform.split offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslShadowTileUniform, _padding) == 88, "ShadowTileUniform._padding offset");
typedef struct WxslSceneUniform {
    WxslLightUniform lights[16];
    WxslShadowTileUniform shadow_tiles[112];
    float ambient_sky[3];
    float environment_enabled;
    float ambient_ground[3];
    float environment_scale;
    uint32_t light_count;
    float time;
    float exposure;
    float previous_time;
    float origin[3];
    uint32_t environment_levels;
    float light_grid_view_proj[4][4];
    uint32_t light_grid[4];
    float light_grid_depth[4];
} WxslSceneUniform;
WXSL_LAYOUT_ASSERT(sizeof(WxslSceneUniform) == 12704, "SceneUniform size");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, lights) == 0, "SceneUniform.lights offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, shadow_tiles) == 1792, "SceneUniform.shadow_tiles offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, ambient_sky) == 12544, "SceneUniform.ambient_sky offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, environment_enabled) == 12556, "SceneUniform.environment_enabled offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, ambient_ground) == 12560, "SceneUniform.ambient_ground offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, environment_scale) == 12572, "SceneUniform.environment_scale offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, light_count) == 12576, "SceneUniform.light_count offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, time) == 12580, "SceneUniform.time offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, exposure) == 12584, "SceneUniform.exposure offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, previous_time) == 12588, "SceneUniform.previous_time offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, origin) == 12592, "SceneUniform.origin offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, environment_levels) == 12604, "SceneUniform.environment_levels offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, light_grid_view_proj) == 12608, "SceneUniform.light_grid_view_proj offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, light_grid) == 12672, "SceneUniform.light_grid offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslSceneUniform, light_grid_depth) == 12688, "SceneUniform.light_grid_depth offset");
typedef struct WxslInstanceTransform {
    float model[4][4];
    float normal_matrix[4][4];
} WxslInstanceTransform;
WXSL_LAYOUT_ASSERT(sizeof(WxslInstanceTransform) == 128, "InstanceTransform size");
WXSL_LAYOUT_ASSERT(offsetof(WxslInstanceTransform, model) == 0, "InstanceTransform.model offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslInstanceTransform, normal_matrix) == 64, "InstanceTransform.normal_matrix offset");
typedef struct WxslVertex {
    float position[3];
    float normal[3];
    float tangent[4];
    float uv[2];
} WxslVertex;
WXSL_LAYOUT_ASSERT(sizeof(WxslVertex) == 48, "Vertex size");
WXSL_LAYOUT_ASSERT(offsetof(WxslVertex, position) == 0, "Vertex.position offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslVertex, normal) == 12, "Vertex.normal offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslVertex, tangent) == 24, "Vertex.tangent offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslVertex, uv) == 40, "Vertex.uv offset");
typedef struct WxslUiInstance {
    float center[2];
    float half_extent[2];
    float axis[2];
    float shape[2];
    float uv_min[2];
    float uv_max[2];
    float color[4];
    uint32_t kind;
} WxslUiInstance;
WXSL_LAYOUT_ASSERT(sizeof(WxslUiInstance) == 68, "UiInstance size");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiInstance, center) == 0, "UiInstance.center offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiInstance, half_extent) == 8, "UiInstance.half_extent offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiInstance, axis) == 16, "UiInstance.axis offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiInstance, shape) == 24, "UiInstance.shape offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiInstance, uv_min) == 32, "UiInstance.uv_min offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiInstance, uv_max) == 40, "UiInstance.uv_max offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiInstance, color) == 48, "UiInstance.color offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiInstance, kind) == 64, "UiInstance.kind offset");
typedef struct WxslGpuEdge {
    float p0[2];
    float p1[2];
    float p2[2];
    float p3[2];
    uint32_t kind;
    uint32_t color;
} WxslGpuEdge;
WXSL_LAYOUT_ASSERT(sizeof(WxslGpuEdge) == 40, "GpuEdge size");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuEdge, p0) == 0, "GpuEdge.p0 offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuEdge, p1) == 8, "GpuEdge.p1 offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuEdge, p2) == 16, "GpuEdge.p2 offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuEdge, p3) == 24, "GpuEdge.p3 offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuEdge, kind) == 32, "GpuEdge.kind offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuEdge, color) == 36, "GpuEdge.color offset");
typedef struct WxslGpuJob {
    float translate[2];
    float scale;
    float range;
    uint32_t size[2];
    uint32_t edge_begin;
    uint32_t edge_end;
    uint32_t pixel_offset;
    uint32_t _pad0;
} WxslGpuJob;
WXSL_LAYOUT_ASSERT(sizeof(WxslGpuJob) == 40, "GpuJob size");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuJob, translate) == 0, "GpuJob.translate offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuJob, scale) == 8, "GpuJob.scale offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuJob, range) == 12, "GpuJob.range offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuJob, size) == 16, "GpuJob.size offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuJob, edge_begin) == 24, "GpuJob.edge_begin offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuJob, edge_end) == 28, "GpuJob.edge_end offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuJob, pixel_offset) == 32, "GpuJob.pixel_offset offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslGpuJob, _pad0) == 36, "GpuJob._pad0 offset");
typedef struct WxslUiViewport {
    float size[2];
    float _pad0[2];
} WxslUiViewport;
WXSL_LAYOUT_ASSERT(sizeof(WxslUiViewport) == 16, "UiViewport size");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiViewport, size) == 0, "UiViewport.size offset");
WXSL_LAYOUT_ASSERT(offsetof(WxslUiViewport, _pad0) == 8, "UiViewport._pad0 offset");
#define WXSL_SHADER_ABI_REVISION 1u
#define WXSL_GROUP_FRAME 0u
#define WXSL_GROUP_MATERIAL 1u
#define WXSL_GROUP_USER 2u
#define WXSL_GROUP_PASS 3u
#define WXSL_BINDING_CAMERA 0u
#define WXSL_BINDING_SCENE 1u
#define WXSL_BINDING_INSTANCES 2u
#define WXSL_BINDING_INSTANCE_ATTRIBUTES 3u
#define WXSL_BINDING_SHADOW_MAPS 4u
#define WXSL_BINDING_SHADOW_SAMPLER 5u
#define WXSL_BINDING_ENVIRONMENT_LUT 6u
#define WXSL_BINDING_LIGHT_COOKIES 11u
#define WXSL_BINDING_POINT_SAMPLER 12u
#define WXSL_BINDING_LOCAL_LIGHTS 13u
#define WXSL_BINDING_LIGHT_GRID 14u
#define WXSL_INSTANCE_TEXTURE_WIDTH 1024u
#define WXSL_BINDING_ENVIRONMENT_DIFFUSE 9u
#define WXSL_BINDING_ENVIRONMENT_SPECULAR 10u
#define WXSL_BINDING_ENVIRONMENT_SAMPLER 7u
#define WXSL_BINDING_PREVIOUS_INSTANCES 8u
#define WXSL_HOST_LAYOUT_ID "5311037746504389822"
#undef WXSL_LAYOUT_ASSERT
#endif
