#pragma once
#include <cstdint>
#include <filesystem>
#include <memory>
#include <nlohmann/json.hpp>
#include <string>
#include <vector>

namespace wxsl {
enum class Operation { Pipeline, Shader, Material, Check };
struct Compiled {
  nlohmann::json metadata, fields;
  std::string wgsl;
  std::vector<uint8_t> params;
};
// Single-owner cache; independent Compiler instances may be used on workers.
class Compiler {
public:
  explicit Compiler(const std::filesystem::path &library);
  ~Compiler();
  Compiler(const Compiler &) = delete;
  Compiler &operator=(const Compiler &) = delete;
  std::shared_ptr<const Compiled> compile(Operation operation, const nlohmann::json &request);
  uint64_t compilations() const;
  uint64_t cache_hits() const;

private:
  struct Impl;
  std::unique_ptr<Impl> impl_;
};
} // namespace wxsl
