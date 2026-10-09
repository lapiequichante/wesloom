#pragma once
#include <cstdint>
#include <filesystem>
#include <memory>
#include <nlohmann/json.hpp>
#include <string>
#include <vector>

namespace wxsl {
// Device half only: all shader, selection, layout and schedule rules arrive as
// data.
class Renderer {
public:
  explicit Renderer(const std::filesystem::path &assets);
  ~Renderer();
  Renderer(const Renderer &) = delete;
  Renderer &operator=(const Renderer &) = delete;
  std::vector<uint8_t> render(const std::string &pipeline, uint32_t frames = 1);
  void mark_pass(const std::string &label);
  uint64_t pass_run_count(const std::string &label) const;
  nlohmann::json capabilities() const;
  uint32_t width() const;
  uint32_t height() const;

private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
} // namespace wxsl
