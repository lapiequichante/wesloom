#pragma once
#include <cstdint>
#include <filesystem>
#include <memory>
#include <nlohmann/json.hpp>
#include <string>
#include <vector>

namespace wxsl {
class Compiler;
// Device half only: all shader, selection, layout and schedule rules arrive as
// data.
class Renderer {
public:
  explicit Renderer(const std::filesystem::path &assets, Compiler *compiler = nullptr);
  ~Renderer();
  Renderer(const Renderer &) = delete;
  Renderer &operator=(const Renderer &) = delete;
  std::vector<uint8_t> render(const std::string &pipeline, uint32_t frames = 1);
  void mark_pass(const std::string &label);
  uint64_t pass_run_count(const std::string &label) const;
  nlohmann::json capabilities() const;
  // Same-layout edits only; failure retains the previous valid material.
  void compile_material(Compiler &compiler, size_t index, const nlohmann::json &request);
  void upload_material_params(size_t index, const std::vector<uint8_t> &bytes);
  size_t pipeline_count() const;
  uint32_t width() const;
  uint32_t height() const;

private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
} // namespace wxsl
