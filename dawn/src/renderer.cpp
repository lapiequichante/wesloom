#include "renderer.hpp"
#include "compiler.hpp"
#include "wxsl_host.h"
#include <algorithm>
#include <array>
#include <bit>
#include <cmath>
#include <optional>
#include <cstring>
#include <fstream>
#include <map>
#include <mutex>
#include <set>
#include <sstream>
#include <stdexcept>
#include <webgpu/webgpu_cpp.h>

namespace wxsl {
namespace {
using Json = nlohmann::json;
namespace gpu = wgpu;
static_assert(std::endian::native == std::endian::little, "offline uploads are little-endian");

std::vector<uint8_t> read(const std::filesystem::path &root, const std::string &name) {
  const std::filesystem::path relative(name);
  if (relative.is_absolute())
    throw std::runtime_error("asset path must be relative: " + name);
  for (const auto &part : relative)
    if (part == "..")
      throw std::runtime_error("asset path escapes its bundle: " + name);
  std::ifstream input(root / relative, std::ios::binary);
  if (!input)
    throw std::runtime_error("cannot read asset: " + name);
  return {std::istreambuf_iterator<char>(input), std::istreambuf_iterator<char>()};
}
std::string text(gpu::StringView value) {
  return value.data ? std::string(value.data, value.length == WGPU_STRLEN ? std::strlen(value.data)
                                                                          : value.length)
                    : std::string();
}
template <class T>
T named(const std::string &name, std::initializer_list<std::pair<const char *, T>> values) {
  for (const auto &value : values)
    if (name == value.first)
      return value.second;
  throw std::runtime_error("unsupported descriptor value `" + name + "`");
}
gpu::TextureFormat format(const std::string &name) {
  using F = gpu::TextureFormat;
  return named<F>(name, {{"rgba8unorm", F::RGBA8Unorm},
                         {"rgba8unormsrgb", F::RGBA8UnormSrgb},
                         {"bgra8unorm", F::BGRA8Unorm},
                         {"rgba16float", F::RGBA16Float},
                         {"rgba32float", F::RGBA32Float},
                         {"r32float", F::R32Float},
                         {"rg32float", F::RG32Float},
                         {"r8unorm", F::R8Unorm},
                         {"rg8unorm", F::RG8Unorm},
                         {"r16float", F::R16Float},
                         {"rg16float", F::RG16Float},
                         {"r32uint", F::R32Uint},
                         {"rgba8uint", F::RGBA8Uint},
                         {"rgba8sint", F::RGBA8Sint},
                         {"depth16unorm", F::Depth16Unorm},
                         {"depth24plus", F::Depth24Plus},
                         {"depth32float", F::Depth32Float},
                         {"depth24plusstencil8", F::Depth24PlusStencil8},
                         {"depth32floatstencil8", F::Depth32FloatStencil8}});
}
gpu::TextureViewDimension dimension(const std::string &name) {
  using D = gpu::TextureViewDimension;
  return named<D>(name,
                  {{"d2", D::e2D}, {"d2_array", D::e2DArray}, {"cube", D::Cube}, {"d3", D::e3D}});
}
gpu::CompareFunction compare(const std::string &name) {
  using C = gpu::CompareFunction;
  return named<C>(name, {{"never", C::Never},
                         {"less", C::Less},
                         {"equal", C::Equal},
                         {"less_equal", C::LessEqual},
                         {"greater", C::Greater},
                         {"not_equal", C::NotEqual},
                         {"greater_equal", C::GreaterEqual},
                         {"always", C::Always}});
}
gpu::BlendFactor factor(const std::string &name) {
  using B = gpu::BlendFactor;
  return named<B>(name, {{"zero", B::Zero},
                         {"one", B::One},
                         {"src", B::Src},
                         {"one_minus_src", B::OneMinusSrc},
                         {"src_alpha", B::SrcAlpha},
                         {"one_minus_src_alpha", B::OneMinusSrcAlpha},
                         {"dst", B::Dst},
                         {"one_minus_dst", B::OneMinusDst},
                         {"dst_alpha", B::DstAlpha},
                         {"one_minus_dst_alpha", B::OneMinusDstAlpha},
                         {"src_alpha_saturated", B::SrcAlphaSaturated},
                         {"constant", B::Constant},
                         {"one_minus_constant", B::OneMinusConstant}});
}
gpu::BlendComponent component(const Json &value) {
  using O = gpu::BlendOperation;
  gpu::BlendComponent result;
  result.srcFactor = factor(value.at("src_factor"));
  result.dstFactor = factor(value.at("dst_factor"));
  result.operation = named<O>(value.at("operation"), {{"add", O::Add},
                                                      {"subtract", O::Subtract},
                                                      {"reverse_subtract", O::ReverseSubtract},
                                                      {"min", O::Min},
                                                      {"max", O::Max}});
  return result;
}
template <class T, class F> T flags(const std::string &input, T empty, F lookup) {
  T result = empty;
  std::istringstream stream(input);
  std::string name;
  while (std::getline(stream, name, '|')) {
    const auto first = name.find_first_not_of(' '), last = name.find_last_not_of(' ');
    if (first != std::string::npos)
      result |= lookup(name.substr(first, last - first + 1));
  }
  return result;
}
gpu::TextureUsage texture_usage(const std::string &value) {
  using U = gpu::TextureUsage;
  return flags(value, U::None, [](const auto &name) {
    return named<U>(name, {{"COPY_SRC", U::CopySrc},
                           {"COPY_DST", U::CopyDst},
                           {"TEXTURE_BINDING", U::TextureBinding},
                           {"STORAGE_BINDING", U::StorageBinding},
                           {"RENDER_ATTACHMENT", U::RenderAttachment}});
  });
}
gpu::BufferUsage buffer_usage(const std::string &value) {
  using U = gpu::BufferUsage;
  return flags(value, U::None, [](const auto &name) {
    return named<U>(name, {{"COPY_SRC", U::CopySrc},
                           {"COPY_DST", U::CopyDst},
                           {"STORAGE", U::Storage},
                           {"UNIFORM", U::Uniform},
                           {"VERTEX", U::Vertex},
                           {"INDEX", U::Index},
                           {"INDIRECT", U::Indirect},
                           {"MAP_READ", U::MapRead},
                           {"MAP_WRITE", U::MapWrite},
                           {"QUERY_RESOLVE", U::QueryResolve}});
  });
}
gpu::VertexFormat vertex_format(const std::string &value) {
  using F = gpu::VertexFormat;
  return named<F>(value, {{"Float32", F::Float32},
                          {"Float32x2", F::Float32x2},
                          {"Float32x3", F::Float32x3},
                          {"Float32x4", F::Float32x4},
                          {"Uint32", F::Uint32}});
}
gpu::BindGroupLayoutEntry layout_entry(uint32_t index, const Json &value) {
  gpu::BindGroupLayoutEntry entry;
  entry.binding = index;
  entry.visibility =
      gpu::ShaderStage::Vertex | gpu::ShaderStage::Fragment | gpu::ShaderStage::Compute;
  const auto kind = value.at("kind").get<std::string>();
  if (kind == "texture") {
    using S = gpu::TextureSampleType;
    entry.texture.sampleType =
        named<S>(value.at("sample_type"), {{"float", S::Float},
                                           {"unfilterable_float", S::UnfilterableFloat},
                                           {"depth", S::Depth},
                                           {"sint", S::Sint},
                                           {"uint", S::Uint}});
    entry.texture.viewDimension = dimension(value.at("dimension"));
  } else if (kind == "storage_texture") {
    entry.visibility = gpu::ShaderStage::Compute;
    entry.storageTexture.access = gpu::StorageTextureAccess::WriteOnly;
    entry.storageTexture.format = format(value.at("format"));
    entry.storageTexture.viewDimension = dimension(value.at("dimension"));
  } else if (kind == "buffer" || kind == "uniform") {
    const bool uniform = kind == "uniform", readonly = value.value("read_only", true);
    entry.buffer.type = uniform    ? gpu::BufferBindingType::Uniform
                        : readonly ? gpu::BufferBindingType::ReadOnlyStorage
                                   : gpu::BufferBindingType::Storage;
    entry.buffer.minBindingSize = value.at("size");
    if (!uniform && !readonly)
      entry.visibility = gpu::ShaderStage::Compute;
  } else
    throw std::runtime_error("unsupported binding kind `" + kind + "`");
  return entry;
}
struct Resource {
  gpu::Texture texture;
  gpu::Buffer buffer;
  gpu::TextureView view;
  Json shape;
};
struct MaterialGpu {
  gpu::BindGroupLayout layout;
  gpu::BindGroup group;
  gpu::Buffer params, attributes;
  std::map<std::string, gpu::ShaderModule> shaders;
};
struct ErrorState {
  std::mutex mutex;
  std::string message;
};
} // namespace

struct Renderer::Impl {
  ErrorState errors; // Outlives the Dawn handles and their repeated error callback.
  std::filesystem::path root;
  Json data, caps, current;
  gpu::Instance instance;
  gpu::Adapter adapter;
  gpu::Device device;
  gpu::Queue queue;
  gpu::Limits limits;
  gpu::BindGroupLayout frame_layout, empty_layout;
  gpu::Buffer camera, scene, transforms, empty_attributes;
  gpu::Texture lut, checker, fallback;
  gpu::TextureView lut_view, checker_view, fallback_view, environment_placeholder;
  gpu::Sampler checker_sampler, lut_sampler, shadow_sampler;
  std::vector<MaterialGpu> materials;
  std::vector<gpu::Buffer> vertices, indices;
  std::vector<Resource> pool;
  Resource target;
  std::map<std::string, Resource> imports;
  std::optional<float> environment_scale;
  uint64_t frame = 0;
  uint32_t view_stride = 0;
  std::set<std::string> demanded, ran;
  std::map<std::string, uint64_t> run_counts;
  std::map<std::string, gpu::ShaderModule> shader_modules;
  std::map<std::string, std::string> runtime_sources;
  std::map<std::string, gpu::RenderPipeline> render_pipelines;

