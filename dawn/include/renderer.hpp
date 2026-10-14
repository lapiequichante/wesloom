#pragma once
#include <cstdint>
#include <filesystem>
#include <memory>
#include <nlohmann/json.hpp>
#include <string>
#include <vector>

namespace wxsl {
class Compiler;
/// Headless Dawn renderer for an exported bundle (ADR 0051).
///
/// Device half only: shaders, draw selection, layouts and schedules arrive as
/// data produced by `wxsl-ffi`'s `bake_docs` exporter, or by a Compiler at
/// runtime. Every method throws `std::runtime_error` on failure.
class Renderer {
public:
  /// Create a device and load the bundle in `assets`. With a `compiler`, the
  /// scene and setup are checked and every shader is recompiled at load
  /// instead of read from the bundle's WGSL files.
  explicit Renderer(const std::filesystem::path &assets, Compiler *compiler = nullptr);
  ~Renderer();
  Renderer(const Renderer &) = delete;
  Renderer &operator=(const Renderer &) = delete;
  /// Render `frames` frames through the bundle's pipeline named `pipeline`
  /// and return the last one as tightly packed RGBA8 rows (`width() * 4`
  /// bytes each). Switching pipeline resets its pools, history and run counts.
  std::vector<uint8_t> render(const std::string &pipeline, uint32_t frames = 1);
  /// Run the `on demand` pass with this label on the next frame.
  void mark_pass(const std::string &label);
  /// How many times the pass with this label has run since the pipeline was
  /// selected.
  uint64_t pass_run_count(const std::string &label) const;
  /// The adapter's name, optional features and the limits the bundle's
  /// requirements are checked against.
  nlohmann::json capabilities() const;
  /// Recompile material `index` from a material request (see docs/cpp.md).
  /// Only edits that keep the material's layout are accepted. On failure the
  /// previous material keeps rendering.
  void compile_material(Compiler &compiler, size_t index, const nlohmann::json &request);
  /// Replace material `index`'s parameter bytes. They must have the computed,
  /// padded size of its parameter layout. Nothing is recompiled.
  void upload_material_params(size_t index, const std::vector<uint8_t> &bytes);
  /// Upload application-decoded linear RGB to the imported resource named
  /// `label`. Returns the radiance restoration scale to pass to
  /// set_environment_scale(), like wgpu's `EnvironmentImage`. Invalidates
  /// `once` bakes.
  float upload_environment_image(const std::string &label, uint32_t width, uint32_t height,
                                 const std::vector<float> &rgb);
  /// Upload linear RGBA spotlight cookie layers: RGB tints the light, alpha
  /// scales its intensity.
  void upload_light_cookies(uint32_t width, uint32_t height, uint32_t layers,
                            const std::vector<uint8_t> &rgba);
  /// Set the scale upload_environment_image() returned.
  void set_environment_scale(float scale);
  /// Number of render pipelines created so far.
  size_t pipeline_count() const;
  /// Target width in pixels, from the bundle.
  uint32_t width() const;
  /// Target height in pixels, from the bundle.
  uint32_t height() const;

private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
} // namespace wxsl