  explicit Impl(const std::filesystem::path &assets, Compiler *compiler) : root(assets) {
    const auto manifest = read(root, "manifest.json");
    data = Json::parse(manifest);
    if (data.at("version") != 1)
      throw std::runtime_error("unsupported offline manifest version");
    if (data.at("abi") != WXSL_SHADER_ABI_REVISION)
      throw std::runtime_error("offline manifest shader ABI mismatch");
    if (data.at("host_layout") != WXSL_HOST_LAYOUT_ID)
      throw std::runtime_error("offline manifest host layout mismatch; "
                               "re-export assets/rebuild the header");
    if (compiler) {
      for (const auto &pipeline : data.at("pipelines")) {
        if (!pipeline.contains("request"))
          continue; // Pure device probes have no document.
        compiler->compile(Operation::Check, pipeline.at("check_request"));
        const auto plan = compiler->compile(Operation::Pipeline, pipeline.at("request"));
        if (plan->metadata.at("data").at("graph") != pipeline.at("graph") ||
            plan->metadata.at("data").at("schedule") != pipeline.at("schedule"))
          throw std::runtime_error("runtime pipeline differs from host bundle; re-export assets");
      }
      for (const auto &material : data.at("materials"))
        for (const auto &stage : material.at("stages")) {
          const auto compiled = compiler->compile(Operation::Material, stage.at("request"));
          if (compiled->metadata.at("data").at("signature") != stage.at("signature"))
            throw std::runtime_error("runtime material layout differs from host bundle");
          runtime_sources[stage.at("wgsl")] = compiled->wgsl;
        }
      for (const auto &effect : data.at("effects"))
        runtime_sources[effect.at("wgsl")] =
            compiler->compile(Operation::Shader, effect.at("request"))->wgsl;
    }
    const gpu::InstanceFeatureName feature = gpu::InstanceFeatureName::TimedWaitAny;
    gpu::InstanceDescriptor descriptor;
    descriptor.requiredFeatureCount = 1;
    descriptor.requiredFeatures = &feature;
    instance = gpu::CreateInstance(&descriptor);
    if (!instance)
      throw std::runtime_error("Dawn instance creation failed");
    gpu::RequestAdapterOptions options;
    struct AdapterReply {
      gpu::Adapter value;
      std::string error;
    };
    auto adapter_reply = std::make_shared<AdapterReply>();
    auto future =
        instance.RequestAdapter(&options, gpu::CallbackMode::WaitAnyOnly,
                                [adapter_reply](gpu::RequestAdapterStatus status,
                                                gpu::Adapter value, gpu::StringView message) {
                                  if (status != gpu::RequestAdapterStatus::Success)
                                    adapter_reply->error = text(message);
                                  else
                                    adapter_reply->value = std::move(value);
                                });
    wait(future);
    adapter = std::move(adapter_reply->value);
    if (!adapter)
      throw std::runtime_error("no Dawn adapter: " + adapter_reply->error);
    gpu::AdapterInfo info;
    adapter.GetInfo(&info);
    gpu::Limits adapter_limits;
    adapter.GetLimits(&adapter_limits);
    std::vector<gpu::FeatureName> enabled;
    for (const auto f : {gpu::FeatureName::Float32Filterable, gpu::FeatureName::Float32Blendable,
                         gpu::FeatureName::TimestampQuery, gpu::FeatureName::Depth32FloatStencil8,
                         gpu::FeatureName::IndirectFirstInstance})
      if (adapter.HasFeature(f))
        enabled.push_back(f);
    gpu::DeviceDescriptor request;
    request.requiredFeatureCount = enabled.size();
    request.requiredFeatures = enabled.data();
    request.SetUncapturedErrorCallback(
        [](const gpu::Device &, gpu::ErrorType, gpu::StringView message, ErrorState *errors) {
          std::lock_guard<std::mutex> lock(errors->mutex);
          errors->message = text(message);
        },
        &errors);
    struct DeviceReply {
      gpu::Device value;
      std::string error;
    };
    auto device_reply = std::make_shared<DeviceReply>();
    future = adapter.RequestDevice(&request, gpu::CallbackMode::WaitAnyOnly,
                                   [device_reply](gpu::RequestDeviceStatus status,
                                                  gpu::Device value, gpu::StringView message) {
                                     if (status != gpu::RequestDeviceStatus::Success)
                                       device_reply->error = text(message);
                                     else
                                       device_reply->value = std::move(value);
                                   });
    wait(future);
    device = std::move(device_reply->value);
    if (!device)
      throw std::runtime_error("Dawn device creation failed: " + device_reply->error);
    device.GetLimits(&limits);
    queue = device.GetQueue();
    caps = {{"adapter", text(info.device)},
            {"float32_blendable", bool(device.HasFeature(gpu::FeatureName::Float32Blendable))},
            {"timestamp_query", bool(device.HasFeature(gpu::FeatureName::TimestampQuery))},
            {"max_color_attachments", limits.maxColorAttachments},
            {"max_color_attachment_bytes_per_sample", limits.maxColorAttachmentBytesPerSample},
            {"uniform_offset_alignment", limits.minUniformBufferOffsetAlignment}};
    if (width() > limits.maxTextureDimension2D || height() > limits.maxTextureDimension2D)
      throw std::runtime_error("target exceeds adapter maxTextureDimension2D");
    init();
    check();
  }
  ~Impl() {
    if (device)
      device.Destroy();
  }
  void wait(gpu::Future future) {
    if (instance.WaitAny(future, 10'000'000'000ull) != gpu::WaitStatus::Success)
      throw std::runtime_error("Dawn async operation failed or timed out");
  }
  void check() {
    std::lock_guard<std::mutex> lock(errors.mutex);
    if (!errors.message.empty()) {
      auto message = std::move(errors.message);
      errors.message.clear();
      throw std::runtime_error("Dawn: " + message);
    }
  }
  uint32_t width() const { return data.at("width"); }
  uint32_t height() const { return data.at("height"); }
  gpu::Buffer buffer(const std::vector<uint8_t> &bytes, gpu::BufferUsage usage,
                     size_t minimum = 4) {
    gpu::BufferDescriptor descriptor;
    descriptor.size = std::max(minimum, bytes.size());
    descriptor.usage = usage | gpu::BufferUsage::CopyDst;
    auto result = device.CreateBuffer(&descriptor);
    if (!bytes.empty())
      queue.WriteBuffer(result, 0, bytes.data(), bytes.size());
    return result;
  }
  gpu::BindGroupLayout layout(const std::vector<gpu::BindGroupLayoutEntry> &entries) {
    gpu::BindGroupLayoutDescriptor descriptor;
    descriptor.entryCount = entries.size();
    descriptor.entries = entries.data();
    return device.CreateBindGroupLayout(&descriptor);
  }
  gpu::BindGroup group(gpu::BindGroupLayout layout,
                       const std::vector<gpu::BindGroupEntry> &entries) {
    gpu::BindGroupDescriptor descriptor;
    descriptor.layout = layout;
    descriptor.entryCount = entries.size();
    descriptor.entries = entries.data();
    return device.CreateBindGroup(&descriptor);
  }
  gpu::ShaderModule shader(const std::string &name) {
    if (const auto found = shader_modules.find(name); found != shader_modules.end())
      return found->second;
    std::string code;
    if (const auto found = runtime_sources.find(name); found != runtime_sources.end())
      code = found->second;
    else {
      auto bytes = read(root, name);
      code.assign(bytes.begin(), bytes.end());
    }
    auto module = shader_source(name, code);
    shader_modules.emplace(name, module);
    return module;
  }
  gpu::ShaderModule shader_source(const std::string &name, const std::string &code) {
    gpu::ShaderSourceWGSL source;
    source.code = {code.data(), code.size()};
    gpu::ShaderModuleDescriptor descriptor;
    descriptor.nextInChain = &source;
    descriptor.label = {name.data(), name.size()};
    auto module = device.CreateShaderModule(&descriptor);
    return module;
  }
  void compile_material(Compiler &compiler, size_t index, const Json &request) {
    auto &row = data.at("materials").at(index);
    const auto &original = row.at("stages").begin().value().at("request");
    if (request.value("config", Json::object()) != original.at("config") ||
        request.value("material", Json::object()).value("cast_shadow", true) !=
            original.at("material").value("cast_shadow", true) ||
        request.value("material", Json::object()).value("tags", Json::array()) !=
            original.at("material").value("tags", Json::array()))
      throw std::runtime_error("runtime edit changes setup/draw selection; re-export host bundle");
    MaterialGpu replacement = materials.at(index);
    Json stages = row.at("stages");
    std::vector<uint8_t> params;
    bool changed = false;
    // All Rust compilation/layout validation finishes before creating GPU
    // objects.
    std::map<std::string, std::shared_ptr<const Compiled>> outputs;
    for (auto stage = stages.begin(); stage != stages.end(); ++stage) {
      auto input = request;
      input["stage"] = stage.key();
      auto output = compiler.compile(Operation::Material, input);
      if (output->metadata.at("data").at("signature") != stage.value().at("signature"))
        throw std::runtime_error("runtime material layout changed; re-export host bundle");
      // Source-library overlays are part of the request, not the graph's key.
      changed |= stage.value().at("request") != input;
      stage.value()["request"] = input;
      stage.value()["fragment_entry"] = output->metadata.at("data").at("fragment_entry");
      changed |= stage.value().at("variant_key") != output->metadata.at("data").at("variant_key");
      stage.value()["variant_key"] = output->metadata.at("data").at("variant_key");
      params = output->params;
      outputs.emplace(stage.key(), std::move(output));
    }
    if (changed)
      for (const auto &[stage, output] : outputs)
        replacement.shaders[stage] = shader_source("runtime " + stage, output->wgsl);
    check();
    // Validate pipelines too, before changing any persistent parameter bytes.
    if (changed) {
      auto previous_material = materials.at(index);
      auto previous_stages = row.at("stages");
      auto previous_pipelines = std::move(render_pipelines);
      auto previous_plan = current;
      materials.at(index) = std::move(replacement);
      row["stages"] = stages;
      try {
        for (const auto &plan : data.at("pipelines")) {
          current = plan;
          for (size_t p = 0; p < plan.at("graph").at("passes").size(); ++p) {
            const auto &pass = plan.at("graph").at("passes").at(p);
            if (!pass.at("kind").contains("geometry"))
              continue;
            std::vector<gpu::BindGroupLayoutEntry> entries;
            const auto &bindings = plan.at("pass_data").at(p).at("bindings");
            for (size_t b = 0; b < bindings.size(); ++b)
              entries.push_back(layout_entry(static_cast<uint32_t>(b), bindings.at(b)));
            render_pipeline(pass, layout(entries), index);
            check();
          }
        }
      } catch (...) {
        current = std::move(previous_plan);
        materials.at(index) = std::move(previous_material);
        row["stages"] = std::move(previous_stages);
        render_pipelines = std::move(previous_pipelines);
        throw;
      }
      current = std::move(previous_plan);
    } else {
      row["stages"] = std::move(stages);
    }
    // Existing bind groups retain their layout; default changes are an upload.
    upload_material_params(index, params);
  }
  void upload_material_params(size_t index, const std::vector<uint8_t> &bytes) {
    const auto size =
        data.at("materials").at(index).at("interface").at("params").at("size").get<size_t>();
    if (bytes.size() != size)
      throw std::runtime_error("material parameter upload size mismatch");
    if (!bytes.empty())
      queue.WriteBuffer(materials.at(index).params, 0, bytes.data(), bytes.size());
  }
  Resource resource(const Json &shape) {
    Resource result;
    result.shape = shape;
    if (shape.contains("texture")) {
      const auto &row = shape.at("texture");
      gpu::TextureDescriptor descriptor;
      descriptor.size = {row.at("size")[0], row.at("size")[1], row.at("size")[2]};
      descriptor.format = format(row.at("format"));
      descriptor.dimension =
          row.at("dimension") == "d3" ? gpu::TextureDimension::e3D : gpu::TextureDimension::e2D;
      descriptor.usage = texture_usage(row.at("usage"));
      descriptor.mipLevelCount = row.value("mip_levels", 1u);
      result.texture = device.CreateTexture(&descriptor);
      gpu::TextureViewDescriptor view;
      view.dimension = dimension(row.at("dimension"));
      result.view = result.texture.CreateView(&view);
    } else {
      const auto &row = shape.at("buffer");
      gpu::BufferDescriptor descriptor;
      descriptor.size = row.at("size");
      descriptor.usage = buffer_usage(row.at("usage"));
      result.buffer = device.CreateBuffer(&descriptor);
    }
    return result;
  }
  float upload_environment_image(const std::string &label, uint32_t w, uint32_t h,
                                 const std::vector<float> &rgb) {
    if (w == 0 || h == 0 || w > limits.maxTextureDimension2D || h > limits.maxTextureDimension2D ||
        uint64_t(w) * h * 3 != rgb.size())
      throw std::runtime_error("HDR dimensions do not match pixels or device limits");
    bool declared = false;
    for (const auto &pipeline : data.at("pipelines"))
      for (const auto &desc : pipeline.at("graph").at("resources"))
        if (desc.at("label") == label && desc.at("imported") == true &&
            desc.at("shape").contains("texture") &&
            desc.at("shape").at("texture").at("format") == "rgba32float")
          declared = true;
    if (!declared)
      throw std::runtime_error("no rgba32float HDR import declared: " + label);
    float peak = 0.0f;
    for (float v : rgb) {
      if (!std::isfinite(v) || v < 0.0f)
        throw std::runtime_error("HDR radiance must be finite and nonnegative");
      peak = std::max(peak, v);
    }
    const float scale = std::max(peak / 16384.0f, 1.0f);
    std::vector<float> rgba(size_t(w) * h * 4, 1.0f);
    for (size_t i = 0; i < rgb.size() / 3; ++i)
      for (size_t c = 0; c < 3; ++c)
        rgba[i * 4 + c] = rgb[i * 3 + c] / scale;
    auto value = resource({{"texture", {{"format", "rgba32float"}, {"dimension", "d2"},
        {"usage", "TEXTURE_BINDING | COPY_DST"}, {"size", {w, h, 1}}}}});
    gpu::TexelCopyTextureInfo to;
    to.texture = value.texture;
    gpu::TexelCopyBufferLayout layout;
    layout.bytesPerRow = w * 16;
    layout.rowsPerImage = h;
    const gpu::Extent3D extent{w, h, 1};
    queue.WriteTexture(&to, rgba.data(), rgba.size() * sizeof(float), &layout, &extent);
    check();
    imports.insert_or_assign(label, std::move(value));
    ran.clear();
    return scale;
  }
  void set_environment_scale(float scale) {
    if (!std::isfinite(scale) || scale <= 0.0f)
      throw std::runtime_error("environment scale must be finite and positive");
    environment_scale = scale;
    if (!current.is_null() && !current.empty()) {
      const auto bytes = read(root, data.at("frame").at("scene"));
      WxslSceneUniform uniform;
      std::memcpy(&uniform, bytes.data(), sizeof(uniform));
      uniform.environment_enabled = current.at("graph").value("environment_maps", Json(nullptr)).is_null() ? 0.0f : 1.0f;
      uniform.environment_scale = scale;
      queue.WriteBuffer(scene, 0, &uniform, sizeof(uniform));
    }
  }
  void init() {
    empty_layout = layout({});
    std::vector<gpu::BindGroupLayoutEntry> entries;
    for (const auto binding : {WXSL_BINDING_CAMERA, WXSL_BINDING_SCENE, WXSL_BINDING_INSTANCES,
                               WXSL_BINDING_INSTANCE_ATTRIBUTES, WXSL_BINDING_PREVIOUS_INSTANCES}) {
      const bool uniform = binding == WXSL_BINDING_CAMERA || binding == WXSL_BINDING_SCENE;
      auto entry = layout_entry(
          binding, {{"kind", uniform ? "uniform" : "buffer"}, {"read_only", true}, {"size", 0}});
      entry.visibility = gpu::ShaderStage::Vertex | gpu::ShaderStage::Fragment;
      entry.buffer.hasDynamicOffset = binding == WXSL_BINDING_CAMERA;
      if (binding == WXSL_BINDING_CAMERA)
        entry.buffer.minBindingSize = sizeof(WxslCameraUniform);
      entries.push_back(entry);
    }
    auto entry =
        layout_entry(WXSL_BINDING_SHADOW_MAPS,
                     {{"kind", "texture"}, {"sample_type", "depth"}, {"dimension", "d2_array"}});
    entry.visibility = gpu::ShaderStage::Vertex | gpu::ShaderStage::Fragment;
    entries.push_back(entry);
    entry = {};
    entry.binding = WXSL_BINDING_SHADOW_SAMPLER;
    entry.visibility = gpu::ShaderStage::Vertex | gpu::ShaderStage::Fragment;
    entry.sampler.type = gpu::SamplerBindingType::Comparison;
    entries.push_back(entry);
    entry = layout_entry(WXSL_BINDING_ENVIRONMENT_LUT,
                         {{"kind", "texture"}, {"sample_type", "float"}, {"dimension", "d2"}});
    entry.visibility = gpu::ShaderStage::Vertex | gpu::ShaderStage::Fragment;
    entries.push_back(entry);
    entry = {};
    entry.binding = WXSL_BINDING_ENVIRONMENT_SAMPLER;
    entry.visibility = gpu::ShaderStage::Vertex | gpu::ShaderStage::Fragment;
    entry.sampler.type = gpu::SamplerBindingType::Filtering;
    entries.push_back(entry);
    for (const auto binding : {WXSL_BINDING_ENVIRONMENT_DIFFUSE, WXSL_BINDING_ENVIRONMENT_SPECULAR}) {
      entry = layout_entry(binding, {{"kind", "texture"}, {"sample_type", "float"}, {"dimension", "cube"}});
      entry.visibility = gpu::ShaderStage::Fragment;
      entries.push_back(entry);
    }
    frame_layout = layout(entries);
    const auto views = read(root, data.at("frame").at("views"));
    if (views.size() % sizeof(WxslCameraUniform) != 0)
      throw std::runtime_error("camera upload stride mismatch");
    view_stride = (sizeof(WxslCameraUniform) + limits.minUniformBufferOffsetAlignment - 1) /
                  limits.minUniformBufferOffsetAlignment * limits.minUniformBufferOffsetAlignment;
    std::vector<uint8_t> aligned(view_stride * (views.size() / sizeof(WxslCameraUniform)));
    for (size_t i = 0; i < views.size() / sizeof(WxslCameraUniform); ++i)
      std::memcpy(aligned.data() + i * view_stride, views.data() + i * sizeof(WxslCameraUniform),
                  sizeof(WxslCameraUniform));
    camera = buffer(aligned, gpu::BufferUsage::Uniform);
    const auto scene_bytes = read(root, data.at("frame").at("scene"));
    if (scene_bytes.size() != sizeof(WxslSceneUniform))
      throw std::runtime_error("scene upload layout mismatch");
    scene = buffer(scene_bytes, gpu::BufferUsage::Uniform);
    transforms = buffer(read(root, data.at("frame").at("instances")), gpu::BufferUsage::Storage,
                        sizeof(WxslInstanceTransform));
    empty_attributes = buffer({}, gpu::BufferUsage::Storage);
    Resource value = resource({{"texture",
                                {{"format", "rgba8unorm"},
                                 {"dimension", "d2"},
                                 {"usage", "COPY_DST | TEXTURE_BINDING"},
                                 {"size", {64, 64, 1}}}}});
    checker = value.texture;
    checker_view = value.view;
    gpu::TexelCopyTextureInfo image;
    image.texture = checker;
    image.aspect = gpu::TextureAspect::All;
    gpu::TexelCopyBufferLayout upload;
    upload.bytesPerRow = 256;
    upload.rowsPerImage = 64;
    const auto pixels = read(root, data.at("checker"));
    const gpu::Extent3D extent{64, 64, 1};
    queue.WriteTexture(&image, pixels.data(), pixels.size(), &upload, &extent);
    gpu::SamplerDescriptor sampler;
    sampler.addressModeU = gpu::AddressMode::Repeat;
    sampler.addressModeV = gpu::AddressMode::Repeat;
    sampler.minFilter = sampler.magFilter = gpu::FilterMode::Linear;
    checker_sampler = device.CreateSampler(&sampler);
    sampler.addressModeU = sampler.addressModeV = gpu::AddressMode::ClampToEdge;
    sampler.mipmapFilter = gpu::MipmapFilterMode::Linear;
    lut_sampler = device.CreateSampler(&sampler);
    sampler.compare = gpu::CompareFunction::Less;
    shadow_sampler = device.CreateSampler(&sampler);
    value = resource({{"texture",
                       {{"format", "depth32float"},
                        {"dimension", "d2_array"},
                        {"usage", "RENDER_ATTACHMENT | TEXTURE_BINDING"},
                        {"size", {1, 1, data.at("frame").at("light_count")}}}}});
    fallback = value.texture;
    fallback_view = value.view;
    const auto lut_size = data.at("frame").at("lut_size");
    value = resource({{"texture",
                       {{"format", "rgba16float"},
                        {"dimension", "d2"},
                        {"usage", "STORAGE_BINDING | TEXTURE_BINDING"},
                        {"size", {lut_size, lut_size, 1}}}}});
    lut = value.texture;
    lut_view = value.view;
    value = resource({{"texture", {{"format", "rgba16float"}, {"dimension", "cube"},
                        {"usage", "TEXTURE_BINDING"}, {"size", {1, 1, 6}}}}});
    environment_placeholder = value.view;
    for (const auto &row : data.at("meshes")) {
      vertices.push_back(buffer(read(root, row.at("vertices")), gpu::BufferUsage::Vertex));
      indices.push_back(buffer(read(root, row.at("indices")), gpu::BufferUsage::Index));
    }
    for (const auto &row : data.at("materials")) {
      MaterialGpu material;
      std::vector<gpu::BindGroupLayoutEntry> descriptors;
      std::vector<gpu::BindGroupEntry> bindings;
      const auto &interface = row.at("interface");
      auto params = read(root, row.at("params"));
      if (!params.empty()) {
        material.params = buffer(params, gpu::BufferUsage::Uniform);
        descriptors.push_back(layout_entry(0, {{"kind", "uniform"}, {"size", params.size()}}));
        gpu::BindGroupEntry binding;
        binding.binding = 0;
        binding.buffer = material.params;
        binding.size = params.size();
        bindings.push_back(binding);
      }
      for (const auto &declared : interface.at("resources")) {
        gpu::BindGroupLayoutEntry descriptor;
        descriptor.binding = declared.at("binding");
        descriptor.visibility = gpu::ShaderStage::Vertex | gpu::ShaderStage::Fragment;
        gpu::BindGroupEntry binding;
        binding.binding = descriptor.binding;
        if (declared.at("ty") == "sampler") {
          descriptor.sampler.type = gpu::SamplerBindingType::Filtering;
          binding.sampler = checker_sampler;
        } else if (declared.at("ty") == "texture2d") {
          descriptor.texture.sampleType = gpu::TextureSampleType::Float;
          descriptor.texture.viewDimension = gpu::TextureViewDimension::e2D;
          binding.textureView = checker_view;
        } else
          throw std::runtime_error("unsupported demo material resource `" +
                                   declared.at("name").get<std::string>() + "`");
        descriptors.push_back(descriptor);
        bindings.push_back(binding);
      }
      material.layout = layout(descriptors);
      material.group = group(material.layout, bindings);
      material.attributes = buffer(read(root, row.at("attributes")), gpu::BufferUsage::Storage);
      for (auto stage = row.at("stages").begin(); stage != row.at("stages").end(); ++stage)
        material.shaders[stage.key()] = shader(stage.value().at("wgsl"));
      materials.push_back(std::move(material));
    }
    auto encoder = device.CreateCommandEncoder();
    for (uint32_t i = 0; i < data.at("frame").at("light_count"); ++i) {
      gpu::TextureViewDescriptor view;
      view.dimension = gpu::TextureViewDimension::e2D;
      view.baseArrayLayer = i;
      view.arrayLayerCount = 1;
      gpu::RenderPassDepthStencilAttachment depth;
      depth.view = fallback.CreateView(&view);
      depth.depthLoadOp = gpu::LoadOp::Clear;
      depth.depthStoreOp = gpu::StoreOp::Store;
      depth.depthClearValue = 1.0f;
      gpu::RenderPassDescriptor pass;
      pass.depthStencilAttachment = &depth;
      auto clear = encoder.BeginRenderPass(&pass);
      clear.End();
    }
    const auto &bake = data.at("effects").at("wxsl.brdf_lut");
    auto bake_layout = layout({layout_entry(
        0, {{"kind", "storage_texture"}, {"format", "rgba16float"}, {"dimension", "d2"}})});
    gpu::BindGroupEntry binding;
    binding.binding = 0;
    binding.textureView = lut_view;
    auto bake_group = group(bake_layout, {binding});
    std::array<gpu::BindGroupLayout, WXSL_GROUP_PASS + 1> groups;
    groups.fill(empty_layout);
    groups[WXSL_GROUP_PASS] = bake_layout;
    gpu::PipelineLayoutDescriptor pl;
    pl.bindGroupLayoutCount = groups.size();
    pl.bindGroupLayouts = groups.data();
    const auto bake_entry = bake.at("kind").at("compute").at("entry").get<std::string>();
    gpu::ComputePipelineDescriptor cp;
    cp.layout = device.CreatePipelineLayout(&pl);
    cp.compute.module = shader(bake.at("wgsl"));
    cp.compute.entryPoint = {bake_entry.data(), bake_entry.size()};
    auto pipeline = device.CreateComputePipeline(&cp);
    auto compute = encoder.BeginComputePass();
    compute.SetPipeline(pipeline);
    compute.SetBindGroup(WXSL_GROUP_PASS, bake_group);
    const auto counts = bake.at("kind").at("compute").at("workgroups");
    compute.DispatchWorkgroups(counts[0], counts[1], counts[2]);
    compute.End();
    auto command = encoder.Finish();
    queue.Submit(1, &command);
  }
  Resource &resolve(uint32_t resource, uint32_t history = 0) {
    const auto &allocation = current.at("schedule").at("allocations").at(resource);
    if (allocation == "imported") {
      if (resource != 0) {
        const auto &desc = current.at("graph").at("resources").at(resource);
        const auto label = desc.at("label").get<std::string>();
        const auto found = imports.find(label);
        if (found == imports.end())
          throw std::runtime_error("missing imported resource " + label);
        const auto &shape = desc.at("shape").at("texture");
        if (shape.at("format") != "rgba32float" || shape.at("dimension") != "d2" ||
            shape.value("mip_levels", 1) != 1 || shape.at("layers") != 1)
          throw std::runtime_error("HDR import shape mismatch: " + label);
        return found->second;
      }
      return target;
    }
    const auto base = allocation.at("ring").at("base").get<size_t>(),
               length = allocation.at("ring").at("length").get<size_t>();
    return pool.at(base + (frame % length + length - history % length) % length);
  }
  gpu::TextureView attachment(uint32_t resource, uint32_t layer, uint32_t mip) {
    auto &value = resolve(resource);
    if (value.shape.at("texture").at("dimension") == "d3")
      throw std::runtime_error("3D render attachments require a depth-slice descriptor");
    gpu::TextureViewDescriptor view;
    view.dimension = gpu::TextureViewDimension::e2D;
    view.baseArrayLayer = layer;
    view.arrayLayerCount = 1;
    view.baseMipLevel = mip;
    view.mipLevelCount = 1;
    return value.texture.CreateView(&view);
  }
  gpu::BindGroup frame_group(size_t material, bool detached) {
    auto shadow = fallback_view;
    if (!detached && !current.at("graph").at("shadow_maps").is_null())
      shadow = resolve(current.at("graph").at("shadow_maps")).view;
    std::vector<gpu::BindGroupEntry> entries;
    for (const auto &[index, value] : std::array<std::pair<uint32_t, gpu::Buffer>, 5>{
             {{WXSL_BINDING_CAMERA, camera},
              {WXSL_BINDING_SCENE, scene},
              {WXSL_BINDING_INSTANCES, transforms},
              {WXSL_BINDING_PREVIOUS_INSTANCES, transforms},
              {WXSL_BINDING_INSTANCE_ATTRIBUTES,
               material < materials.size() ? materials[material].attributes : empty_attributes}}}) {
      gpu::BindGroupEntry entry;
      entry.binding = index;
      entry.buffer = value;
      entry.size = index == WXSL_BINDING_CAMERA ? sizeof(WxslCameraUniform) : value.GetSize();
      entries.push_back(entry);
    }
    gpu::BindGroupEntry entry;
    entry.binding = WXSL_BINDING_SHADOW_MAPS;
    entry.textureView = shadow;
    entries.push_back(entry);
    entry = {};
    entry.binding = WXSL_BINDING_SHADOW_SAMPLER;
    entry.sampler = shadow_sampler;
    entries.push_back(entry);
    entry = {};
    entry.binding = WXSL_BINDING_ENVIRONMENT_LUT;
    entry.textureView = lut_view;
    entries.push_back(entry);
    entry = {};
    entry.binding = WXSL_BINDING_ENVIRONMENT_SAMPLER;
    entry.sampler = lut_sampler;
    entries.push_back(entry);
    const auto maps = current.at("graph").value("environment_maps", Json(nullptr));
    for (size_t index = 0; index < 2; ++index) {
      entry = {};
      entry.binding = index == 0 ? WXSL_BINDING_ENVIRONMENT_DIFFUSE : WXSL_BINDING_ENVIRONMENT_SPECULAR;
      entry.textureView = !detached && !maps.is_null() ? resolve(maps.at(index)).view : environment_placeholder;
      entries.push_back(entry);
    }
    return group(frame_layout, entries);
  }
  gpu::RenderPipeline render_pipeline(const Json &pass, gpu::BindGroupLayout pass_layout,
                                      size_t material) {
    const auto key = current.at("graph").dump() + pass.dump() + std::to_string(material);
    if (const auto found = render_pipelines.find(key); found != render_pipelines.end())
      return found->second;
    const bool geometry = pass.at("kind").contains("geometry");
    std::array<gpu::BindGroupLayout, WXSL_GROUP_PASS + 1> groups;
    groups.fill(empty_layout);
    groups[WXSL_GROUP_FRAME] = frame_layout;
    groups[WXSL_GROUP_MATERIAL] = geometry ? materials.at(material).layout : empty_layout;
    groups[WXSL_GROUP_PASS] = pass_layout;
    gpu::PipelineLayoutDescriptor pl;
    pl.bindGroupLayoutCount = groups.size();
    pl.bindGroupLayouts = groups.data();
    const auto label = pass.at("label").get<std::string>();
    gpu::RenderPipelineDescriptor descriptor;
    descriptor.layout = device.CreatePipelineLayout(&pl);
    descriptor.label = {label.data(), label.size()};
    std::string vertex_entry, fragment_entry;
    gpu::ShaderModule module;
    if (geometry) {
      const auto stage = pass.at("kind").at("geometry").at("stage").get<std::string>();
      module = materials.at(material).shaders.at(stage);
      vertex_entry = "vs_main";
      const auto &entry =
          data.at("materials").at(material).at("stages").at(stage).at("fragment_entry");
      if (!entry.is_null())
        fragment_entry = entry.get<std::string>();
    } else {
      const auto &effect =
          data.at("effects").at(pass.at("kind").at("screen").at("effect").get<std::string>());
      module = shader(effect.at("wgsl"));
      vertex_entry = effect.at("kind").at("screen").at("vertex_entry");
      fragment_entry = effect.at("kind").at("screen").at("fragment_entry");
    }
    descriptor.vertex.module = module;
    descriptor.vertex.entryPoint = {vertex_entry.data(), vertex_entry.size()};
    std::vector<gpu::VertexAttribute> attributes;
    gpu::VertexBufferLayout vertex;
    if (geometry) {
      const auto &row = data.at("vertex_layout");
      for (const auto &item : row.at("attributes")) {
        gpu::VertexAttribute attribute;
        attribute.format = vertex_format(item.at("format"));
        attribute.offset = item.at("offset");
        attribute.shaderLocation = item.at("location");
        attributes.push_back(attribute);
      }
      vertex.arrayStride = row.at("stride");
      vertex.attributeCount = attributes.size();
      vertex.attributes = attributes.data();
      vertex.stepMode = gpu::VertexStepMode::Vertex;
      descriptor.vertex.bufferCount = 1;
      descriptor.vertex.buffers = &vertex;
    }
    std::vector<gpu::ColorTargetState> targets;
    std::vector<gpu::BlendState> blends(pass.at("color").size());
    for (size_t i = 0; i < pass.at("color").size(); ++i) {
      const auto &color = pass.at("color").at(i);
      const auto id = color.at("resource").get<uint32_t>();
      gpu::ColorTargetState target;
      target.format =
          format(current.at("graph").at("resources").at(id).at("shape").at("texture").at("format"));
      const auto &blend =
          color.at("blend").is_null() ? pass.at("state").at("blend") : color.at("blend");
      if (!blend.is_null()) {
        blends[i].color = component(blend.at("color"));
        blends[i].alpha = component(blend.at("alpha"));
        target.blend = &blends[i];
      }
      targets.push_back(target);
    }
    if (targets.size() > limits.maxColorAttachments)
      throw std::runtime_error("pass exceeds maxColorAttachments: " +
                               pass.at("label").get<std::string>());
    gpu::FragmentState fragment;
    if (!fragment_entry.empty()) {
      fragment.module = module;
      fragment.entryPoint = {fragment_entry.data(), fragment_entry.size()};
      fragment.targetCount = targets.size();
      fragment.targets = targets.data();
      descriptor.fragment = &fragment;
    }
    gpu::DepthStencilState depth;
    if (!pass.at("state").at("depth_format").is_null()) {
      depth.format = format(pass.at("state").at("depth_format"));
      depth.depthCompare = compare(pass.at("state").at("depth_compare"));
      depth.depthWriteEnabled = pass.at("state").at("depth_write").get<bool>();
      descriptor.depthStencil = &depth;
    }
    descriptor.primitive.topology = gpu::PrimitiveTopology::TriangleList;
    descriptor.primitive.frontFace = gpu::FrontFace::CCW;
    const auto &cull = pass.at("state").at("cull_mode");
    descriptor.primitive.cullMode =
        cull.is_null() ? gpu::CullMode::None
                       : named<gpu::CullMode>(cull, {{"back", gpu::CullMode::Back},
                                                     {"front", gpu::CullMode::Front}});
    auto pipeline = device.CreateRenderPipeline(&descriptor);
    render_pipelines.emplace(key, pipeline);
    return pipeline;
  }
  std::vector<uint8_t> render(const std::string &name, uint32_t count) {
    if (count == 0)
      throw std::runtime_error("frame count must be nonzero");
    const auto &next = data.at("pipelines").at(name);
    const auto &requirements = next.at("requirements");
    if (requirements.at("max_color_attachments").get<uint32_t>() > limits.maxColorAttachments)
      throw std::runtime_error(name + " requires more than maxColorAttachments");
    if (requirements.at("max_color_attachment_bytes_per_sample").get<uint32_t>() >
        limits.maxColorAttachmentBytesPerSample)
      throw std::runtime_error(name + " exceeds maxColorAttachmentBytesPerSample");
    if (requirements.at("float32_blendable").get<bool>() &&
        !device.HasFeature(gpu::FeatureName::Float32Blendable))
      throw std::runtime_error(name + " requires float32_blendable");
    if (current != next) {
      current = next;
      const auto scene_bytes = read(root, data.at("frame").at("scene"));
      WxslSceneUniform uniform;
      std::memcpy(&uniform, scene_bytes.data(), sizeof(uniform));
      uniform.environment_enabled = current.at("graph").value("environment_maps", Json(nullptr)).is_null() ? 0.0f : 1.0f;
      uniform.environment_scale = environment_scale.value_or(current.at("graph").value("environment_scale", 1.0f));
      queue.WriteBuffer(scene, 0, &uniform, sizeof(uniform));
      pool.clear();
      ran.clear();
      run_counts.clear();
      frame = 0;
      for (const auto &shape : current.at("physical_slots"))
        pool.push_back(resource(shape));
      target = resource({{"texture",
                          {{"format", "rgba8unorm"},
                           {"dimension", "d2"},
                           {"usage", "COPY_SRC | RENDER_ATTACHMENT"},
                           {"size", {width(), height(), 1}}}}});
    }
    for (uint32_t n = 0; n < count; ++n) {
      auto encoder = device.CreateCommandEncoder();
      for (const auto &index : current.at("schedule").at("order")) {
        const auto i = index.get<size_t>();
        const auto &pass = current.at("graph").at("passes").at(i);
        const auto &extra = current.at("pass_data").at(i);
        const auto label = pass.at("label").get<std::string>(),
                   policy = pass.at("policy").get<std::string>();
        if ((policy == "once" || policy == "on_resize") && ran.contains(label))
          continue;
        if (policy == "on_demand" && !demanded.contains(label))
          continue;
        if (policy != "per_frame" && policy != "once" && policy != "on_resize" &&
            policy != "on_demand")
          throw std::runtime_error("unsupported policy " + policy);
        std::vector<gpu::BindGroupLayoutEntry> layouts;
        std::vector<gpu::BindGroupEntry> entries;
        for (size_t binding = 0; binding < extra.at("bindings").size(); ++binding)
          layouts.push_back(
              layout_entry(static_cast<uint32_t>(binding), extra.at("bindings").at(binding)));
        for (const auto &read : pass.at("reads")) {
          auto &value = resolve(read.at("resource"), read.at("history"));
          gpu::BindGroupEntry entry;
          entry.binding = static_cast<uint32_t>(entries.size());
          if (value.buffer) {
            entry.buffer = value.buffer;
            entry.size = value.buffer.GetSize();
          } else
            entry.textureView = value.view;
          entries.push_back(entry);
        }
        for (const auto &write : pass.at("writes")) {
          auto &value = resolve(write);
          gpu::BindGroupEntry entry;
          entry.binding = static_cast<uint32_t>(entries.size());
          if (value.buffer) {
            entry.buffer = value.buffer;
            entry.size = value.buffer.GetSize();
          } else
            entry.textureView = value.view;
          entries.push_back(entry);
        }
        std::string effect_id;
        if (pass.at("kind").contains("screen"))
          effect_id = pass.at("kind").at("screen").at("effect");
        if (pass.at("kind").contains("compute"))
          effect_id = pass.at("kind").at("compute").at("effect");
        gpu::Buffer parameters;
        if (!effect_id.empty()) {
          const auto &effect = data.at("effects").at(effect_id);
          if (effect.at("param_size").get<uint32_t>() != 0) {
            const auto bytes = read(root, extra.contains("params") && !extra.at("params").is_null()
                                              ? extra.at("params") : effect.at("params"));
            parameters = buffer(bytes, gpu::BufferUsage::Uniform);
            gpu::BindGroupEntry entry;
            entry.binding = static_cast<uint32_t>(entries.size());
            entry.buffer = parameters;
            entry.size = bytes.size();
            entries.push_back(entry);
          }
        }
        auto pass_layout = layout(layouts);
        auto pass_group = group(pass_layout, entries);
        if (pass.at("kind").contains("compute")) {
          const auto &effect = data.at("effects").at(effect_id);
          std::array<gpu::BindGroupLayout, WXSL_GROUP_PASS + 1> groups;
          groups.fill(empty_layout);
          groups[WXSL_GROUP_PASS] = pass_layout;
          gpu::PipelineLayoutDescriptor pl;
          pl.bindGroupLayoutCount = groups.size();
          pl.bindGroupLayouts = groups.data();
          const auto entry = effect.at("kind").at("compute").at("entry").get<std::string>();
          gpu::ComputePipelineDescriptor descriptor;
          descriptor.layout = device.CreatePipelineLayout(&pl);
          descriptor.compute.module = shader(effect.at("wgsl"));
          descriptor.compute.entryPoint = {entry.data(), entry.size()};
          auto compute = encoder.BeginComputePass();
          compute.SetPipeline(device.CreateComputePipeline(&descriptor));
          compute.SetBindGroup(WXSL_GROUP_PASS, pass_group);
          const auto counts = effect.at("kind").at("compute").at("workgroups");
          compute.DispatchWorkgroups(counts[0], counts[1], counts[2]);
          compute.End();
        } else {
          std::vector<gpu::RenderPassColorAttachment> colors;
          for (const auto &color : pass.at("color")) {
            gpu::RenderPassColorAttachment entry;
            entry.view = attachment(color.at("resource"), color.at("layer"), color.value("mip", 0u));
            entry.storeOp =
                color.at("store").get<bool>() ? gpu::StoreOp::Store : gpu::StoreOp::Discard;
            const auto &load = color.at("load");
            entry.loadOp = load.is_string() ? gpu::LoadOp::Load : gpu::LoadOp::Clear;
            if (!load.is_string()) {
              const auto &clear = load.at("clear");
              entry.clearValue = {clear.at("r"), clear.at("g"), clear.at("b"), clear.at("a")};
            }
            colors.push_back(entry);
          }
          gpu::RenderPassDepthStencilAttachment depth;
          gpu::RenderPassDescriptor descriptor;
          descriptor.label = {label.data(), label.size()};
          descriptor.colorAttachmentCount = colors.size();
          descriptor.colorAttachments = colors.data();
          bool detached = false;
          if (!pass.at("depth").is_null()) {
            const auto &row = pass.at("depth");
            depth.view = attachment(row.at("resource"), row.at("layer"), row.value("mip", 0u));
            depth.depthLoadOp = row.at("clear").is_null() ? gpu::LoadOp::Load : gpu::LoadOp::Clear;
            if (!row.at("clear").is_null())
              depth.depthClearValue = row.at("clear");
            depth.depthStoreOp =
                row.at("store").get<bool>() ? gpu::StoreOp::Store : gpu::StoreOp::Discard;
            descriptor.depthStencilAttachment = &depth;
            detached = row.at("resource") == current.at("graph").at("shadow_maps");
          }
          const auto maps = current.at("graph").value("environment_maps", Json(nullptr));
          if (!maps.is_null())
            for (const auto &color : pass.at("color"))
              for (const auto &map : maps)
                detached = detached || color.at("resource") == map;
          auto render = encoder.BeginRenderPass(&descriptor);
          const auto offset = extra.at("view_slot").get<uint32_t>() * view_stride;
          if (pass.at("kind").contains("geometry")) {
            const auto &source = pass.at("kind").at("geometry").at("source");
            for (const auto &draw_index : extra.at("draws")) {
              const auto draw = draw_index.get<uint32_t>();
              const auto &item = data.at("draws").at(draw);
              const auto material = item.at("material").get<size_t>(),
                         mesh = item.at("mesh").get<size_t>();
              render.SetPipeline(render_pipeline(pass, pass_layout, material));
              render.SetBindGroup(WXSL_GROUP_FRAME, frame_group(material, detached), 1, &offset);
              render.SetBindGroup(WXSL_GROUP_MATERIAL, materials.at(material).group);
              render.SetBindGroup(WXSL_GROUP_PASS, pass_group);
              render.SetVertexBuffer(0, vertices.at(mesh));
              render.SetIndexBuffer(indices.at(mesh), gpu::IndexFormat::Uint32);
              if (source.contains("indirect")) {
                const auto &args = source.at("indirect");
                auto &arguments = resolve(args.at("buffer"));
                if (!arguments.buffer)
                  throw std::runtime_error("indirect arguments are not a buffer: " + label);
                for (uint64_t record = 0; record < args.at("count").get<uint32_t>(); ++record)
                  render.DrawIndexedIndirect(arguments.buffer,
                                             args.at("offset").get<uint64_t>() + record * 20);
              } else if (source.contains("scene")) {
                render.DrawIndexed(data.at("meshes").at(mesh).at("index_count"), 1, 0, 0, draw);
              } else
                throw std::runtime_error("unknown draw source: " + label);
            }
          } else {
            render.SetPipeline(render_pipeline(pass, pass_layout, 0));
            render.SetBindGroup(WXSL_GROUP_FRAME, frame_group(materials.size(), detached), 1, &offset);
            render.SetBindGroup(WXSL_GROUP_PASS, pass_group);
            render.Draw(3);
          }
          render.End();
        }
        ran.insert(label);
        demanded.erase(label);
        ++run_counts[label];
      }
      auto command = encoder.Finish();
      queue.Submit(1, &command);
      ++frame;
      check();
    }
    const auto pitch = (width() * 4 + 255) / 256 * 256;
    gpu::BufferDescriptor descriptor;
    descriptor.size = uint64_t(pitch) * height();
    descriptor.usage = gpu::BufferUsage::MapRead | gpu::BufferUsage::CopyDst;
    auto result = device.CreateBuffer(&descriptor);
    auto encoder = device.CreateCommandEncoder();
    gpu::TexelCopyTextureInfo from;
    from.texture = target.texture;
    from.aspect = gpu::TextureAspect::All;
    gpu::TexelCopyBufferInfo to;
    to.buffer = result;
    to.layout.bytesPerRow = pitch;
    to.layout.rowsPerImage = height();
    const gpu::Extent3D extent{width(), height(), 1};
    encoder.CopyTextureToBuffer(&from, &to, &extent);
    auto command = encoder.Finish();
    queue.Submit(1, &command);
    auto mapped = std::make_shared<bool>(false);
    auto future =
        result.MapAsync(gpu::MapMode::Read, 0, descriptor.size, gpu::CallbackMode::WaitAnyOnly,
                        [mapped](gpu::MapAsyncStatus status, gpu::StringView) {
                          *mapped = status == gpu::MapAsyncStatus::Success;
                        });
    wait(future);
    check();
    if (!*mapped)
      throw std::runtime_error("Dawn readback mapping failed");
    const auto *bytes = static_cast<const uint8_t *>(result.GetConstMappedRange());
    std::vector<uint8_t> pixels(size_t(width()) * height() * 4);
    for (uint32_t y = 0; y < height(); ++y)
      std::memcpy(pixels.data() + size_t(y) * width() * 4, bytes + size_t(y) * pitch, width() * 4);
    result.Unmap();
    return pixels;
  }
};

Renderer::Renderer(const std::filesystem::path &assets, Compiler *compiler)
    : impl_(std::make_unique<Impl>(assets, compiler)) {}
Renderer::~Renderer() = default;
std::vector<uint8_t> Renderer::render(const std::string &pipeline, uint32_t frames) {
  return impl_->render(pipeline, frames);
}
void Renderer::mark_pass(const std::string &label) { impl_->demanded.insert(label); }
uint64_t Renderer::pass_run_count(const std::string &label) const {
  const auto found = impl_->run_counts.find(label);
  return found == impl_->run_counts.end() ? 0 : found->second;
}
nlohmann::json Renderer::capabilities() const { return impl_->caps; }
void Renderer::compile_material(Compiler &compiler, size_t index, const nlohmann::json &request) {
  impl_->compile_material(compiler, index, request);
}
void Renderer::upload_material_params(size_t index, const std::vector<uint8_t> &bytes) {
  impl_->upload_material_params(index, bytes);
}
float Renderer::upload_environment_image(const std::string &label, uint32_t width, uint32_t height,
                                        const std::vector<float> &rgb) {
  return impl_->upload_environment_image(label, width, height, rgb);
}
void Renderer::set_environment_scale(float scale) { impl_->set_environment_scale(scale); }
size_t Renderer::pipeline_count() const { return impl_->render_pipelines.size(); }
uint32_t Renderer::width() const { return impl_->width(); }
uint32_t Renderer::height() const { return impl_->height(); }
} // namespace wxsl
